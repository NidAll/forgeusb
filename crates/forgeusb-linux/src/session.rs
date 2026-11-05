//! Desktop-session storage actions through UDisks2 (`udisksctl`).
//!
//! These run as the ordinary desktop user, before planning (unmount) and after
//! a verified write (power off). UDisks applies its own Polkit rules, so the
//! user can only unmount or power off what their session is allowed to. The
//! privileged write helper never unmounts anything: a mounted target is still
//! refused there. Only USB devices whose sole problem is mounted filesystems
//! are ever touched; internal or otherwise refused disks are never unmounted.
use anyhow::{Context, Result, bail, ensure};
use forgeusb_device::Device;
use std::{
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn udisksctl() -> Result<PathBuf> {
    for candidate in ["/usr/bin/udisksctl", "/bin/udisksctl"] {
        if let Ok(m) = std::fs::metadata(candidate)
            && m.is_file()
            && m.uid() == 0
            && m.mode() & 0o022 == 0
            && m.mode() & 0o111 != 0
        {
            return Ok(candidate.into());
        }
    }
    bail!("udisksctl (UDisks2) is not installed; unmount the drive in your file manager instead")
}

fn run(args: &[&str]) -> Result<()> {
    let mut child = Command::new(udisksctl()?)
        .args(args)
        .arg("--no-user-interaction")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("start udisksctl")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut message = String::new();
            if let Some(err) = child.stderr.take() {
                let _ = err.take(4096).read_to_string(&mut message);
            }
            ensure!(
                status.success(),
                "{}",
                message
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("udisksctl failed")
                    .trim()
            );
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("udisksctl did not finish within 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn find(path: &Path) -> Result<Device> {
    forgeusb_device::discover()?
        .into_iter()
        .find(|d| d.path == path)
        .with_context(|| format!("{} is not a discovered whole disk", path.display()))
}

fn same_drive(a: &Device, b: &Device) -> bool {
    a.identity.major == b.identity.major
        && a.identity.minor == b.identity.minor
        && a.identity.sysfs_path == b.identity.sysfs_path
        && a.identity.serial == b.identity.serial
        && a.identity.wwid == b.identity.wwid
}

fn unmount_all(device: &Device) -> Result<usize> {
    let mut count = 0;
    for part in device.partitions.iter().filter(|p| !p.mounts.is_empty()) {
        let path = part.path.to_str().context("non-UTF-8 partition path")?;
        run(&["unmount", "-b", path]).with_context(|| format!("unmount {path}"))?;
        count += 1;
    }
    if count == 0 && !device.mounts.is_empty() {
        // A partitionless ("superfloppy") filesystem mounted on the disk itself.
        let path = device.path.to_str().context("non-UTF-8 device path")?;
        run(&["unmount", "-b", path]).with_context(|| format!("unmount {path}"))?;
        count += 1;
    }
    Ok(count)
}

/// Unmount every filesystem on a USB drive so it can be planned and written.
/// Returns the freshly discovered device, which must now be eligible.
pub fn unmount(target: &Path) -> Result<Device> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "unmount through the desktop session as your normal user, not as root"
    );
    let device = find(target)?;
    if device.eligible {
        return Ok(device);
    }
    ensure!(
        device.transport == "usb" && device.blocked_only_by_mounts,
        "{} cannot be prepared by unmounting: {}",
        device.path.display(),
        device.reasons.join("; ")
    );
    unmount_all(&device)?;
    // UDisks completes asynchronously with respect to mountinfo; allow it to settle.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let fresh = find(target)?;
        ensure!(
            same_drive(&device, &fresh),
            "a different drive appeared at {}",
            target.display()
        );
        if fresh.eligible {
            return Ok(fresh);
        }
        if Instant::now() > deadline {
            bail!("drive is still not ready: {}", fresh.reasons.join("; "));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// After a verified write, unmount anything the desktop auto-mounted and power
/// the drive off so it can be unplugged safely.
pub fn power_off(expected: &Device) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "power off through the desktop session as your normal user, not as root"
    );
    let device = find(&expected.path)?;
    ensure!(
        same_drive(expected, &device) && device.transport == "usb",
        "the drive at {} changed; not powering it off",
        expected.path.display()
    );
    unmount_all(&device)?;
    let path = device.path.to_str().context("non-UTF-8 device path")?;
    run(&["power-off", "-b", path]).context("power off the drive")
}
