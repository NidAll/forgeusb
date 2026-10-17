//! Unprivileged, verified construction of new regular-file installation media.
//! No block devices, mounts, loop devices, elevation, or implicit raw writes.
mod iso;
mod process;
mod snapshot;

use anyhow::{Context, Result, ensure};
use forgeusb_core::{Cancellation, Progress, Stage, progress};
use forgeusb_filesystem::FAT32_MAX_FILE_BYTES;
use forgeusb_partition::{
    Layout, Scheme, plan_uefi_fat32, verify_partition_table, write_partition_table,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use snapshot::Entry;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub scheme: Scheme,
    pub capacity_bytes: Option<u64>,
    pub timeout: Duration,
}
impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            scheme: Scheme::Mbr,
            capacity_bytes: None,
            timeout: Duration::from_secs(3600),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct BuildReport {
    pub output: PathBuf,
    pub image_bytes: u64,
    pub sha256: String,
    pub layout: Layout,
    pub files_verified: usize,
    pub payload_bytes: u64,
    pub efi_architectures: Vec<String>,
    pub wim_split: bool,
    pub source_kind: String,
    pub source_sha256: Option<String>,
    pub coverage: String,
}
struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn deadline(options: &BuildOptions) -> Result<Instant> {
    ensure!(
        !options.timeout.is_zero() && options.timeout <= Duration::from_secs(7200),
        "timeout must be greater than zero and at most 2 hours"
    );
    Instant::now()
        .checked_add(options.timeout)
        .context("timeout overflow")
}
fn emit_stage(emit: &mut impl FnMut(Progress), stage: Stage, done: u64, total: u64, message: &str) {
    emit(progress(stage, done, total, message));
}
fn dirs(entries: &[Entry]) -> Vec<PathBuf> {
    let mut all = BTreeSet::new();
    for e in entries {
        let mut parent = e.relative.parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            all.insert(p.to_owned());
            parent = p.parent();
        }
    }
    let mut paths = all.into_iter().collect::<Vec<_>>();
    paths.sort_by_key(|p| (p.components().count(), p.clone()));
    paths
}
fn run_args(
    tool: &Path,
    args: Vec<OsString>,
    cwd: &Path,
    cancel: &Cancellation,
    end: Instant,
) -> Result<()> {
    process::run(
        tool,
        &args,
        cwd,
        cancel,
        end,
        1024 * 1024,
        &mut std::io::sink(),
    )?;
    Ok(())
}
fn choose_capacity(bytes: u64, files: usize, requested: Option<u64>) -> Result<u64> {
    let overhead = bytes / 8
        + (files as u64)
            .checked_mul(8192)
            .context("filesystem overhead overflow")?
        + 64 * 1024 * 1024;
    let minimum = bytes
        .checked_add(overhead)
        .context("media capacity overflow")?
        .max(320 * 1024 * 1024);
    let automatic = minimum
        .checked_add(1024 * 1024 - 1)
        .context("media capacity overflow")?
        / (1024 * 1024)
        * (1024 * 1024);
    let capacity = requested.unwrap_or(automatic);
    ensure!(
        capacity >= minimum,
        "requested image is too small; require at least {} MiB including conservative FAT overhead",
        minimum.div_ceil(1024 * 1024)
    );
    ensure!(
        capacity <= snapshot::MAX_BYTES && capacity.is_multiple_of(512),
        "image capacity must be sector-aligned and at most 128 GiB"
    );
    Ok(capacity)
}
fn format_fat(
    volume: &Path,
    layout: &Layout,
    label: &str,
    cancel: &Cancellation,
    end: Instant,
    cwd: &Path,
) -> Result<()> {
    let mkfs = process::tool("mkfs.fat")?;
    let mut args = process::args(&["-F", "32", "-S", "512", "-n", label, "-h"]);
    args.push(layout.partitions[0].start_lba.to_string().into());
    args.push(volume.as_os_str().to_owned());
    run_args(&mkfs, args, cwd, cancel, end)
}
fn verify_fat_header(volume: &mut File, layout: &Layout) -> Result<()> {
    volume.seek(SeekFrom::Start(0))?;
    let mut b = [0; 512];
    volume.read_exact(&mut b)?;
    let u16at = |i| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32at = |i| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    ensure!(
        b[510..] == [0x55, 0xaa]
            && &b[82..90] == b"FAT32   "
            && u16at(11) == 512
            && b[13].is_power_of_two()
            && u16at(14) >= 2
            && b[16] == 2
            && u16at(17) == 0
            && u16at(22) == 0
            && u32at(36) > 0,
        "formatter did not produce expected FAT32 geometry"
    );
    ensure!(
        u32at(28) as u64 == layout.partitions[0].start_lba
            && u32at(32) as u64 <= layout.partitions[0].sectors
            && u32at(32) > 0,
        "FAT32 hidden-sector/partition-size mismatch"
    );
    let metadata = u64::from(u16at(14)) + u64::from(b[16]) * u64::from(u32at(36));
    let clusters = u64::from(u32at(32))
        .checked_sub(metadata)
        .context("FAT metadata exceeds volume")?
        / u64::from(b[13]);
    ensure!(
        (65525..0x0ffffff5).contains(&clusters),
        "FAT32 data cluster count out of range"
    );
    Ok(())
}
fn verify_fat(
    volume: &mut File,
    layout: &Layout,
    entries: &[Entry],
    cwd: &Path,
    cancel: &Cancellation,
    end: Instant,
    emit: &mut impl FnMut(Progress),
) -> Result<()> {
    let volume_path = snapshot::retained_path(volume);
    verify_fat_header(volume, layout)?;
    let check = process::tool("fsck.fat")?;
    let mut args = process::args(&["-n"]);
    args.push(volume_path.as_os_str().to_owned());
    run_args(&check, args, cwd, cancel, end)?;
    let mcopy = process::tool("mcopy")?;
    for (i, entry) in entries.iter().enumerate() {
        emit_stage(
            emit,
            Stage::Verifying,
            i as u64,
            entries.len() as u64,
            "Hashing every file read back from FAT32",
        );
        let mut args = process::args(&["-i"]);
        args.push(volume_path.as_os_str().to_owned());
        args.push(
            format!(
                "::/{}",
                entry.relative.to_str().context("non-UTF8 staged path")?
            )
            .into(),
        );
        args.push("-".into());
        let mut sha = DigestWriter(Sha256::new());
        let bytes = process::run(&mcopy, &args, cwd, cancel, end, entry.bytes, &mut sha)?;
        ensure!(
            bytes == entry.bytes && format!("{:x}", sha.0.finalize()) == entry.sha256,
            "FAT32 file read-back hash mismatch: {}",
            entry.relative.display()
        );
    }
    Ok(())
}
fn assemble(
    volume: &mut File,
    output: &Path,
    layout: &Layout,
    cancel: &Cancellation,
    end: Instant,
    emit: &mut impl FnMut(Progress),
    parent_anchor: &File,
) -> Result<(PathBuf, String)> {
    // tempfile persistence is an atomic, no-clobber publication. Until this
    // final step, failure/cancellation leaves no apparently usable output.
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize()?;
    let name = output.file_name().context("output requires a filename")?;
    let output = parent.join(name);
    ensure!(
        fs::symlink_metadata(&output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "output already exists or is inaccessible"
    );
    let anchored_parent = snapshot::retained_path(parent_anchor);
    let anchored_output = anchored_parent.join(name);
    let mut staged = tempfile::Builder::new()
        .prefix(".forgeusb-image-")
        .tempfile_in(&anchored_parent)?;
    staged.as_file_mut().set_len(layout.capacity_bytes)?;
    write_partition_table(staged.as_file_mut(), layout)?;
    let target = staged.as_file_mut();
    target.seek(SeekFrom::Start(
        layout.partitions[0].start_lba * layout.sector_bytes,
    ))?;
    volume.seek(SeekFrom::Start(0))?;
    let total = volume.metadata()?.len();
    let mut copied = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut source_hash = Sha256::new();
    loop {
        cancel.check()?;
        ensure!(Instant::now() < end, "media assembly timed out");
        let n = volume.read(&mut buf)?;
        if n == 0 {
            break;
        }
        target.write_all(&buf[..n])?;
        source_hash.update(&buf[..n]);
        copied += n as u64;
        emit_stage(
            emit,
            Stage::Writing,
            copied,
            total,
            "Assembling verified partition into disk image",
        );
    }
    ensure!(
        copied == layout.partitions[0].sectors * layout.sector_bytes,
        "partition payload size mismatch"
    );
    emit_stage(emit, Stage::Flushing, copied, total, "Flushing disk image");
    target.sync_all()?;
    verify_partition_table(target, layout)?;
    target.seek(SeekFrom::Start(
        layout.partitions[0].start_lba * layout.sector_bytes,
    ))?;
    let mut read_hash = Sha256::new();
    let mut remaining = total;
    while remaining > 0 {
        cancel.check()?;
        ensure!(Instant::now() < end, "partition read-back timed out");
        let take = (remaining as usize).min(buf.len());
        target.read_exact(&mut buf[..take])?;
        read_hash.update(&buf[..take]);
        remaining -= take as u64;
    }
    ensure!(
        source_hash.finalize() == read_hash.finalize(),
        "assembled partition read-back hash mismatch"
    );
    let sha256 = snapshot::hash(target, cancel, end)?;
    cancel.check()?;
    ensure!(
        Instant::now() < end,
        "media preparation timed out before publication"
    );
    let observed = snapshot::open_directory(&parent)?;
    use std::os::unix::fs::MetadataExt;
    ensure!(
        observed.metadata()?.dev() == parent_anchor.metadata()?.dev()
            && observed.metadata()?.ino() == parent_anchor.metadata()?.ino(),
        "output directory changed before publication"
    );
    staged
        .persist_noclobber(&anchored_output)
        .map_err(|e| e.error)
        .context("publishing new image without overwriting existing files")?;
    parent_anchor.sync_all()?;
    Ok((output, sha256))
}
/// Build Windows UEFI/FAT32 media from a directory or ISO9660/UDF image.
/// This verifies storage integrity, not Microsoft signatures or Windows Setup.
/// Input trees are copied through retained descriptors before tools consume them.
pub fn build_windows_image(
    source: &Path,
    output: &Path,
    options: &BuildOptions,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<BuildReport> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "installation media preparation must run as an ordinary user"
    );
    let end = deadline(options)?;
    cancel.check()?;
    ensure!(
        fs::symlink_metadata(output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "output already exists or is inaccessible"
    );
    // Dependency checks happen before copying large caller input.
    for name in ["mkfs.fat", "mcopy", "mmd", "fsck.fat", "wimlib-imagex"] {
        process::tool(name)?;
    }
    let output_parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    if fs::symlink_metadata(source)?.is_dir() {
        ensure!(
            !output_parent.starts_with(source.canonicalize()?),
            "output directory must be outside the source tree"
        );
    }
    let output_anchor = snapshot::open_directory(&output_parent)?;
    let workspace = tempfile::Builder::new()
        .prefix(".forgeusb-media-")
        .tempdir_in(snapshot::retained_path(&output_anchor))?;
    let workspace_anchor = snapshot::open_directory(workspace.path())?;
    let work_path = snapshot::retained_path(&workspace_anchor);
    let tree = work_path.join("tree");
    fs::create_dir(&tree)?;
    emit_stage(
        &mut emit,
        Stage::Inspecting,
        0,
        0,
        "Creating private, validated media snapshot",
    );
    let meta = fs::symlink_metadata(source)?;
    let (mut entries, source_kind, source_sha256) = if meta.is_dir() {
        (
            snapshot::snapshot_tree(source, &tree, cancel, end)?,
            "extracted_tree".into(),
            None,
        )
    } else {
        let mut input = snapshot::open_regular(source)?;
        let before = input.metadata()?;
        ensure!(
            before.len() > 0 && before.len() <= snapshot::MAX_BYTES,
            "ISO size outside supported safety budget"
        );
        let hash = snapshot::hash(&mut input, cancel, end)?;
        let (entries, kind) = iso::extract(&input, &tree, cancel, end)?;
        snapshot::stable(&before, &input.metadata()?)?;
        (entries, kind, Some(hash))
    };
    let mut report = forgeusb_windows::inspect_extracted_tree(&tree)?;
    ensure!(
        entries.iter().any(|e| e
            .relative
            .to_string_lossy()
            .eq_ignore_ascii_case("efi/microsoft/boot/bcd")
            && e.bytes > 0),
        "missing nonempty EFI/Microsoft/Boot/BCD required by Windows UEFI boot manager"
    );
    let wimlib = process::tool("wimlib-imagex")?;
    for image in [
        entries
            .iter()
            .find(|e| {
                e.relative
                    .to_string_lossy()
                    .eq_ignore_ascii_case("sources/boot.wim")
            })
            .context("missing boot.wim")?
            .relative
            .clone(),
        report.install_image.clone(),
    ] {
        let mut args = process::args(&["verify"]);
        args.push(tree.join(&image).as_os_str().to_owned());
        if image
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("swm"))
        {
            for part in entries.iter().filter(|entry| {
                let key = entry.relative.to_string_lossy().to_lowercase();
                if key == "sources/install.swm" {
                    return true;
                }
                key.strip_prefix("sources/install")
                    .and_then(|s| s.strip_suffix(".swm"))
                    .is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                    })
            }) {
                let mut reference = OsString::from("--ref=");
                reference.push(tree.join(&part.relative));
                args.push(reference);
            }
        }
        run_args(&wimlib, args, &work_path, cancel, end)
            .context("Windows WIM resource verification failed")?;
    }
    let mut wim_split = false;
    for path in &report.oversized_files {
        ensure!(
            path.to_string_lossy()
                .eq_ignore_ascii_case("sources/install.wim"),
            "oversized FAT32 file cannot be safely split: {} (only install.wim is supported; solid ESD needs conversion first)",
            path.display()
        );
    }
    if report.needs_wim_split {
        emit_stage(
            &mut emit,
            Stage::Writing,
            0,
            report.total_bytes,
            "Splitting install.wim for FAT32 with wimlib",
        );
        let source_path = tree.join(&report.install_image);
        let parts = forgeusb_windows::split_wim(
            &source_path,
            &work_path.join("split"),
            cancel,
            end.saturating_duration_since(Instant::now()),
        )?;
        let parent = source_path.parent().context("install image lacks parent")?;
        fs::remove_file(&source_path)?;
        entries.retain(|e| {
            !e.relative
                .to_string_lossy()
                .eq_ignore_ascii_case("sources/install.wim")
        });
        for part in parts {
            let name = part.file_name().context("split part lacks name")?;
            let dest = parent.join(name);
            ensure!(
                !dest.exists(),
                "split WIM output collides with existing media file"
            );
            fs::rename(&part, &dest)?;
            let mut f = snapshot::open_regular(&dest)?;
            entries.push(Entry {
                relative: dest.strip_prefix(&tree)?.into(),
                bytes: f.metadata()?.len(),
                sha256: snapshot::hash(&mut f, cancel, end)?,
            });
        }
        report = forgeusb_windows::inspect_extracted_tree(&tree)?;
        wim_split = true;
    }
    ensure!(
        report.oversized_files.is_empty()
            && entries.iter().all(|e| e.bytes <= FAT32_MAX_FILE_BYTES),
        "FAT32 oversize file remains after staging"
    );
    let bytes = entries.iter().try_fold(0u64, |n, e| {
        n.checked_add(e.bytes).context("payload size overflow")
    })?;
    let capacity = choose_capacity(bytes, entries.len(), options.capacity_bytes)?;
    let layout = plan_uefi_fat32(capacity, 512, options.scheme)?;
    let volume_path = work_path.join("partition.fat32");
    let mut volume = snapshot::exclusive(&volume_path)?;
    volume.set_len(layout.partitions[0].sectors * 512)?;
    emit_stage(
        &mut emit,
        Stage::Writing,
        0,
        bytes,
        "Formatting private FAT32 partition",
    );
    format_fat(&volume_path, &layout, "FORGEUSB", cancel, end, &work_path)?;
    let mmd = process::tool("mmd")?;
    let mcopy = process::tool("mcopy")?;
    for dir in dirs(&entries) {
        let mut args = process::args(&["-i"]);
        args.push(volume_path.as_os_str().to_owned());
        args.push(format!("::/{}", dir.to_str().context("non-UTF8 directory")?).into());
        run_args(&mmd, args, &work_path, cancel, end)?;
    }
    let mut done = 0;
    for entry in &entries {
        let mut args = process::args(&["-n", "-i"]);
        args.push(volume_path.as_os_str().to_owned());
        args.push(tree.join(&entry.relative).as_os_str().to_owned());
        args.push(
            format!(
                "::/{}",
                entry.relative.to_str().context("non-UTF8 filename")?
            )
            .into(),
        );
        run_args(&mcopy, args, &work_path, cancel, end)?;
        done += entry.bytes;
        emit_stage(
            &mut emit,
            Stage::Writing,
            done,
            bytes,
            "Copying installation files into FAT32",
        );
    }
    volume.sync_all()?;
    verify_fat(
        &mut volume,
        &layout,
        &entries,
        &work_path,
        cancel,
        end,
        &mut emit,
    )?;
    let (output, sha256) = assemble(
        &mut volume,
        output,
        &layout,
        cancel,
        end,
        &mut emit,
        &output_anchor,
    )?;
    emit_stage(
        &mut emit,
        Stage::Complete,
        capacity,
        capacity,
        "Verified Windows UEFI media image ready",
    );
    Ok(BuildReport{output,image_bytes:capacity,sha256,layout,files_verified:entries.len(),payload_bytes:bytes,efi_architectures:report.efi_architectures,wim_split,source_kind,source_sha256,coverage:"Verified WIM resource data via wimlib, partition tables (including both GPT copies/CRCs), FAT32 filesystem consistency, every staged file SHA-256 read back from FAT32, and whole assembled partition SHA-256. Windows-shaped simulation does not validate genuine Windows 10/11 Setup, BIOS boot, physical USB firmware or Secure Boot/signature acceptance. Original EFI loaders are preserved; no unsigned NTFS driver is introduced.".into()})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn windows_fixture(root: &Path) {
        fs::create_dir_all(root.join("sources")).unwrap();
        fs::create_dir_all(root.join("EFI/BOOT")).unwrap();
        fs::create_dir_all(root.join("EFI/Microsoft/Boot")).unwrap();
        let payload = tempfile::tempdir().unwrap();
        fs::write(
            payload.path().join("synthetic.txt"),
            b"A genuine tiny WIM container, not Windows.",
        )
        .unwrap();
        let tool = process::tool("wimlib-imagex").unwrap();
        for name in ["boot.wim", "install.wim"] {
            let args = vec![
                "capture".into(),
                payload.path().as_os_str().to_owned(),
                root.join("sources").join(name).as_os_str().to_owned(),
            ];
            run_args(
                &tool,
                args,
                payload.path(),
                &Cancellation::default(),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        }
        fs::write(
            root.join("EFI/Microsoft/Boot/BCD"),
            b"regf synthetic BCD; not real Windows media",
        )
        .unwrap();
        let mut pe = vec![0u8; 134];
        pe[..2].copy_from_slice(b"MZ");
        pe[60..64].copy_from_slice(&128u32.to_le_bytes());
        pe[128..132].copy_from_slice(b"PE\0\0");
        pe[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(root.join("EFI/BOOT/BOOTX64.EFI"), pe).unwrap();
        fs::write(
            root.join("spaces and unicode Ω.txt"),
            b"Long filename round trip\r\n",
        )
        .unwrap();
    }
    fn dependencies() -> bool {
        let mut missing = [
            "mkfs.fat",
            "mcopy",
            "mmd",
            "fsck.fat",
            "wimlib-imagex",
            "xorriso",
            "genisoimage",
        ]
        .into_iter()
        .filter(|s| process::tool(s).is_err())
        .collect::<Vec<_>>();
        if process::tool("7z")
            .or_else(|_| process::tool("7zz"))
            .is_err()
        {
            missing.push("7z or 7zz");
        }
        if missing.is_empty() {
            true
        } else {
            assert!(
                std::env::var_os("FORGEUSB_REQUIRE_MEDIA_TOOLS").is_none(),
                "required media fixture tools missing: {missing:?}"
            );
            eprintln!("external media test skipped; missing {missing:?}");
            false
        }
    }
    #[test]
    fn cancellation_and_no_clobber() {
        if !dependencies() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        fs::create_dir(&input).unwrap();
        windows_fixture(&input);
        let out = d.path().join("image");
        fs::write(&out, b"preserve").unwrap();
        assert!(
            build_windows_image(
                &input,
                &out,
                &BuildOptions::default(),
                &Cancellation::default(),
                |_| {}
            )
            .is_err()
        );
        assert_eq!(fs::read(&out).unwrap(), b"preserve");
        fs::remove_file(&out).unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(
            build_windows_image(&input, &out, &BuildOptions::default(), &cancel, |_| {}).is_err()
        );
        assert!(!out.exists());
    }
    #[test]
    fn extracted_tree_and_iso_roundtrip() {
        if !dependencies() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        fs::create_dir(&input).unwrap();
        windows_fixture(&input);
        for scheme in [Scheme::Mbr, Scheme::Gpt] {
            let out = d.path().join(format!("{scheme:?}.img"));
            let report = build_windows_image(
                &input,
                &out,
                &BuildOptions {
                    scheme,
                    ..Default::default()
                },
                &Cancellation::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(report.files_verified, 5);
            assert_eq!(report.source_kind, "extracted_tree");
            assert_eq!(report.efi_architectures, ["x86_64"]);
            let mut f = File::open(&out).unwrap();
            verify_partition_table(&mut f, &report.layout).unwrap();
            assert_eq!(
                report.sha256,
                snapshot::hash(
                    &mut f,
                    &Cancellation::default(),
                    Instant::now() + Duration::from_secs(60)
                )
                .unwrap()
            );
        }
        if let Ok(xorriso) = process::tool("xorriso") {
            let iso = d.path().join("windows-shaped.iso");
            let args = vec![
                "-as".into(),
                "mkisofs".into(),
                "-J".into(),
                "-r".into(),
                "-o".into(),
                iso.as_os_str().to_owned(),
                input.as_os_str().to_owned(),
            ];
            run_args(
                &xorriso,
                args,
                d.path(),
                &Cancellation::default(),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
            let report = build_windows_image(
                &iso,
                &d.path().join("iso.img"),
                &BuildOptions::default(),
                &Cancellation::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(report.files_verified, 5);
            assert_eq!(report.source_kind, "iso9660");
            assert!(report.source_sha256.is_some());
        }
        if let Ok(geniso) = process::tool("genisoimage") {
            let iso = d.path().join("windows-shaped-udf.iso");
            let args = vec![
                "-udf".into(),
                "-J".into(),
                "-r".into(),
                "-o".into(),
                iso.as_os_str().to_owned(),
                input.as_os_str().to_owned(),
            ];
            run_args(
                &geniso,
                args,
                d.path(),
                &Cancellation::default(),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
            let report = build_windows_image(
                &iso,
                &d.path().join("udf.img"),
                &BuildOptions::default(),
                &Cancellation::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(report.source_kind, "udf_iso");
            assert_eq!(report.files_verified, 5);
        }
    }
    #[test]
    fn cancellation_during_assembly_never_publishes() {
        if !dependencies() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        fs::create_dir(&input).unwrap();
        windows_fixture(&input);
        let out = d.path().join("cancel.img");
        let cancel = Cancellation::default();
        let trigger = cancel.clone();
        assert!(
            build_windows_image(&input, &out, &BuildOptions::default(), &cancel, |p| {
                if p.message == "Assembling verified partition into disk image" {
                    trigger.cancel();
                }
            })
            .is_err()
        );
        assert!(!out.exists());
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            1,
            "private staging artifacts must clean up after cancellation"
        );
    }
    #[test]
    fn unsafe_source_never_publishes_output() {
        if !dependencies() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("input");
        fs::create_dir(&input).unwrap();
        windows_fixture(&input);
        std::os::unix::fs::symlink("/etc/passwd", input.join("escape")).unwrap();
        let out = d.path().join("unsafe.img");
        assert!(
            build_windows_image(
                &input,
                &out,
                &BuildOptions::default(),
                &Cancellation::default(),
                |_| {}
            )
            .is_err()
        );
        assert!(!out.exists());
        fs::remove_file(input.join("escape")).unwrap();
        fs::write(input.join("σ.txt"), b"same").unwrap();
        fs::write(input.join("ς.txt"), b"same").unwrap();
        assert!(
            build_windows_image(
                &input,
                &out,
                &BuildOptions::default(),
                &Cancellation::default(),
                |_| {}
            )
            .is_err()
        );
        assert!(!out.exists());
    }
}

#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub filesystem: forgeusb_filesystem::Filesystem,
    pub scheme: Scheme,
    pub capacity_bytes: u64,
    pub label: String,
    pub timeout: Duration,
}
#[derive(Debug, Clone, Serialize)]
pub struct FormatReport {
    pub output: PathBuf,
    pub image_bytes: u64,
    pub sha256: String,
    pub layout: Layout,
    pub filesystem: forgeusb_filesystem::Filesystem,
    pub label: String,
    pub coverage: String,
}
fn verify_ntfs_header(volume: &mut File, layout: &Layout) -> Result<()> {
    volume.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 512];
    volume.read_exact(&mut header)?;
    let u16at = |i| u16::from_le_bytes([header[i], header[i + 1]]);
    let u32at = |i| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
    let u64at = |i| u64::from_le_bytes(header[i..i + 8].try_into().unwrap());
    ensure!(
        &header[3..11] == b"NTFS    "
            && header[510..] == [0x55, 0xaa]
            && u16at(11) == 512
            && header[13].is_power_of_two(),
        "formatter did not produce expected NTFS geometry"
    );
    let sectors = layout.partitions[0].sectors;
    ensure!(
        u32at(28) as u64 == layout.partitions[0].start_lba
            && u64at(40) <= sectors
            && u64at(40) >= sectors.saturating_sub(8),
        "NTFS volume/partition size mismatch"
    );
    ensure!(
        u64at(48)
            .checked_mul(u64::from(header[13]))
            .is_some_and(|s| s < sectors)
            && u64at(56)
                .checked_mul(u64::from(header[13]))
                .is_some_and(|s| s < sectors),
        "NTFS MFT geometry out of bounds"
    );
    volume.seek(SeekFrom::Start((sectors - 1) * 512))?;
    let mut backup = [0; 512];
    volume.read_exact(&mut backup)?;
    ensure!(header == backup, "NTFS backup boot sector differs");
    Ok(())
}
/// Create a new FAT32 EFI or NTFS data disk image. Formatting never targets a
/// device and installs no BIOS bootstrap, UEFI:NTFS driver, or operating system.
pub fn format_image(
    output: &Path,
    options: &FormatOptions,
    cancel: &Cancellation,
    mut emit: impl FnMut(Progress),
) -> Result<FormatReport> {
    use forgeusb_filesystem::Filesystem;
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "image formatting must run as an ordinary user"
    );
    ensure!(
        matches!(options.filesystem, Filesystem::Fat32 | Filesystem::Ntfs),
        "image formatting supports FAT32 and NTFS"
    );
    ensure!(
        options.capacity_bytes <= snapshot::MAX_BYTES,
        "image exceeds 128 GiB safety budget"
    );
    let end = deadline(&BuildOptions {
        scheme: options.scheme,
        capacity_bytes: Some(options.capacity_bytes),
        timeout: options.timeout,
    })?;
    let label = &options.label;
    ensure!(
        !label.is_empty()
            && !label
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
        "volume label contains forbidden characters"
    );
    if options.filesystem == Filesystem::Fat32 {
        ensure!(
            label.len() <= 11
                && label.bytes().all(|b| b.is_ascii_uppercase()
                    || b.is_ascii_digit()
                    || b == b' '
                    || b == b'_'),
            "FAT32 label must contain 1 to 11 uppercase ASCII letters, digits, spaces or underscores"
        );
    } else {
        ensure!(
            label.encode_utf16().count() <= 32,
            "NTFS label exceeds 32 UTF-16 code units"
        );
    }
    let layout = if options.filesystem == Filesystem::Fat32 {
        plan_uefi_fat32(options.capacity_bytes, 512, options.scheme)?
    } else {
        forgeusb_partition::plan_ntfs(options.capacity_bytes, 512, options.scheme)?
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    ensure!(
        fs::symlink_metadata(output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "output already exists or is inaccessible"
    );
    let output_anchor = snapshot::open_directory(&parent)?;
    let work = tempfile::Builder::new()
        .prefix(".forgeusb-format-")
        .tempdir_in(snapshot::retained_path(&output_anchor))?;
    let work_anchor = snapshot::open_directory(work.path())?;
    let work_path = snapshot::retained_path(&work_anchor);
    let path = work_path.join("partition");
    let mut volume = snapshot::exclusive(&path)?;
    volume.set_len(layout.partitions[0].sectors * 512)?;
    emit_stage(
        &mut emit,
        Stage::Writing,
        0,
        options.capacity_bytes,
        "Formatting private partition image",
    );
    if options.filesystem == Filesystem::Fat32 {
        format_fat(&path, &layout, label, cancel, end, &work_path)?;
        verify_fat(
            &mut volume,
            &layout,
            &[],
            &work_path,
            cancel,
            end,
            &mut emit,
        )?;
    } else {
        let mkfs = process::tool("mkfs.ntfs").or_else(|_| process::tool("mkntfs"))?;
        // -F is needed by ntfs-3g for an ordinary file. The target is a new
        // private regular file retained above; no block path is accepted.
        let mut args = process::args(&[
            "-F", "-Q", "-s", "512", "-H", "255", "-S", "63", "-L", label, "-p",
        ]);
        args.push(layout.partitions[0].start_lba.to_string().into());
        args.push(path.as_os_str().to_owned());
        run_args(&mkfs, args, &work_path, cancel, end)?;
        verify_ntfs_header(&mut volume, &layout)?;
        let ntfsfix = process::tool("ntfsfix")?;
        let mut args = process::args(&["-n"]);
        args.push(path.as_os_str().to_owned());
        run_args(&ntfsfix, args, &work_path, cancel, end)?;
    }
    volume.sync_all()?;
    let (output, sha256) = assemble(
        &mut volume,
        output,
        &layout,
        cancel,
        end,
        &mut emit,
        &output_anchor,
    )?;
    emit_stage(
        &mut emit,
        Stage::Complete,
        options.capacity_bytes,
        options.capacity_bytes,
        "Verified formatted disk image ready",
    );
    Ok(FormatReport{output,image_bytes:options.capacity_bytes,sha256,layout,filesystem:options.filesystem,label:label.clone(),coverage:"New ordinary-file image only. Partition metadata, filesystem boot geometry, filesystem diagnostic check and assembled partition SHA-256 verified. Blank FAT32/NTFS format does not install boot loaders, confer Secure Boot trust, or prove firmware/Windows compatibility. ntfsfix is a limited consistency diagnostic, not Windows chkdsk.".into()})
}
