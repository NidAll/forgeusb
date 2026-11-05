mod human;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use forgeusb_core::{Cancellation, Progress};
use forgeusb_image::Coverage;
use forgeusb_linux::{HelperEvent, Plan, TargetKind, WriteRequest};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
#[derive(Parser)]
#[command(
    name = "forgeusb",
    version,
    about = "Linux boot-media writer with explicit device confirmation and full read-back"
)]
struct Args {
    /// Machine-readable output (JSON documents or newline-delimited events).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}
#[derive(clap::Args, Debug, Default)]
struct WindowsOptions {
    /// Skip the Windows 11 TPM 2.0, Secure Boot and 4 GB RAM checks.
    #[arg(long)]
    bypass_requirements: bool,
    /// Allow finishing setup without a Microsoft account.
    #[arg(long)]
    no_online_account: bool,
    /// Create a local administrator account (empty password; Windows asks for one at first sign-in).
    #[arg(long, value_name = "NAME")]
    local_account: Option<String>,
    /// Use your desktop's regional settings (or pass a locale such as fr-FR).
    #[arg(long, value_name = "LOCALE", num_args = 0..=1, default_missing_value = "host")]
    regional: Option<String>,
    /// Decline optional data collection in the privacy questions.
    #[arg(long)]
    no_data_collection: bool,
    /// Prevent automatic BitLocker device encryption.
    #[arg(long)]
    no_bitlocker: bool,
}
impl WindowsOptions {
    fn customization(&self) -> Result<forgeusb_windows::unattend::Customization> {
        let regional = match self.regional.as_deref() {
            None => None,
            Some("host") => Some(
                forgeusb_windows::unattend::host_locale()
                    .context("cannot read your desktop locale; pass --regional xx-YY")?,
            ),
            Some(tag) => Some(tag.to_owned()),
        };
        Ok(forgeusb_windows::unattend::Customization {
            bypass_requirements: self.bypass_requirements,
            bypass_online_account: self.no_online_account,
            local_account: self.local_account.clone(),
            regional,
            disable_data_collection: self.no_data_collection,
            disable_bitlocker: self.no_bitlocker,
        })
    }
}
#[derive(Subcommand)]
enum Cmd {
    /// List disks, whether each can be written, and what is on it.
    List,
    /// Inspect an image and compute its SHA-256; no target access.
    Inspect {
        image: PathBuf,
        /// The image was made by ForgeUSB: digest its data ranges only.
        #[arg(long)]
        prepared: bool,
    },
    /// Compute MD5, SHA-1, SHA-256 and SHA-512, and compare with a published checksum.
    Checksum {
        image: PathBuf,
        /// Expected digest; otherwise a SHA256SUMS-style file beside the image is used.
        #[arg(long)]
        expect: Option<String>,
    },
    /// Read-only exact operation preview. Save --json output for non-interactive writing.
    Plan {
        image: PathBuf,
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        test_file: bool,
        /// The image was made by prepare-windows or format-image: skip its
        /// unused filesystem space instead of writing zeros. Never use this
        /// for decompressed or downloaded images, whose zeros are data.
        #[arg(long)]
        prepared: bool,
    },
    /// Write a previously reviewed JSON plan. Prompts unless --confirm is supplied.
    Write {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        confirm: Option<String>,
        /// Unmount anything auto-mounted and power the drive off when verified.
        #[arg(long)]
        eject: bool,
    },
    /// Compare every planned byte against the target. Does not prove firmware compatibility.
    Verify {
        image: PathBuf,
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        test_file: bool,
        #[arg(long)]
        prepared: bool,
    },
    /// Unmount every filesystem on a USB drive through UDisks2 (desktop session).
    Unmount { device: PathBuf },
    /// Unmount and power off a USB drive so it can be unplugged safely.
    Eject { device: PathBuf },
    /// Expand a .gz/.xz/.zst/.bz2/.zip disk image into a NEW sparse image file.
    Decompress {
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Refuse images that expand beyond this size (default: 2 TiB, or the --device capacity).
        #[arg(long)]
        limit_mib: Option<u64>,
        #[arg(long)]
        device: Option<PathBuf>,
    },
    /// Discover required/optional tools without installing or executing them.
    CheckDependencies,
    /// Inspect an already extracted Windows installer tree (no disk writes).
    InspectWindowsTree { tree: PathBuf },
    /// Split a WIM into a NEW staging directory with system wimlib (no device access).
    SplitWim {
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 3600)]
        timeout_seconds: u64,
        #[arg(long, default_value_t = 3800, value_parser = clap::value_parser!(u32).range(1..=3800))]
        part_size_mib: u32,
    },
    /// Prepare Windows ISO/tree as a NEW verified FAT32 disk image, without root or device access.
    /// Then use `plan --prepared` and `write` to deploy it.
    PrepareWindows {
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = PartitionScheme::Mbr)]
        scheme: PartitionScheme,
        /// Size the image to fill this USB drive (recommended).
        #[arg(long, conflicts_with = "capacity_mib")]
        device: Option<PathBuf>,
        /// Explicit image capacity in MiB; otherwise sized to the payload.
        #[arg(long)]
        capacity_mib: Option<u64>,
        #[arg(long, default_value = "FORGEUSB")]
        label: String,
        #[command(flatten)]
        windows: WindowsOptions,
        #[arg(long, default_value_t = 3600)]
        timeout_seconds: u64,
    },
    /// Create a NEW partitioned FAT32/NTFS/exFAT/ext4 disk image. No bootloader installation.
    FormatImage {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        filesystem: FormatFilesystem,
        #[arg(long, value_enum, default_value_t = PartitionScheme::Mbr)]
        scheme: PartitionScheme,
        /// Size the image to fill this USB drive.
        #[arg(
            long,
            conflicts_with = "capacity_mib",
            required_unless_present = "capacity_mib"
        )]
        device: Option<PathBuf>,
        #[arg(long)]
        capacity_mib: Option<u64>,
        #[arg(long, default_value = "FORGEUSB")]
        label: String,
        #[arg(long, default_value_t = 3600)]
        timeout_seconds: u64,
    },
}
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum FormatFilesystem {
    Fat32,
    Ntfs,
    Exfat,
    Ext4,
}
impl From<FormatFilesystem> for forgeusb_filesystem::Filesystem {
    fn from(value: FormatFilesystem) -> Self {
        match value {
            FormatFilesystem::Fat32 => Self::Fat32,
            FormatFilesystem::Ntfs => Self::Ntfs,
            FormatFilesystem::Exfat => Self::Exfat,
            FormatFilesystem::Ext4 => Self::Ext4,
        }
    }
}
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum PartitionScheme {
    Mbr,
    Gpt,
}
impl From<PartitionScheme> for forgeusb_partition::Scheme {
    fn from(value: PartitionScheme) -> Self {
        match value {
            PartitionScheme::Mbr => Self::Mbr,
            PartitionScheme::Gpt => Self::Gpt,
        }
    }
}
fn out_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
/// Whether a carriage-return progress line on stderr still needs ending.
static PROGRESS_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn end_progress_line() {
    if PROGRESS_OPEN.swap(false, std::sync::atomic::Ordering::SeqCst) {
        eprintln!();
    }
}
fn completion(r: &forgeusb_linux::Report, json: bool) -> Result<()> {
    end_progress_line();
    if json {
        println!(
            "{}",
            serde_json::to_string(&HelperEvent::Complete(r.clone()))?
        );
    } else {
        print!("{}", human::report(r));
    }
    Ok(())
}
fn event(p: Progress, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&HelperEvent::Progress(p)).unwrap_or_default()
        );
    } else {
        let percent = p
            .completed
            .saturating_mul(100)
            .checked_div(p.total)
            .unwrap_or(0);
        eprint!(
            "\r{:<12} {:>3}%  {:<60}",
            format!("{:?}", p.stage),
            percent,
            p.message.chars().take(60).collect::<String>()
        );
        PROGRESS_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
        if p.stage == forgeusb_core::Stage::Complete {
            end_progress_line();
        }
    }
}
fn absolute(p: PathBuf) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}
fn coverage(prepared: bool) -> Coverage {
    if prepared {
        Coverage::DataRanges
    } else {
        Coverage::Full
    }
}
/// Capacity of an eligible (or merely mounted) USB drive, for sizing new images.
fn device_capacity(path: &Path) -> Result<u64> {
    let device = forgeusb_device::discover()?
        .into_iter()
        .find(|d| d.path == path)
        .context("--device is not a discovered whole disk")?;
    ensure!(
        device.eligible || device.blocked_only_by_mounts,
        "{} cannot be used: {}",
        path.display(),
        device.reasons.join("; ")
    );
    Ok(device.capacity)
}
fn run(args: Args) -> Result<()> {
    let cancel = Cancellation::default();
    let c = cancel.clone();
    ctrlc::set_handler(move || c.cancel())?;
    let json = args.json;
    match args.command {
        Cmd::List => {
            let devices = forgeusb_device::discover()?;
            if json {
                out_json(&devices)
            } else {
                print!("{}", human::devices(&devices));
                Ok(())
            }
        }
        Cmd::Inspect { image, prepared } => {
            let info =
                forgeusb_image::inspect_with(&absolute(image)?, &cancel, coverage(prepared))?;
            if json {
                out_json(&info)
            } else {
                print!("{}", human::image(&info));
                Ok(())
            }
        }
        Cmd::Checksum { image, expect } => {
            let image = absolute(image)?;
            let sums = forgeusb_image::checksums::compute(&image, &cancel, |p| event(p, false))?;
            let published = forgeusb_image::checksums::find_published(&image);
            let expected = expect
                .clone()
                .or_else(|| published.as_ref().map(|p| p.digest.clone()));
            let matched = expected.as_deref().and_then(|e| sums.matches(e));
            if json {
                out_json(&serde_json::json!({
                    "checksums": sums,
                    "expected": expected,
                    "expected_from": published.as_ref().map(|p| &p.list),
                    "match": matched,
                }))?;
            } else {
                end_progress_line();
                println!(
                    "MD5      {}\nSHA-1    {}\nSHA-256  {}\nSHA-512  {}",
                    sums.md5, sums.sha1, sums.sha256, sums.sha512
                );
                if let Some(e) = &expected {
                    let from = if expect.is_some() {
                        "--expect".to_owned()
                    } else {
                        published
                            .as_ref()
                            .map(|p| p.list.display().to_string())
                            .unwrap_or_default()
                    };
                    match matched {
                        Some(alg) => println!("Match    {alg} matches {from}"),
                        None => println!("MISMATCH {e} from {from} matches none of the above"),
                    }
                }
            }
            ensure!(
                expected.is_none() || matched.is_some(),
                "checksum mismatch: the image is not the published file"
            );
            Ok(())
        }
        Cmd::Plan {
            image,
            device,
            test_file,
            prepared,
        } => {
            let plan = forgeusb_linux::plan_with(
                &absolute(image)?,
                &absolute(device)?,
                test_file,
                coverage(prepared),
                &cancel,
            )?;
            if json {
                out_json(&plan)
            } else {
                print!("{}", human::plan(&plan));
                eprintln!(
                    "Save this plan with --json > plan.json, then: forgeusb write --plan plan.json"
                );
                Ok(())
            }
        }
        Cmd::CheckDependencies => {
            let deps = forgeusb_linux::dependencies();
            if json {
                out_json(&deps)
            } else {
                print!("{}", human::dependencies(&deps));
                Ok(())
            }
        }
        Cmd::InspectWindowsTree { tree } => {
            out_json(&forgeusb_windows::inspect_extracted_tree(&absolute(tree)?)?)
        }
        Cmd::SplitWim {
            source,
            output,
            timeout_seconds,
            part_size_mib,
        } => {
            let outputs = forgeusb_windows::split_wim_with_size(
                &absolute(source)?,
                &absolute(output)?,
                &cancel,
                std::time::Duration::from_secs(timeout_seconds),
                part_size_mib,
            )?;
            if json {
                out_json(&outputs)
            } else {
                for o in outputs {
                    println!("{}", o.display());
                }
                Ok(())
            }
        }
        Cmd::Unmount { device } => {
            let d = forgeusb_linux::session::unmount(&absolute(device)?)?;
            if json {
                out_json(&d)
            } else {
                println!("{} is unmounted and ready to plan.", human::device_name(&d));
                Ok(())
            }
        }
        Cmd::Eject { device } => {
            let path = absolute(device)?;
            let d = forgeusb_device::discover()?
                .into_iter()
                .find(|d| d.path == path)
                .context("not a discovered whole disk")?;
            forgeusb_linux::session::power_off(&d)?;
            if !json {
                println!(
                    "{} was powered off; you can unplug it.",
                    human::device_name(&d)
                );
            }
            Ok(())
        }
        Cmd::Decompress {
            source,
            output,
            limit_mib,
            device,
        } => {
            let limit = match (limit_mib, device) {
                (Some(mib), _) => mib.checked_mul(1024 * 1024).context("limit overflow")?,
                (None, Some(d)) => device_capacity(&absolute(d)?)?,
                (None, None) => forgeusb_media::MAX_IMAGE_BYTES,
            };
            let report = forgeusb_media::decompress::decompress_image(
                &absolute(source)?,
                &absolute(output)?,
                limit,
                &cancel,
                |p| event(p, false),
            )?;
            if json {
                out_json(&report)
            } else {
                end_progress_line();
                println!(
                    "Expanded {} {} → {} ({} stored)\nSource SHA-256: {}\nNext: forgeusb plan {} --device /dev/sdX",
                    report.compression.name(),
                    human::bytes(report.source_bytes),
                    human::bytes(report.bytes),
                    human::bytes(report.stored_bytes),
                    report.source_sha256,
                    report.output.display()
                );
                Ok(())
            }
        }
        Cmd::PrepareWindows {
            source,
            output,
            scheme,
            device,
            capacity_mib,
            label,
            windows,
            timeout_seconds,
        } => {
            let capacity_bytes = match (device, capacity_mib) {
                (Some(d), _) => Some(device_capacity(&absolute(d)?)?),
                (None, Some(mib)) => {
                    Some(mib.checked_mul(1024 * 1024).context("capacity overflow")?)
                }
                (None, None) => None,
            };
            let report = forgeusb_media::build_windows_image(
                &absolute(source)?,
                &absolute(output)?,
                &forgeusb_media::BuildOptions {
                    scheme: scheme.into(),
                    capacity_bytes,
                    timeout: std::time::Duration::from_secs(timeout_seconds),
                    label,
                    customization: windows.customization()?,
                },
                &cancel,
                |p| event(p, false),
            )?;
            if json {
                out_json(&report)
            } else {
                end_progress_line();
                println!(
                    "Prepared {} ({} files verified, {} payload)\nSHA-256 (data ranges): {}",
                    report.output.display(),
                    report.files_verified,
                    human::bytes(report.payload_bytes),
                    report.sha256
                );
                for c in &report.customizations {
                    println!("Setup option: {c}");
                }
                println!(
                    "Next: forgeusb plan {} --device /dev/sdX --prepared",
                    report.output.display()
                );
                Ok(())
            }
        }
        Cmd::FormatImage {
            output,
            filesystem,
            scheme,
            device,
            capacity_mib,
            label,
            timeout_seconds,
        } => {
            let capacity_bytes = match (device, capacity_mib) {
                (Some(d), _) => device_capacity(&absolute(d)?)?,
                (None, Some(mib)) => mib.checked_mul(1024 * 1024).context("capacity overflow")?,
                (None, None) => anyhow::bail!("pass --device or --capacity-mib"),
            };
            let report = forgeusb_media::format_image(
                &absolute(output)?,
                &forgeusb_media::FormatOptions {
                    filesystem: filesystem.into(),
                    scheme: scheme.into(),
                    capacity_bytes,
                    label,
                    timeout: std::time::Duration::from_secs(timeout_seconds),
                },
                &cancel,
                |p| event(p, false),
            )?;
            if json {
                out_json(&report)
            } else {
                end_progress_line();
                println!(
                    "Formatted {} ({:?}, {})\nNext: forgeusb plan {} --device /dev/sdX --prepared",
                    report.output.display(),
                    report.filesystem,
                    human::bytes(report.image_bytes),
                    report.output.display()
                );
                Ok(())
            }
        }
        Cmd::Verify {
            image,
            device,
            test_file,
            prepared,
        } if !test_file => {
            // Plan read-only as the desktop user, then let the helper revalidate
            // the drive and read it back; it never opens the drive for writing.
            let p = forgeusb_linux::plan_with(
                &absolute(image)?,
                &absolute(device)?,
                false,
                coverage(prepared),
                &cancel,
            )?;
            privileged_verify(p, json, &cancel)
        }
        Cmd::Verify {
            image,
            device,
            test_file,
            prepared,
        } => {
            let r = forgeusb_linux::verify_with(
                &absolute(image)?,
                &absolute(device)?,
                test_file,
                coverage(prepared),
                &cancel,
                |p| event(p, json),
            )?;
            completion(&r, json)
        }
        Cmd::Write {
            plan,
            confirm,
            eject,
        } => {
            let file = forgeusb_image::open_source(&plan)?;
            ensure!(file.metadata()?.is_file(), "plan must be a regular file");
            let mut bytes = Vec::new();
            file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 4 * 1024 * 1024, "plan exceeds 4 MiB limit");
            let p: Plan = serde_json::from_slice(&bytes)?;
            let confirmation = match confirm {
                Some(c) => c,
                None => {
                    ensure!(
                        io::stdin().is_terminal(),
                        "noninteractive writes require --plan and exact --confirm; there is no --yes override"
                    );
                    eprint!("{}", human::plan(&p));
                    eprintln!("Type exactly: {}", p.confirmation);
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    line.trim_end().to_string()
                }
            };
            ensure!(
                confirmation == p.confirmation,
                "confirmation mismatch; nothing written"
            );
            let device = match &p.target_kind {
                TargetKind::Block(d) => Some((**d).clone()),
                TargetKind::TestFile => None,
            };
            if device.is_some() && unsafe { libc::geteuid() } != 0 {
                privileged_write(p, confirmation, json, &cancel)?;
            } else {
                ensure!(
                    unsafe { libc::geteuid() } != 0 || device.is_some(),
                    "root may not use disposable-file mode"
                );
                let r = forgeusb_linux::execute(&p, &confirmation, &cancel, |p| event(p, json))?;
                completion(&r, json)?;
            }
            if eject && let Some(d) = device {
                forgeusb_linux::session::power_off(&d)?;
                if !json {
                    println!("Drive powered off; you can unplug it.");
                }
            }
            Ok(())
        }
    }
}
fn privileged_write(
    plan: Plan,
    confirmation: String,
    json: bool,
    cancel: &Cancellation,
) -> Result<()> {
    let (size, hash) = (plan.image.size, plan.image.sha256.clone());
    let request = serde_json::to_string(&WriteRequest { plan, confirmation })?;
    run_helper("write", &request, size, &hash, json, cancel)
}

