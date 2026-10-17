//! Unprivileged Windows-media staging. No mounts, formatting, or device writes.
use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::Cancellation;
use forgeusb_filesystem::{FAT32_MAX_FILE_BYTES, validate_fat_name};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const WIMLIB: &str = "/usr/bin/wimlib-imagex";

/// Discover a trusted executable through the caller's PATH without assuming a distro path.
pub fn wimlib_path() -> Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !dir.is_absolute() {
            continue;
        }
        let path = dir.join("wimlib-imagex");
        if let Ok(meta) = fs::metadata(&path)
            && meta.is_file()
            && meta.mode() & 0o111 != 0
            && meta.mode() & 0o022 == 0
        {
            return Ok(path);
        }
    }
    bail!("wimlib-imagex is unavailable; install wimlib to split large install.wim files")
}
const MAX_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeReport {
    pub root: PathBuf,
    pub files: usize,
    pub total_bytes: u64,
    pub efi_architectures: Vec<String>,
    pub install_image: PathBuf,
    pub oversized_files: Vec<PathBuf>,
    pub needs_wim_split: bool,
    pub coverage: String,
}

fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "expected regular file: {}",
        path.display()
    );
    Ok(file)
}

fn is_wim(path: &Path) -> Result<bool> {
    let mut f = open_regular(path)?;
    let mut h = [0u8; 8];
    Ok(f.read_exact(&mut h).is_ok() && &h == b"MSWIM\0\0\0")
}

fn pe_machine_file(f: &mut File) -> Result<u16> {
    let mut dos = [0u8; 64];
    f.read_exact(&mut dos).context("truncated EFI DOS header")?;
    ensure!(&dos[..2] == b"MZ", "EFI file lacks DOS signature");
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    ensure!(
        (64..=1024 * 1024).contains(&offset)
            && offset
                .checked_add(6)
                .is_some_and(|n| n <= f.metadata().map(|m| m.len()).unwrap_or(0)),
        "EFI PE offset out of bounds"
    );
    f.seek(SeekFrom::Start(offset))?;
    let mut pe = [0u8; 6];
    f.read_exact(&mut pe)?;
    ensure!(&pe[..4] == b"PE\0\0", "EFI file lacks PE signature");
    Ok(u16::from_le_bytes([pe[4], pe[5]]))
}

