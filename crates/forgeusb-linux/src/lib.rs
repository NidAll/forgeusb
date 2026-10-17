//! Shared read-only planner and raw-image operation engine for CLI and GUI.
/// Fail closed when a desktop/CLI is paired with an older installed helper.
/// Increment whenever the privileged execution contract or safety policy changes.
pub const HELPER_PROTOCOL_ARGUMENT: &str = "--protocol-v2";

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
    warnings.push("Verification compares every source byte. On a larger physical target, any stale GPT backup header at the device end and its bounded entry array outside the image are cleared and read back separately; other unused space is preserved. Image GPT metadata is preserved, without relocation. Firmware compatibility requires a boot test.".into());
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
        ensure!(
            image
                .size
                .is_multiple_of(device.identity.logical_sector_size as u64),
            "raw image size must be a multiple of the target logical sector size"
        );
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
        let cleared = if matches!(p.target_kind, TargetKind::Block(_)) {
            let regions = trailing_gpt_regions(&mut target, p.image.size, p.capacity)?;
            clear_regions(&mut target, &regions, cancel)?;
            regions
        } else {
            Vec::new()
        };
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
        verify_cleared_regions(&mut target, &cleared, cancel)?;
        source_matches(&source, &p.image)?;
        if let TargetKind::Block(d) = &p.target_kind {
            forgeusb_device::revalidate(d)?;
        }
        emit(progress(
            Stage::Complete,
            v.bytes,
            p.image.size,
            if cleared.is_empty() {
                "Write and full source-length read-back verified".to_owned()
            } else {
                format!(
                    "Every source byte verified; {} stale GPT metadata bytes separately cleared and verified",
                    cleared.iter().map(|r| r.end - r.start).sum::<u64>()
                )
            },
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
/// Find only backup GPT metadata outside the copied image. A stale header at
/// the physical device end can otherwise override the newly written MBR on
/// firmware or partitioning tools. Never relocate or modify image metadata.
fn trailing_gpt_regions<T: Read + Seek>(
    target: &mut T,
    image_size: u64,
    capacity: u64,
) -> Result<Vec<std::ops::Range<u64>>> {
    if capacity < 512 || !capacity.is_multiple_of(512) || image_size > capacity - 512 {
        return Ok(Vec::new());
    }
    let header_start = capacity - 512;
    target.seek(SeekFrom::Start(header_start))?;
    let mut h = [0u8; 512];
    target
        .read_exact(&mut h)
        .context("read target-end GPT signature before cleanup")?;
    if &h[..8] != b"EFI PART" {
        return Ok(Vec::new());
    }
    let mut regions = Vec::with_capacity(2);
    regions.push(header_start..capacity);
    let u32_at = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().expect("GPT fixed slice"));
    let u64_at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().expect("GPT fixed slice"));
    let len = u32_at(12) as usize;
    let crc = u32_at(16);
    let table = u64_at(72).checked_mul(512);
    let bytes = (u32_at(80) as u64).checked_mul(u32_at(84) as u64);
    let entry = u32_at(84);
    let sane_header = (92..=512).contains(&len)
        && u64_at(24) == capacity / 512 - 1
        && entry >= 128
        && entry.is_multiple_of(8);
    h[16..20].fill(0);
    // Invalid metadata cannot authorize wiping arbitrary extents: invalidate
    // only the recognizable final header if its CRC or bounds cannot be trusted.
    if sane_header
        && crc32fast::hash(&h[..len]) == crc
        && let (Some(start), Some(bytes)) = (table, bytes)
        && bytes > 0
        && bytes <= 1024 * 1024
        && start >= image_size
        && let Some(end) = start.checked_add(bytes)
        && end <= header_start
    {
        regions.push(start..end);
    }
    Ok(regions)
}
fn clear_regions<T: Write + Seek>(
    target: &mut T,
    regions: &[std::ops::Range<u64>],
    cancel: &Cancellation,
) -> Result<()> {
    let zeros = [0u8; 64 * 1024];
    for region in regions {
        target.seek(SeekFrom::Start(region.start))?;
        let mut left = region.end - region.start;
        while left > 0 {
            cancel.check()?;
            let n = left.min(zeros.len() as u64) as usize;
            target
                .write_all(&zeros[..n])
                .context("clear stale target-end GPT metadata")?;
            left -= n as u64;
        }
    }
    Ok(())
}
fn verify_cleared_regions<T: Read + Seek>(
    target: &mut T,
    regions: &[std::ops::Range<u64>],
    cancel: &Cancellation,
) -> Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    for region in regions {
        target.seek(SeekFrom::Start(region.start))?;
        let mut left = region.end - region.start;
        while left > 0 {
            cancel.check()?;
            let n = left.min(buffer.len() as u64) as usize;
            target
                .read_exact(&mut buffer[..n])
                .context("read back cleared GPT metadata")?;
            ensure!(
                buffer[..n].iter().all(|&b| b == 0),
                "stale GPT metadata cleanup failed read-back verification"
            );
            left -= n as u64;
        }
    }
    Ok(())
}
fn tool_in(name: &str, paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().map(|dir| dir.join(name)).find(|path| {
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
    })
}
/// Read-only dependency reporting. Resolve unprivileged tools through PATH for
/// distro portability; this function never executes any discovered tool. Root
/// reporting uses trusted system directories and the helper stays fixed-path.
pub fn dependencies() -> serde_json::Value {
    let dirs: Vec<_> = if unsafe { libc::geteuid() } == 0 {
        ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
            .map(PathBuf::from)
            .into()
    } else {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default()
    };
    let tools = [
        ("lsblk", "lsblk"),
        ("udisksctl", "udisksctl"),
        ("pkexec", "pkexec"),
        ("helper", "/usr/lib/forgeusb/forgeusb-helper"),
        ("sfdisk", "sfdisk"),
        ("mkfs.fat", "mkfs.fat"),
        ("mkfs.ntfs", "mkfs.ntfs"),
        ("wimlib-imagex", "wimlib-imagex"),
        ("7z", "7z"),
        ("7zz", "7zz"),
        ("mcopy", "mcopy"),
        ("mmd", "mmd"),
        ("fsck.fat", "fsck.fat"),
        ("ntfsfix", "ntfsfix"),
        ("xorriso", "xorriso"),
        ("qemu", "qemu-system-x86_64"),
    ];
    serde_json::json!(
        tools
            .into_iter()
            .map(|(name, command)| {
                let path = if command.starts_with('/') {
                    tool_in(command, [PathBuf::new()])
                } else {
                    tool_in(command, dirs.clone())
                };
                serde_json::json!({"name": name, "installed": path.is_some(), "path": path})
            })
            .collect::<Vec<_>>()
    )
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
    fn stale_gpt_fixture() -> std::io::Cursor<Vec<u8>> {
        let mut bytes = vec![0xa5; 1024 * 1024];
        let capacity = bytes.len();
        let h = &mut bytes[capacity - 512..];
        h.fill(0);
        h[..8].copy_from_slice(b"EFI PART");
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&(capacity as u64 / 512 - 1).to_le_bytes());
        h[72..80].copy_from_slice(&(capacity as u64 / 512 - 33).to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        let crc = crc32fast::hash(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        std::io::Cursor::new(bytes)
    }
    #[test]
    fn stale_backup_cleanup_preserves_source_and_checks_cleared_metadata() {
        let mut target = stale_gpt_fixture();
        let original = target.get_ref().clone();
        let capacity = original.len() as u64;
        let image_size = 256 * 1024;
        let regions = trailing_gpt_regions(&mut target, image_size, capacity).unwrap();
        assert_eq!(
            regions,
            [
                capacity - 512..capacity,
                capacity - 33 * 512..capacity - 512
            ]
        );
        let cancel = Cancellation::default();
        clear_regions(&mut target, &regions, &cancel).unwrap();
        verify_cleared_regions(&mut target, &regions, &cancel).unwrap();
        assert_eq!(
            &target.get_ref()[..capacity as usize - 33 * 512],
            &original[..capacity as usize - 33 * 512]
        );
        target.get_mut()[capacity as usize - 1] = 1;
        assert!(verify_cleared_regions(&mut target, &regions, &cancel).is_err());
    }
    #[test]
    fn malformed_or_overlapping_old_gpt_cannot_expand_cleanup() {
        let mut target = stale_gpt_fixture();
        let capacity = target.get_ref().len() as u64;
        let header_start = capacity - 512;
        // No metadata bytes inside the source may be changed, even when a
        // valid old header points its entry array into the copied image.
        assert_eq!(
            trailing_gpt_regions(&mut target, capacity - 16 * 512, capacity).unwrap(),
            std::slice::from_ref(&(header_start..capacity))
        );
        assert!(
            trailing_gpt_regions(&mut target, capacity, capacity)
                .unwrap()
                .is_empty()
        );
        let h = &mut target.get_mut()[header_start as usize..];
        h[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            trailing_gpt_regions(&mut target, 512, capacity).unwrap(),
            std::slice::from_ref(&(header_start..capacity))
        );
        target.get_mut()[header_start as usize] = 0;
        assert!(
            trailing_gpt_regions(&mut target, 512, capacity)
                .unwrap()
                .is_empty()
        );
        assert!(
            trailing_gpt_regions(&mut std::io::Cursor::new([]), 0, 0)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn cleanup_obeys_cancellation_before_mutating_target() {
        let mut target = stale_gpt_fixture();
        let original = target.get_ref().clone();
        let capacity = original.len() as u64;
        let regions = trailing_gpt_regions(&mut target, 512, capacity).unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(clear_regions(&mut target, &regions, &cancel).is_err());
        assert_eq!(*target.get_ref(), original);
    }
    #[test]
    fn dependency_reporting_finds_executable_in_distro_specific_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("sfdisk");
        fs::write(&tool, "fixture; never executed").unwrap();
        assert!(tool_in("sfdisk", [dir.path().to_owned()]).is_none());
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(tool_in("sfdisk", [dir.path().to_owned()]), Some(tool));
    }
    #[test]
    fn modified_serialized_plan_never_creates_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = fixture(dir.path());
        let target = dir.path().join("new.img");
        let plan = plan(&source, &target, true).unwrap();
        for mutate in [
            |p: &mut Plan| p.image.sha256 = "0".repeat(64),
            |p: &mut Plan| p.capacity += 512,
            |p: &mut Plan| p.strategy = "untrusted".into(),
            |p: &mut Plan| p.confirmation = "forged approval".into(),
        ] {
            let mut modified = plan.clone();
            mutate(&mut modified);
            assert!(
                execute(
                    &modified,
                    &modified.confirmation,
                    &Cancellation::default(),
                    |_| {}
                )
                .is_err()
            );
            assert!(!target.exists());
        }
    }
    #[test]
    fn retained_source_descriptor_survives_parent_path_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("original");
        fs::create_dir(&source_dir).unwrap();
        let source_path = fixture(&source_dir);
        let target = dir.path().join("new.img");
        let plan = plan(&source_path, &target, true).unwrap();
        let source = open_source(&source_path).unwrap();
        let retained_dir = dir.path().join("retained");
        fs::rename(&source_dir, &retained_dir).unwrap();
        fs::create_dir(&source_dir).unwrap();
        fs::write(&source_path, vec![0u8; plan.image.size as usize]).unwrap();
        assert!(
            execute_opened(
                &plan,
                &plan.confirmation,
                source,
                &Cancellation::default(),
                |_| {}
            )
            .unwrap()
            .verified
        );
        assert_eq!(
            fs::read(target).unwrap(),
            fs::read(retained_dir.join("source.img")).unwrap()
        );
    }
    #[test]
    fn source_content_change_mid_write_never_reports_verified() {
        let dir = tempfile::tempdir().unwrap();
        let source = fixture(dir.path());
        let target = dir.path().join("new.img");
        let plan = plan(&source, &target, true).unwrap();
        let mut modified = false;
        let result = execute(
            &plan,
            &plan.confirmation,
            &Cancellation::default(),
            |event| {
                if event.stage == Stage::Writing && !modified {
                    modified = true;
                    let mut file = OpenOptions::new().write(true).open(&source).unwrap();
                    file.seek(SeekFrom::Start(1024 * 1024 + 1000)).unwrap();
                    file.write_all(&[99]).unwrap();
                }
            },
        );
        assert!(modified);
        assert!(result.unwrap_err().to_string().contains("partial data"));
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