/// Read a physical drive back through the helper: discovery as the desktop
/// user cannot open block devices, and the helper never writes in this mode.
fn privileged_verify(plan: Plan, json: bool, cancel: &Cancellation) -> Result<()> {
    let (size, hash) = (plan.image.size, plan.image.sha256.clone());
    let request = serde_json::to_string(&forgeusb_linux::VerifyRequest { plan })?;
    run_helper("verify", &request, size, &hash, json, cancel)
}

fn run_helper(
    mode: &'static str,
    request: &str,
    expected_size: u64,
    expected_hash: &str,
    json: bool,
    cancel: &Cancellation,
) -> Result<()> {
    let helper = std::path::Path::new("/usr/lib/forgeusb/forgeusb-helper");
    ensure!(
        helper.is_file(),
        "privileged helper is not installed. Build/package ForgeUSB and install its Polkit policy; never run the GUI as root."
    );
    use std::os::unix::fs::MetadataExt;
    for path in [
        "/usr",
        "/usr/lib",
        "/usr/lib/forgeusb",
        "/usr/lib/forgeusb/forgeusb-helper",
        "/usr/bin/pkexec",
    ] {
        let m = std::fs::symlink_metadata(path)?;
        ensure!(
            !m.file_type().is_symlink() && m.uid() == 0 && m.mode() & 0o022 == 0,
            "untrusted privileged path: {path}"
        );
    }
    let mut child = Command::new("/usr/bin/pkexec")
        .arg(helper)
        .args([
            mode,
            "--plan-json",
            forgeusb_linux::HELPER_PROTOCOL_ARGUMENT,
        ])
        .env_clear()
        .env("PATH", "/usr/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start Polkit helper")?;
    let mut stdin = child.stdin.take().context("helper stdin")?;
    writeln!(stdin, "{request}")?;
    stdin.flush()?;
    let input = Arc::new(Mutex::new(Some(stdin)));
    let writer = input.clone();
    let cancellation = cancel.clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = done.clone();
    let monitor = std::thread::spawn(move || {
        while !finished.load(std::sync::atomic::Ordering::SeqCst) {
            if cancellation.is_cancelled() {
                if let Ok(mut guard) = writer.lock()
                    && let Some(mut input) = guard.take()
                {
                    let _ = writeln!(input, "cancel");
                    let _ = input.flush();
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let result = (|| -> Result<()> {
        let output = child.stdout.take().context("helper stdout")?;
        let mut complete = false;
        let mut reader = io::BufReader::new(output);
        loop {
            let mut line = Vec::new();
            let n = reader
                .by_ref()
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            ensure!(
                n <= 1024 * 1024 && line.last() == Some(&b'\n'),
                "oversized or truncated helper response"
            );
            match serde_json::from_slice::<HelperEvent>(&line)? {
                HelperEvent::Progress(p) => event(p, json),
                HelperEvent::Complete(r) => {
                    ensure!(
                        !complete
                            && r.verified
                            && r.bytes == expected_size
                            && r.sha256 == expected_hash
                            && (mode == "write" || r.written == 0),
                        "helper completion does not match approved image"
                    );
                    complete = true;
                    completion(&r, json)?;
                }
                HelperEvent::Error(e) => anyhow::bail!("{e}"),
            }
        }
        let status = child.wait()?;
        ensure!(
            status.success() && complete,
            "{}",
            if mode == "write" {
                "helper failed or stopped; target may be incomplete"
            } else {
                "helper verification failed or stopped"
            }
        );
        Ok(())
    })();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = monitor.join();
    if result.is_err() {
        if let Ok(mut guard) = input.lock()
            && let Some(mut writer) = guard.take()
        {
            let _ = writeln!(writer, "cancel");
            let _ = writer.flush();
        }
        let _ = child.wait();
    }
    result
}
fn main() {
    let args = Args::parse();
    let json = args.json;
    if let Err(e) = run(args) {
        end_progress_line();
        if json {
            eprintln!(
                "{}",
                serde_json::json!({"error":format!("{e:#}"),"exit_code":1})
            );
        } else {
            eprintln!("ForgeUSB: {e:#}");
        }
        std::process::exit(1);
    }
}