fn open_under(root: &File, relative: &Path) -> Result<File> {
    let mut dir = root.try_clone()?;
    let parts = relative.components().collect::<Vec<_>>();
    for (index, component) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            bail!("unsafe relative tree component")
        };
        let name = CString::new(name.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if index + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        ensure!(
            fd >= 0,
            "cannot safely open tree entry: {}",
            std::io::Error::last_os_error()
        );
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}
fn split_header(part: &mut File) -> Result<(u16, u16, [u8; 16])> {
    let mut header = [0u8; 208];
    part.read_exact(&mut header)
        .context("truncated split WIM header")?;
    ensure!(
        &header[..8] == b"MSWIM\0\0\0"
            && u32::from_le_bytes(header[8..12].try_into().unwrap()) >= 208,
        "invalid split WIM header"
    );
    let number = u16::from_le_bytes(header[40..42].try_into().unwrap());
    let count = u16::from_le_bytes(header[42..44].try_into().unwrap());
    ensure!(
        number > 0 && count > 0 && number <= count && count <= 1024,
        "invalid split WIM part count"
    );
    Ok((number, count, header[24..40].try_into().unwrap()))
}
/// Inspect a quiescent caller-owned tree through retained directory descriptors.
/// WIM/PE headers and SWM-set completeness are structural checks. The media
/// builder additionally runs wimlib resource verification before publication.
pub fn inspect_extracted_tree(root: &Path) -> Result<TreeReport> {
    let root_handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let root = root.canonicalize()?;
    let mut stack = vec![(root_handle.try_clone()?, PathBuf::new(), 0usize)];
    let mut files = BTreeMap::<String, (PathBuf, u64)>::new();
    let mut names = BTreeSet::new();
    let mut upper_names = BTreeSet::new();
    let mut entries = 0usize;
    let mut path_bytes = 0usize;
    let mut total = 0u64;
    while let Some((dir, relative, depth)) = stack.pop() {
        ensure!(depth <= 64, "tree exceeds directory depth limit");
        let anchored = PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            dir.as_raw_fd()
        ));
        for entry in fs::read_dir(anchored)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= MAX_ENTRIES, "tree exceeds entry limit");
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF8 filename")?;
            validate_fat_name(name)?;
            let path = relative.join(name);
            let text = path.to_str().context("non-UTF8 path")?;
            ensure!(text.len() <= 4096, "tree path exceeds limit");
            path_bytes = path_bytes
                .checked_add(text.len())
                .context("path storage overflow")?;
            ensure!(
                path_bytes <= 16 * 1024 * 1024,
                "tree exceeds aggregate path budget"
            );
            ensure!(
                names.insert(text.to_lowercase()) && upper_names.insert(text.to_uppercase()),
                "case-insensitive path collision: {text}"
            );
            let opened = open_under(&dir, Path::new(name))?;
            let meta = opened.metadata()?;
            if meta.is_dir() {
                stack.push((opened, path, depth + 1));
            } else {
                ensure!(meta.is_file(), "symlink/special tree entry rejected");
                total = total
                    .checked_add(meta.len())
                    .context("tree byte count overflow")?;
                files.insert(text.to_lowercase(), (path, meta.len()));
            }
        }
    }
    let boot = files
        .get("sources/boot.wim")
        .context("missing sources/boot.wim")?;
    let mut boot_file = open_under(&root_handle, &boot.0)?;
    let mut magic = [0u8; 8];
    ensure!(
        boot_file.read_exact(&mut magic).is_ok() && &magic == b"MSWIM\0\0\0",
        "boot.wim lacks WIM magic"
    );
    let mut architectures = Vec::new();
    for (name, machine, arch) in [
        ("bootx64.efi", 0x8664, "x86_64"),
        ("bootia32.efi", 0x014c, "x86"),
        ("bootaa64.efi", 0xaa64, "aarch64"),
        ("bootarm.efi", 0x01c2, "arm"),
    ] {
        if let Some((path, _)) = files.get(&format!("efi/boot/{name}")) {
            ensure!(
                pe_machine_file(&mut open_under(&root_handle, path)?)? == machine,
                "EFI loader machine does not match {name}"
            );
            architectures.push(arch.to_owned());
        }
    }
    ensure!(
        !architectures.is_empty(),
        "missing supported EFI fallback loader"
    );
    let installs = [
        "sources/install.wim",
        "sources/install.esd",
        "sources/install.swm",
    ]
    .iter()
    .filter_map(|key| files.get(*key))
    .collect::<Vec<_>>();
    ensure!(
        installs.len() == 1,
        "expected exactly one install.wim, install.esd or install.swm"
    );
    let install = installs[0];
    let mut install_file = open_under(&root_handle, &install.0)?;
    ensure!(
        install_file.read_exact(&mut magic).is_ok() && &magic == b"MSWIM\0\0\0",
        "installation image lacks WIM magic"
    );
    if install
        .0
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("swm"))
    {
        let mut numbers = BTreeSet::new();
        let mut guid = None;
        let mut count = None;
        for (key, (path, _)) in &files {
            if key.starts_with("sources/install") && key.ends_with(".swm") {
                let (number, total, id) = split_header(&mut open_under(&root_handle, path)?)?;
                let expected = if number == 1 {
                    "sources/install.swm".into()
                } else {
                    format!("sources/install{number}.swm")
                };
                ensure!(
                    *key == expected,
                    "split WIM filename disagrees with header part index"
                );
                ensure!(numbers.insert(number), "duplicate split WIM part index");
                if let Some(old) = guid {
                    ensure!(old == id, "split WIM GUID mismatch");
                } else {
                    guid = Some(id);
                }
                if let Some(old) = count {
                    ensure!(old == total, "split WIM total count mismatch");
                } else {
                    count = Some(total);
                }
            }
        }
        ensure!(
            Some(numbers.len() as u16) == count
                && (1..=numbers.len() as u16).all(|n| numbers.contains(&n)),
            "incomplete split WIM part set"
        );
    }
    let oversized_files = files
        .values()
        .filter(|(_, n)| *n > FAT32_MAX_FILE_BYTES)
        .map(|(p, _)| p.clone())
        .collect();
    let needs_wim_split = files
        .get("sources/install.wim")
        .is_some_and(|(_, n)| *n > FAT32_MAX_FILE_BYTES);
    Ok(TreeReport{root,files:files.len(),total_bytes:total,efi_architectures:architectures,install_image:install.0.clone(),oversized_files,needs_wim_split,coverage:"Checks safe names, file sizes, WIM magic, EFI fallback PE machine and existing SWM header/part-set completeness through retained directory descriptors. Does not validate WIM resource data, PE signatures, Secure Boot or bootability; the media builder additionally verifies WIM resources with wimlib.".into()})
}

