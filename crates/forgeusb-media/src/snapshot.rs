use anyhow::{Context, Result, ensure};
use forgeusb_core::Cancellation;
use forgeusb_filesystem::validate_fat_name;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::Instant,
};
pub const MAX_BYTES: u64 = 128 * 1024 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 100_000;
#[derive(Debug, Clone)]
pub struct Entry {
    pub relative: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}
pub fn retained_path(file: &File) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}
pub fn stable(before: &Metadata, after: &Metadata) -> Result<()> {
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "source changed during media preparation"
    );
    Ok(())
}
pub fn open_regular(path: &Path) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        f.metadata()?.is_file(),
        "media source must be an ordinary file"
    );
    Ok(f)
}
pub fn open_directory(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?)
}
pub fn hash(file: &mut File, cancel: &Cancellation, deadline: Instant) -> Result<String> {
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_BYTES,
        "hash input must be a bounded regular file"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut h = Sha256::new();
    let mut b = [0; 1024 * 1024];
    let mut remaining = before.len();
    while remaining > 0 {
        cancel.check()?;
        ensure!(Instant::now() < deadline, "hashing timed out");
        let n = (remaining as usize).min(b.len());
        file.read_exact(&mut b[..n])?;
        h.update(&b[..n]);
        remaining -= n as u64;
    }
    stable(&before, &file.metadata()?)?;
    Ok(format!("{:x}", h.finalize()))
}
pub fn validate_relative(path: &str) -> Result<PathBuf> {
    ensure!(
        !path.is_empty() && path.len() <= 4096 && !path.starts_with('/') && !path.contains('\\'),
        "unsafe archive path"
    );
    let mut depth = 0;
    for component in path.split('/') {
        validate_fat_name(component)?;
        depth += 1;
    }
    ensure!(depth <= 64, "media path exceeds directory depth limit");
    Ok(PathBuf::from(path))
}
pub fn exclusive(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("output must be a new regular file")
}
fn open_at(dir: &File, name: &std::ffi::OsStr, flags: i32) -> Result<File> {
    let n = CString::new(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            n.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "cannot safely open source entry: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(fd) })
}
/// Copy a quiescent tree through retained directory descriptors, never following
/// symlinks. Hashes describe the private snapshot consumed by the formatter.
pub fn snapshot_tree(
    source: &Path,
    dest: &Path,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Vec<Entry>> {
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(source)?;
    let mut stack = vec![(root, PathBuf::new())];
    let mut names = BTreeSet::new();
    let mut upper_names = BTreeSet::new();
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut path_budget = 0usize;
    while let Some((directory, relative)) = stack.pop() {
        let before_dir = directory.metadata()?;
        for entry in fs::read_dir(retained_path(&directory))? {
            cancel.check()?;
            ensure!(Instant::now() < deadline, "tree snapshot timed out");
            let entry = entry?;
            let name = entry.file_name();
            let n = name.to_str().context("non-UTF8 media filename")?;
            validate_fat_name(n)?;
            let path = relative.join(n);
            let text = path.to_str().context("non-UTF8 media path")?;
            validate_relative(text)?;
            ensure!(
                names.len() < MAX_ENTRIES
                    && names.insert(text.to_lowercase())
                    && upper_names.insert(text.to_uppercase()),
                "too many entries or case-insensitive name collision"
            );
            path_budget = path_budget
                .checked_add(text.len())
                .context("path budget overflow")?;
            ensure!(
                path_budget <= 16 * 1024 * 1024,
                "aggregate path budget exceeded"
            );
            let mut opened = open_at(&directory, &name, libc::O_RDONLY)?;
            let before = opened.metadata()?;
            if before.is_dir() {
                fs::create_dir(dest.join(&path))?;
                stack.push((opened, path));
            } else {
                ensure!(before.is_file(), "symlink/special source entry rejected");
                total = total
                    .checked_add(before.len())
                    .context("media size overflow")?;
                ensure!(
                    total <= MAX_BYTES,
                    "media tree exceeds 128 GiB safety budget"
                );
                let mut target = exclusive(&dest.join(&path))?;
                let mut bytes = 0;
                let mut sha = Sha256::new();
                let mut buffer = [0; 1024 * 1024];
                loop {
                    cancel.check()?;
                    ensure!(Instant::now() < deadline, "tree snapshot timed out");
                    let n = opened.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    bytes += n as u64;
                    ensure!(bytes <= before.len(), "source file grew during snapshot");
                    target.write_all(&buffer[..n])?;
                    sha.update(&buffer[..n]);
                }
                ensure!(
                    bytes == before.len(),
                    "source file truncated during snapshot"
                );
                stable(&before, &opened.metadata()?)?;
                entries.push(Entry {
                    relative: path,
                    bytes,
                    sha256: format!("{:x}", sha.finalize()),
                });
            }
        }
        stable(&before_dir, &directory.metadata()?)?;
    }
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(entries)
}
