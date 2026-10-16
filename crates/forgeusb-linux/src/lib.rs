//! Shared read-only planner and raw-image operation engine for CLI and GUI.
use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, capacity_check, progress};
use forgeusb_device::Device;
use forgeusb_image::{FileStamp, ImageInfo, inspect_cancel, open_source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "device", rename_all = "snake_case")]
pub enum TargetKind {
    Block(Box<Device>),
    TestFile,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub image: ImageInfo,
    pub target: PathBuf,
    pub capacity: u64,
    pub confirmation: String,
    pub warnings: Vec<String>,
    pub target_kind: TargetKind,
    pub strategy: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub bytes: u64,
    pub sha256: String,
    pub verified: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRequest {
    pub plan: Plan,
    pub confirmation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HelperEvent {
    Progress(Progress),
    Complete(Report),
    Error(String),
}
pub fn plan(source: &Path, target: &Path, test_file: bool) -> Result<Plan> {
    plan_cancel(source, target, test_file, &Cancellation::default())
}
pub fn plan_cancel(
    source: &Path,
    target: &Path,
    test_file: bool,
    cancel: &Cancellation,
) -> Result<Plan> {
    ensure!(
        source.is_absolute() && target.is_absolute(),
        "source and target must be absolute paths"
    );
    let image = inspect_cancel(source, cancel)?;
    plan_image(image, target, test_file)
}
fn plan_image(image: ImageInfo, target: &Path, test_file: bool) -> Result<Plan> {
    ensure!(
        image.path.is_absolute() && target.is_absolute(),
        "source and target must be absolute paths"
    );
    let strategy = forgeusb_boot::select_strategy(
        &forgeusb_boot::ImageCapabilities {
            disk_layout: image.bootable_disk,
            iso9660: image.iso9660,
            windows_installer: image.windows,
            compressed: image.kind == forgeusb_image::ImageKind::Compressed,
            ..Default::default()
        },
        forgeusb_boot::Firmware::PreserveImage,
    )?;
    ensure!(strategy.executable, "{}", strategy.explanation);
    let mut warnings = image.warnings.clone();
    warnings.push(strategy.explanation.clone());
    warnings.push("Writing destroys existing target data. Cancellation can leave incomplete media. There is no rollback.".into());
    warnings.push("Verification covers every source byte; device space beyond the source is preserved and is not verified. Firmware boot compatibility is inherited from the image and requires a boot test.".into());
    let (target_kind, capacity, confirmation) = if test_file {
        ensure!(
            !target.starts_with("/dev")
                && !target.starts_with("/proc")
                && !target.starts_with("/sys"),
            "test target must be a new ordinary file outside special filesystems"
        );
        ensure!(target.file_name().is_some(), "test target needs a filename");
        let parent = target.parent().context("target has no parent")?;
        let canonical = fs::canonicalize(parent)?;
        ensure!(
            canonical == parent,
            "test target parent must be canonical, without symlinks or .."
        );
        ensure!(
            fs::symlink_metadata(target).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "test target already exists; choose a new disposable file"
        );
        warnings.push("Disposable-file mode: creates a new file exclusively and never opens an existing target for writing.".into());
        (
            TargetKind::TestFile,
            image.size,
            format!("CREATE {} {} bytes", target.display(), image.size),
        )
    } else {
        let device = forgeusb_device::discover()?
            .into_iter()
            .find(|d| d.path == target)
            .context("target is not a discovered whole block device")?;
        ensure!(
            device.eligible,
            "target refused: {}",
            device.reasons.join("; ")
        );
        capacity_check(image.size, device.capacity)?;
        if unsafe { libc::geteuid() } != 0 {
            warnings.push("Device eligibility is preliminary in the desktop process. The privileged helper must complete system-wide namespace, mount and identity checks before any physical write; it may reject this plan.".into());
        }
        let confirmation = format!(
            "ERASE {} {} {} bytes",
            device.path.display(),
            device.model,
            device.capacity
        );
        (
            TargetKind::Block(Box::new(device.clone())),
            device.capacity,
            confirmation,
        )
    };
    Ok(Plan {
        image,
        target: target.to_path_buf(),
        capacity,
        confirmation,
        warnings,
        target_kind,
        strategy: strategy.id,
    })
}
fn source_matches(f: &File, info: &ImageInfo) -> Result<()> {
    ensure!(
        FileStamp::of(f)? == info.stamp,
        "source identity or metadata changed since planning"
    );
    Ok(())
}
pub fn execute(
    p: &Plan,
    confirmation: &str,
    cancel: &Cancellation,
    emit: impl FnMut(Progress),
) -> Result<Report> {
    let source = open_source(&p.image.path)?;
    execute_opened(p, confirmation, source, cancel, emit)
}
/// Run using a retained source descriptor opened with the caller's credentials.
/// No source path is reopened after authorization.
pub fn execute_opened(
    p: &Plan,
    confirmation: &str,
    mut source: File,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<Report> {
    cancel.check()?;
    source_matches(&source, &p.image)?;
    ensure!(
        confirmation == p.confirmation,
        "confirmation must exactly match the approved plan"
    );
    emit(progress(
        Stage::Revalidating,
        0,
        p.image.size,
        "Rechecking source and device identity before opening target",
    ));
    // Rebuild policy decisions from current facts rather than trusting serialized plan fields.
    let inspected = forgeusb_image::inspect_opened(&p.image.path, &mut source, cancel)?;
    let fresh = plan_image(
        inspected,
        &p.target,
        matches!(p.target_kind, TargetKind::TestFile),
    )?;
    ensure!(
        fresh.image == p.image
            && fresh.confirmation == p.confirmation
            && fresh.capacity == p.capacity
            && fresh.strategy == p.strategy,
        "plan is stale or modified; inspect and confirm again"
    );
    match (&fresh.target_kind, &p.target_kind) {
        (TargetKind::Block(a), TargetKind::Block(b)) => {
            ensure!(a.identity == b.identity, "device identity changed")
        }
        (TargetKind::TestFile, TargetKind::TestFile) => {}
        _ => bail!("target kind changed"),
    }
    source_matches(&source, &p.image)?;
    source.seek(SeekFrom::Start(0))?;
    cancel.check()?;
    let mut target = match &p.target_kind {
        TargetKind::TestFile => OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&p.target)
            .context("create new disposable target")?,
        TargetKind::Block(d) => forgeusb_device::open_exclusive(d, true)?,
    };
    // Hold both file descriptors and the device lock until verification completes.
    let result = (|| -> Result<Report> {
        capacity_check(p.image.size, p.capacity)?;
        source_matches(&source, &p.image)?;
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut at = 0u64;
        let mut hash = Sha256::new();
        while at < p.image.size {
            cancel.check()?;
            let n = ((p.image.size - at).min(buffer.len() as u64)) as usize;
            source
                .read_exact(&mut buffer[..n])
                .context("source truncated during write")?;
            target
                .write_all(&buffer[..n])
                .with_context(|| format!("write target at offset {at}"))?;
            hash.update(&buffer[..n]);
            at += n as u64;
            emit(progress(
                Stage::Writing,
                at,
                p.image.size,
                "Writing raw image",
            ));
        }
        ensure!(
            format!("{:x}", hash.finalize()) == p.image.sha256,
            "source content changed during write; target is incomplete"
        );
        source_matches(&source, &p.image)?;
        emit(progress(
            Stage::Flushing,
            at,
            p.image.size,
            "Synchronizing target data",
        ));
        target
            .sync_all()
            .context("flush failed; target is not verified")?;
        // Evict clean pages when possible. This is a kernel read-back, not proof of controller power-loss persistence.
        let advised = unsafe {
            libc::posix_fadvise(
                target.as_raw_fd(),
                0,
                p.image
                    .size
                    .try_into()
                    .context("image too large for off_t")?,
                libc::POSIX_FADV_DONTNEED,
            )
        };
        if advised != 0 {
            emit(progress(
                Stage::Flushing,
                at,
                p.image.size,
                format!(
                    "Cache eviction unavailable (errno {advised}); verifying through kernel I/O"
                ),
            ));
        }
        let v = forgeusb_verify::compare(
            &mut source,
            &mut target,
            p.image.size,
            &p.image.sha256,
            cancel,
            &mut emit,
        )?;
        source_matches(&source, &p.image)?;
        if let TargetKind::Block(d) = &p.target_kind {
            forgeusb_device::revalidate(d)?;
        }
        emit(progress(
            Stage::Complete,
            v.bytes,
            p.image.size,
            "Write and full source-length read-back verified",
        ));
        Ok(Report {
            bytes: v.bytes,
            sha256: v.sha256,
            verified: true,
        })
    })();
    if let Err(e) = result {
        let flush = target.sync_all();
        if let Err(flush) = flush {
            bail!("{e:#}; partial-target flush also failed: {flush}");
        }
        return Err(e.context("operation failed; selected target may contain partial data and must not be treated as bootable"));
    }
    result
}
pub fn verify(
    source_path: &Path,
    target_path: &Path,
    test_file: bool,
    cancel: &Cancellation,
    emit: impl FnMut(Progress),
) -> Result<Report> {
    let info = inspect_cancel(source_path, cancel)?;
    let mut source = open_source(source_path)?;
    source_matches(&source, &info)?;
    let mut target = if test_file {
        let f = open_source(target_path)?;
        ensure!(
            f.metadata()?.dev() != info.stamp.device || f.metadata()?.ino() != info.stamp.inode,
            "source and target are the same file"
        );
        f
    } else {
        let d = forgeusb_device::discover()?
            .into_iter()
            .find(|d| d.path == target_path)
            .context("not a discovered block device")?;
        forgeusb_device::open_exclusive(&d, false)?
    };
    let size = target.metadata()?.len();
    if test_file {
        capacity_check(info.size, size)?;
    }
    let v = forgeusb_verify::compare(
        &mut source,
        &mut target,
        info.size,
        &info.sha256,
        cancel,
        emit,
    )?;
    source_matches(&source, &info)?;
    Ok(Report {
        bytes: v.bytes,
        sha256: v.sha256,
        verified: true,
    })
}
/// Check tools by fixed absolute paths. No shell or PATH resolution.
pub fn dependencies() -> serde_json::Value {
    let tools = [
        ("lsblk", "/usr/bin/lsblk"),
        ("udisksctl", "/usr/bin/udisksctl"),
        ("pkexec", "/usr/bin/pkexec"),
        ("helper", "/usr/lib/forgeusb/forgeusb-helper"),
        ("sfdisk", "/usr/bin/sfdisk"),
        ("mkfs.fat", "/usr/bin/mkfs.fat"),
        ("mkfs.ntfs", "/usr/bin/mkfs.ntfs"),
        ("wimlib-imagex", "/usr/bin/wimlib-imagex"),
        ("xorriso", "/usr/bin/xorriso"),
        ("qemu", "/usr/bin/qemu-system-x86_64"),
    ];
    serde_json::json!(tools.into_iter().map(|(name,path)|serde_json::json!({"name":name,"path":path,"installed":Path::new(path).is_file()})).collect::<Vec<_>>())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(dir: &Path) -> PathBuf {
        let p = dir.join("source.img");
        let mut bytes = vec![0u8; 2 * 1024 * 1024];
        bytes[510] = 0x55;
        bytes[511] = 0xaa;
        bytes[450] = 0x83;
        bytes[454..458].copy_from_slice(&1u32.to_le_bytes());
        bytes[458..462].copy_from_slice(&4095u32.to_le_bytes());
        bytes[1000] = 42;
        fs::write(&p, bytes).unwrap();
        p
    }
    #[test]
    fn full_write_readback_and_no_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let s = fixture(d.path());
        let t = d.path().join("new.img");
        let p = plan(&s, &t, true).unwrap();
        assert!(!t.exists());
        let r = execute(&p, &p.confirmation, &Cancellation::default(), |_| {}).unwrap();
        assert!(r.verified);
        assert_eq!(fs::read(&s).unwrap(), fs::read(&t).unwrap());
        assert!(execute(&p, &p.confirmation, &Cancellation::default(), |_| {}).is_err());
    }
    #[test]
    fn confirmation_cancel_and_stale_source() {
        let d = tempfile::tempdir().unwrap();
        let s = fixture(d.path());
        let t = d.path().join("new.img");
        let p = plan(&s, &t, true).unwrap();
        assert!(execute(&p, "wrong", &Cancellation::default(), |_| {}).is_err());
        assert!(!t.exists());
        let c = Cancellation::default();
        c.cancel();
        assert!(execute(&p, &p.confirmation, &c, |_| {}).is_err());
        assert!(!t.exists());
        let mut f = OpenOptions::new().write(true).open(&s).unwrap();
        f.seek(SeekFrom::Start(1000)).unwrap();
        f.write_all(&[43]).unwrap();
        assert!(execute(&p, &p.confirmation, &Cancellation::default(), |_| {}).is_err());
        assert!(!t.exists());
    }
    #[test]
    fn cancellation_mid_write_leaves_explicit_partial() {
        let d = tempfile::tempdir().unwrap();
        let s = fixture(d.path());
        let t = d.path().join("new.img");
        let p = plan(&s, &t, true).unwrap();
        let c = Cancellation::default();
        let r = execute(&p, &p.confirmation, &c, |event| {
            if event.stage == Stage::Writing {
                c.cancel();
            }
        });
        assert!(r.is_err());
        assert!(t.metadata().unwrap().len() < p.image.size);
    }
}