struct WimChild {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for WimChild {
    fn drop(&mut self) {
        if !self.reaped {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            };
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn run_wim_command(
    mut cmd: Command,
    cwd: &Path,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<()> {
    cancel.check()?;
    ensure!(Instant::now() < deadline, "WIM operation timed out");
    cmd.env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    unsafe {
        cmd.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 4 * 1024 * 1024 * 1024,
                rlim_max: 4 * 1024 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = WimChild {
        child: cmd.spawn().context("launching wimlib")?,
        reaped: false,
    };
    let mut stderr = child.child.stderr.take().context("missing wimlib stderr")?;
    let fd = stderr.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "cannot configure bounded wimlib diagnostics"
    );
    let mut errors = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut eof = false;
    loop {
        cancel.check()?;
        ensure!(Instant::now() < deadline, "WIM operation timed out");
        for _ in 0..16 {
            match stderr.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    let remain = 65536usize.saturating_sub(errors.len());
                    errors.extend_from_slice(&buffer[..n.min(remain)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("reading wimlib diagnostics"),
            }
        }
        if eof && let Some(status) = child.child.try_wait()? {
            child.reaped = true;
            ensure!(
                status.success(),
                "wimlib failed ({status}): {}",
                String::from_utf8_lossy(&errors)
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Split a WIM into a newly-created private staging directory. Never run as root.
/// A failed or cancelled operation retains that directory for inspection/removal.
pub fn split_wim(
    source: &Path,
    output: &Path,
    cancel: &Cancellation,
    timeout: Duration,
) -> Result<Vec<PathBuf>> {
    split_wim_with_size(source, output, cancel, timeout, 3800)
}

/// Configurable split size permits bounded small-fixture verification of the same pipeline.
pub fn split_wim_with_size(
    source: &Path,
    output: &Path,
    cancel: &Cancellation,
    timeout: Duration,
    part_mib: u32,
) -> Result<Vec<PathBuf>> {
    ensure!(
        (1..=3800).contains(&part_mib),
        "WIM split size must be 1 through 3800 MiB"
    );
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "WIM staging must run as an ordinary user"
    );
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(7200),
        "timeout must be within 1 ns and 2 hours"
    );
    cancel.check()?;
    ensure!(
        is_wim(source)?,
        "source lacks WIM magic; solid ESD splitting is not supported"
    );
    let retained_source = open_regular(source)?;
    let source = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        retained_source.as_raw_fd()
    ));
    let before = retained_source.metadata()?;
    ensure!(
        before.uid() == unsafe { libc::geteuid() }
            && before.mode() & 0o022 == 0
            && before.len() <= 128 * 1024 * 1024 * 1024,
        "WIM source must be caller-owned and not group/world-writable"
    );
    let output_parent = output
        .parent()
        .context("output requires parent directory")?
        .canonicalize()?;
    let output_name = output
        .file_name()
        .context("output requires a new directory name")?;
    let requested_output = output.to_owned();
    let parent_anchor = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&output_parent)?;
    let anchored_parent = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        parent_anchor.as_raw_fd()
    ));
    let output = anchored_parent.join(output_name);
    let tool = wimlib_path()?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .context("output staging directory must not already exist")?;
    let output_anchor = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&output)?;
    let output = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        output_anchor.as_raw_fd()
    ));
    let after = retained_source.metadata()?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "source changed before WIM split"
    );
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("WIM timeout overflow")?;
    let mut verify = Command::new(&tool);
    verify.arg("verify").arg(&source);
    run_wim_command(verify, &output, cancel, deadline).context("verifying source WIM resources")?;
    let mut split = Command::new(&tool);
    split
        .arg("split")
        .arg(&source)
        .arg(output.join("install.swm"))
        .arg(part_mib.to_string())
        .arg("--check");
    run_wim_command(split, &output, cancel, deadline)
        .context("splitting WIM into FAT32-sized parts")?;
    let after = retained_source.metadata()?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "source changed during WIM split"
    );
    let mut parts = Vec::new();
    let mut headers = BTreeMap::new();
    let mut expected_guid = None;
    let mut expected_count = None;
    for entry in fs::read_dir(&output)? {
        cancel.check()?;
        ensure!(Instant::now() < deadline, "WIM split validation timed out");
        let p = entry?.path();
        ensure!(parts.len() < 1024, "too many WIM parts");
        ensure!(
            p.extension().is_some_and(|e| e == "swm")
                && is_wim(&p)?
                && fs::metadata(&p)?.len() <= FAT32_MAX_FILE_BYTES,
            "invalid or oversized split WIM part: {}",
            p.display()
        );
        let mut part = open_regular(&p)?;
        let (number, count, part_guid) = split_header(&mut part)?;
        let name = if number == 1 {
            "install.swm".into()
        } else {
            format!("install{number}.swm")
        };
        ensure!(
            p.file_name().is_some_and(|n| n == name.as_str()),
            "split WIM part filename does not match header index"
        );
        if let Some(guid) = expected_guid {
            ensure!(guid == part_guid, "split WIM GUID mismatch");
        } else {
            expected_guid = Some(part_guid);
        }
        if let Some(total) = expected_count {
            ensure!(total == count, "split WIM total count mismatch");
        } else {
            expected_count = Some(count);
        }
        ensure!(
            headers.insert(number, p.clone()).is_none(),
            "duplicate split WIM part number"
        );
        part.sync_all()?;
        parts.push(p);
    }
    ensure!(
        !parts.is_empty()
            && Some(parts.len() as u16) == expected_count
            && (1..=parts.len() as u16).all(|n| headers.contains_key(&n)),
        "wimlib produced incomplete split parts"
    );
    let mut verify = Command::new(&tool);
    verify
        .arg("verify")
        .arg(output.join("install.swm"))
        .arg(format!("--ref={}", output.join("install*.swm").display()));
    run_wim_command(verify, &output, cancel, deadline)
        .context("verifying complete split WIM resources")?;
    File::open(&output)?.sync_all()?;
    parts.sort();
    let output_check = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&requested_output)?;
    ensure!(
        output_check.metadata()?.dev() == output_anchor.metadata()?.dev()
            && output_check.metadata()?.ino() == output_anchor.metadata()?.ino(),
        "WIM output directory changed before completion"
    );
    Ok(parts
        .into_iter()
        .map(|p| requested_output.join(p.file_name().unwrap()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_tree(root: &Path) {
        fs::create_dir_all(root.join("sources")).unwrap();
        fs::create_dir_all(root.join("EFI/BOOT")).unwrap();
        fs::write(root.join("sources/boot.wim"), b"MSWIM\0\0\0").unwrap();
        fs::write(root.join("sources/install.wim"), b"MSWIM\0\0\0").unwrap();
        let mut pe = vec![0u8; 134];
        pe[..2].copy_from_slice(b"MZ");
        pe[60..64].copy_from_slice(&128u32.to_le_bytes());
        pe[128..132].copy_from_slice(b"PE\0\0");
        pe[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(root.join("EFI/BOOT/BOOTX64.EFI"), pe).unwrap();
    }
    #[test]
    fn recognizes_case_insensitive_windows_structure() {
        let d = tempfile::tempdir().unwrap();
        make_tree(d.path());
        let r = inspect_extracted_tree(d.path()).unwrap();
        assert_eq!(r.files, 3);
        assert_eq!(r.efi_architectures, vec!["x86_64"]);
        assert!(!r.needs_wim_split);
    }
    #[test]
    fn rejects_symlink_and_collisions() {
        let d = tempfile::tempdir().unwrap();
        make_tree(d.path());
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("escape")).unwrap();
        assert!(inspect_extracted_tree(d.path()).is_err());
        fs::remove_file(d.path().join("escape")).unwrap();
        fs::write(d.path().join("sources/INSTALL.WIM"), b"MSWIM\0\0\0").unwrap();
        assert!(inspect_extracted_tree(d.path()).is_err());
    }
    #[test]
    fn rejects_missing_or_mismatched_split_parts() {
        let d = tempfile::tempdir().unwrap();
        make_tree(d.path());
        fs::remove_file(d.path().join("sources/install.wim")).unwrap();
        let mut header = [0u8; 208];
        header[..8].copy_from_slice(b"MSWIM\0\0\0");
        header[8..12].copy_from_slice(&208u32.to_le_bytes());
        header[40..42].copy_from_slice(&1u16.to_le_bytes());
        header[42..44].copy_from_slice(&2u16.to_le_bytes());
        fs::write(d.path().join("sources/install.swm"), header).unwrap();
        assert!(inspect_extracted_tree(d.path()).is_err());
        header[40..42].copy_from_slice(&2u16.to_le_bytes());
        header[24] = 1;
        fs::write(d.path().join("sources/INSTALL2.SWM"), header).unwrap();
        assert!(inspect_extracted_tree(d.path()).is_err());
        header[24] = 0;
        fs::write(d.path().join("sources/INSTALL2.SWM"), header).unwrap();
        assert!(inspect_extracted_tree(d.path()).is_ok());
    }
    #[test]
    fn rejects_malformed_efi() {
        let d = tempfile::tempdir().unwrap();
        make_tree(d.path());
        fs::write(d.path().join("EFI/BOOT/BOOTX64.EFI"), b"MZ").unwrap();
        assert!(inspect_extracted_tree(d.path()).is_err());
    }
    #[test]
    fn sparse_oversized_install_requires_split() {
        let d = tempfile::tempdir().unwrap();
        make_tree(d.path());
        OpenOptions::new()
            .write(true)
            .open(d.path().join("sources/install.wim"))
            .unwrap()
            .set_len(FAT32_MAX_FILE_BYTES + 1)
            .unwrap();
        let r = inspect_extracted_tree(d.path()).unwrap();
        assert!(r.needs_wim_split);
        assert_eq!(r.oversized_files.len(), 1);
    }
}

#[cfg(test)]
mod real_wim_tests {
    use super::*;
    #[test]
    fn real_wim_split_complete_parts() {
        let Ok(tool) = wimlib_path() else {
            assert!(
                std::env::var_os("FORGEUSB_REQUIRE_MEDIA_TOOLS").is_none(),
                "required real WIM fixture tool wimlib-imagex unavailable"
            );
            eprintln!("real WIM test skipped; wimlib unavailable");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        fs::create_dir(&input).unwrap();
        for index in 0..3 {
            fs::write(
                input.join(format!("payload{index}.bin")),
                vec![index as u8; 768 * 1024],
            )
            .unwrap();
        }
        let wim = d.path().join("install.wim");
        let mut command = Command::new(&tool);
        command
            .arg("capture")
            .arg(&input)
            .arg(&wim)
            .arg("--compress=none");
        run_wim_command(
            command,
            d.path(),
            &Cancellation::default(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        let output = d.path().join("split");
        let parts = split_wim_with_size(
            &wim,
            &output,
            &Cancellation::default(),
            Duration::from_secs(30),
            1,
        )
        .unwrap();
        assert!(parts.len() >= 2);
        let mut command = Command::new(&tool);
        command
            .arg("verify")
            .arg(output.join("install.swm"))
            .arg(format!("--ref={}", output.join("install*.swm").display()));
        run_wim_command(
            command,
            d.path(),
            &Cancellation::default(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        let restored = d.path().join("restored");
        fs::create_dir(&restored).unwrap();
        let mut command = Command::new(&tool);
        command
            .arg("apply")
            .arg(output.join("install.swm"))
            .arg("1")
            .arg(&restored)
            .arg(format!("--ref={}", output.join("install*.swm").display()));
        run_wim_command(
            command,
            d.path(),
            &Cancellation::default(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        for index in 0..3 {
            assert_eq!(
                fs::read(input.join(format!("payload{index}.bin"))).unwrap(),
                fs::read(restored.join(format!("payload{index}.bin"))).unwrap()
            );
        }
    }
}
