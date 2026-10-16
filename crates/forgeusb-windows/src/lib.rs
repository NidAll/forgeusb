//! Unprivileged Windows-media staging. No mounts, formatting, or device writes.
use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::Cancellation;
use forgeusb_filesystem::{FAT32_MAX_FILE_BYTES, validate_fat_name};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const WIMLIB: &str = "/usr/bin/wimlib-imagex";
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

fn pe_machine(path: &Path) -> Result<u16> {
    let mut f = open_regular(path)?;
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

/// Inspect a caller-owned extracted tree without following directory symlinks.
/// This is metadata and header validation, not WIM integrity/signature validation.
pub fn inspect_extracted_tree(root: &Path) -> Result<TreeReport> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "tree root must be a directory, not a symlink"
    );
    let root = root.canonicalize()?;
    let mut stack = vec![(root.clone(), 0usize)];
    let mut files = BTreeMap::<String, (PathBuf, u64)>::new();
    let mut names = BTreeMap::<String, PathBuf>::new();
    let mut entries = 0usize;
    let mut path_bytes = 0usize;
    let mut total = 0u64;
    while let Some((dir, depth)) = stack.pop() {
        ensure!(depth <= 64, "tree exceeds directory depth limit");
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= MAX_ENTRIES, "tree exceeds {MAX_ENTRIES} entries");
            let path = entry.path();
            let kind = entry.file_type()?;
            ensure!(
                kind.is_dir() || kind.is_file(),
                "symlink/special file rejected: {}",
                path.display()
            );
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF8 filename")?;
            validate_fat_name(name)?;
            let relative = path
                .strip_prefix(&root)?
                .to_str()
                .context("non-UTF8 path")?
                .to_owned();
            ensure!(relative.len() <= 4096, "tree path exceeds limit");
            path_bytes = path_bytes
                .checked_add(relative.len())
                .context("path storage overflow")?;
            ensure!(
                path_bytes <= 16 * 1024 * 1024,
                "tree exceeds aggregate path budget"
            );
            let key = relative.to_lowercase();
            ensure!(
                names.insert(key.clone(), path.clone()).is_none(),
                "case-insensitive path collision: {relative}"
            );
            if kind.is_dir() {
                stack.push((path, depth + 1));
            } else {
                let meta = fs::symlink_metadata(&path)?;
                ensure!(meta.is_file(), "tree changed while inspecting");
                total = total
                    .checked_add(meta.len())
                    .context("tree byte count overflow")?;
                files.insert(key, (path, meta.len()));
            }
        }
    }
    let boot = files
        .get("sources/boot.wim")
        .context("missing sources/boot.wim")?;
    ensure!(is_wim(&boot.0)?, "boot.wim lacks WIM magic");
    let mut architectures = Vec::new();
    for (name, machine, arch) in [
        ("bootx64.efi", 0x8664, "x86_64"),
        ("bootia32.efi", 0x014c, "x86"),
        ("bootaa64.efi", 0xaa64, "aarch64"),
        ("bootarm.efi", 0x01c2, "arm"),
    ] {
        if let Some((path, _)) = files.get(&format!("efi/boot/{name}")) {
            ensure!(
                pe_machine(path)? == machine,
                "EFI loader machine does not match {name}"
            );
            architectures.push(arch.to_owned());
        }
    }
    ensure!(
        !architectures.is_empty(),
        "missing supported EFI fallback loader"
    );
    let install = files
        .get("sources/install.wim")
        .or_else(|| files.get("sources/install.esd"))
        .or_else(|| files.get("sources/install.swm"))
        .context("missing install.wim, install.esd or install.swm")?;
    ensure!(is_wim(&install.0)?, "installation image lacks WIM magic");
    let oversized = files
        .values()
        .filter(|(_, len)| *len > FAT32_MAX_FILE_BYTES)
        .map(|(p, _)| p.strip_prefix(&root).unwrap().to_owned())
        .collect::<Vec<_>>();
    let needs_wim_split = files
        .get("sources/install.wim")
        .is_some_and(|(_, n)| *n > FAT32_MAX_FILE_BYTES);
    Ok(TreeReport { root:root.clone(), files:files.len(), total_bytes:total, efi_architectures:architectures, install_image:install.0.strip_prefix(&root)?.to_owned(), oversized_files:oversized, needs_wim_split,
        coverage:"Checks names, file sizes, WIM magic, EFI fallback PE machine. Does not validate complete WIM resources, split-part completeness, PE signatures, Secure Boot or bootability. Run only on a quiescent caller-owned staging tree.".into() })
}

/// Split a WIM into a newly-created private staging directory. Never run as root.
/// A failed or cancelled operation retains that directory for inspection/removal.
pub fn split_wim(
    source: &Path,
    output: &Path,
    cancel: &Cancellation,
    timeout: Duration,
) -> Result<Vec<PathBuf>> {
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
    let source = source.canonicalize()?;
    let before = fs::metadata(&source)?;
    ensure!(
        before.uid() == unsafe { libc::geteuid() } && before.mode() & 0o022 == 0,
        "WIM source must be caller-owned and not group/world-writable"
    );
    let output_parent = output
        .parent()
        .context("output requires parent directory")?
        .canonicalize()?;
    let output_name = output
        .file_name()
        .context("output requires a new directory name")?;
    let output = output_parent.join(output_name);
    let tool = fs::metadata(WIMLIB).context("install wimlib: /usr/bin/wimlib-imagex is missing")?;
    ensure!(
        tool.is_file() && tool.uid() == 0 && tool.mode() & 0o022 == 0,
        "wimlib executable must be root-owned and not group/world writable"
    );
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .context("output staging directory must not already exist")?;
    let after = fs::metadata(&source)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec(),
        "source changed before WIM split"
    );
    let mut child = Command::new(WIMLIB)
        .arg("split")
        .arg(&source)
        .arg(output.join("install.swm"))
        .arg("3800")
        .arg("--check")
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin")
        .current_dir(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("launching wimlib split")?;
    let start = Instant::now();
    loop {
        let observed = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("waiting for WIM split process");
            }
        };
        if let Some(status) = observed {
            ensure!(
                status.success(),
                "wimlib split failed ({status}); partial staging remains at {}",
                output.display()
            );
            break;
        }
        if cancel.is_cancelled() || start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "WIM split cancelled or timed out; partial staging remains at {}",
                output.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = fs::metadata(&source)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec(),
        "source changed during WIM split"
    );
    let mut parts = Vec::new();
    for entry in fs::read_dir(&output)? {
        let p = entry?.path();
        ensure!(parts.len() < 1024, "too many WIM parts");
        ensure!(
            p.extension().is_some_and(|e| e == "swm")
                && is_wim(&p)?
                && fs::metadata(&p)?.len() <= FAT32_MAX_FILE_BYTES,
            "invalid or oversized split WIM part: {}",
            p.display()
        );
        File::open(&p)?.sync_all()?;
        parts.push(p);
    }
    ensure!(!parts.is_empty(), "wimlib produced no parts");
    File::open(&output)?.sync_all()?;
    parts.sort();
    Ok(parts)
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
