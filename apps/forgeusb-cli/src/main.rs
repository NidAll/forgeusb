use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use forgeusb_core::{Cancellation, Progress};
use forgeusb_linux::{HelperEvent, Plan, TargetKind, WriteRequest};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::PathBuf,
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
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    /// Discover whole block devices, eligibility, mounted filesystems and identity.
    List,
    /// Structurally inspect and SHA-256 an image; no target access.
    Inspect { image: PathBuf },
    /// Read-only exact operation preview. Save --json output for non-interactive writing.
    Plan {
        image: PathBuf,
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        test_file: bool,
    },
    /// Write a previously reviewed JSON plan. Prompts unless --confirm is supplied.
    Write {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Compare every source byte against the target. Does not prove firmware compatibility.
    Verify {
        image: PathBuf,
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        test_file: bool,
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
    /// Then use plan/write to deploy it with the usual device safeguards.
    PrepareWindows {
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = PartitionScheme::Mbr)]
        scheme: PartitionScheme,
        /// Optional image capacity in MiB; otherwise sized to the payload.
        #[arg(long)]
        capacity_mib: Option<u64>,
        #[arg(long, default_value_t = 3600)]
        timeout_seconds: u64,
    },
    /// Create a NEW partitioned FAT32/NTFS regular disk image. No device access or bootloader installation.
    FormatImage {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        filesystem: FormatFilesystem,
        #[arg(long, value_enum, default_value_t = PartitionScheme::Mbr)]
        scheme: PartitionScheme,
        #[arg(long)]
        capacity_mib: u64,
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
}
impl From<FormatFilesystem> for forgeusb_filesystem::Filesystem {
    fn from(value: FormatFilesystem) -> Self {
        match value {
            FormatFilesystem::Fat32 => Self::Fat32,
            FormatFilesystem::Ntfs => Self::Ntfs,
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
fn show<T: serde::Serialize + std::fmt::Debug>(v: &T, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        println!("{v:#?}");
    }
    Ok(())
}
fn completion(r: &forgeusb_linux::Report, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&HelperEvent::Complete(r.clone()))?
        );
        Ok(())
    } else {
        show(r, false)
    }
}
fn event(p: Progress, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&HelperEvent::Progress(p)).unwrap_or_default()
        );
    } else {
        eprintln!(
            "{:?} {:>3}%  {}",
            p.stage,
            p.completed
                .saturating_mul(100)
                .checked_div(p.total)
                .unwrap_or(0),
            p.message
        );
    }
}
fn absolute(p: PathBuf) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}
fn run(args: Args) -> Result<()> {
    let cancel = Cancellation::default();
    let c = cancel.clone();
    ctrlc::set_handler(move || c.cancel())?;
    match args.command {
        Cmd::List => show(&forgeusb_device::discover()?, args.json),
        Cmd::Inspect { image } => show(
            &forgeusb_image::inspect_cancel(&absolute(image)?, &cancel)?,
            args.json,
        ),
        Cmd::Plan {
            image,
            device,
            test_file,
        } => show(
            &forgeusb_linux::plan_cancel(
                &absolute(image)?,
                &absolute(device)?,
                test_file,
                &cancel,
            )?,
            args.json,
        ),
        Cmd::CheckDependencies => show(&forgeusb_linux::dependencies(), args.json),
        Cmd::InspectWindowsTree { tree } => show(
            &forgeusb_windows::inspect_extracted_tree(&absolute(tree)?)?,
            args.json,
        ),
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
            show(&outputs, args.json)
        }
        Cmd::PrepareWindows {
            source,
            output,
            scheme,
            capacity_mib,
            timeout_seconds,
        } => {
            let capacity_bytes = capacity_mib
                .map(|value| value.checked_mul(1024 * 1024).context("capacity overflow"))
                .transpose()?;
            let report = forgeusb_media::build_windows_image(
                &absolute(source)?,
                &absolute(output)?,
                &forgeusb_media::BuildOptions {
                    scheme: scheme.into(),
                    capacity_bytes,
                    timeout: std::time::Duration::from_secs(timeout_seconds),
                },
                &cancel,
                |p| event(p, false),
            )?;
            show(&report, args.json)
        }
        Cmd::FormatImage {
            output,
            filesystem,
            scheme,
            capacity_mib,
            label,
            timeout_seconds,
        } => {
            let capacity_bytes = capacity_mib
                .checked_mul(1024 * 1024)
                .context("capacity overflow")?;
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
            show(&report, args.json)
        }
        Cmd::Verify {
            image,
            device,
            test_file,
        } => {
            let r = forgeusb_linux::verify(
                &absolute(image)?,
                &absolute(device)?,
                test_file,
                &cancel,
                |p| event(p, args.json),
            )?;
            completion(&r, args.json)
        }
        Cmd::Write { plan, confirm } => {
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
                    eprintln!(
                        "Target: {} ({} bytes)\nStrategy: {}\nSHA-256: {}\n{}\nType exactly: {}",
                        p.target.display(),
                        p.capacity,
                        p.strategy,
                        p.image.sha256,
                        p.warnings.join("\n"),
                        p.confirmation
                    );
                    let mut line = String::new();
                    io::stdin().read_line(&mut line)?;
                    line.trim_end().to_string()
                }
            };
            ensure!(
                confirmation == p.confirmation,
                "confirmation mismatch; nothing written"
            );
            if matches!(p.target_kind, TargetKind::Block(_)) && unsafe { libc::geteuid() } != 0 {
                privileged_write(p, confirmation, args.json, &cancel)
            } else {
                ensure!(
                    unsafe { libc::geteuid() } != 0
                        || matches!(p.target_kind, TargetKind::Block(_)),
                    "root may not use disposable-file mode"
                );
                let r =
                    forgeusb_linux::execute(&p, &confirmation, &cancel, |p| event(p, args.json))?;
                completion(&r, args.json)
            }
        }
    }
}
fn privileged_write(
    plan: Plan,
    confirmation: String,
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
    let expected_size = plan.image.size;
    let expected_hash = plan.image.sha256.clone();
    let mut child = Command::new("/usr/bin/pkexec")
        .arg(helper)
        .args([
            "write",
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
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&WriteRequest { plan, confirmation })?
    )?;
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
                            && r.sha256 == expected_hash,
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
            "helper failed or stopped; target may be incomplete"
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
