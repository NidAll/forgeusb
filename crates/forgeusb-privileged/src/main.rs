//! Minimal Polkit entrypoint. Receives one bounded plan; engine independently revalidates it.
//! `write` erases and writes the confirmed plan; `verify` only reads the drive back.
use anyhow::{Context, Result, ensure};
use forgeusb_core::Cancellation;
use forgeusb_linux::{HelperEvent, Plan, TargetKind, VerifyRequest, WriteRequest};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::MetadataExt;
fn emit(event: HelperEvent) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let Ok(s) = serde_json::to_string(&event) {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}
fn groups() -> Result<Vec<libc::gid_t>> {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    ensure!(
        (0..=65536).contains(&count),
        "cannot read current group credentials"
    );
    let mut groups = vec![0; count as usize];
    ensure!(
        unsafe { libc::getgroups(count, groups.as_mut_ptr()) } == count,
        "group credentials changed"
    );
    Ok(groups)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Verify,
}
fn validate_invocation(args: &[String]) -> Result<Mode> {
    let mode = match args {
        [mode, plan, protocol]
            if plan == "--plan-json" && protocol == forgeusb_linux::HELPER_PROTOCOL_ARGUMENT =>
        {
            match mode.as_str() {
                "write" => Some(Mode::Write),
                "verify" => Some(Mode::Verify),
                _ => None,
            }
        }
        _ => None,
    };
    mode.context(
        "helper protocol mismatch: install the matching ForgeUSB app and helper; usage: forgeusb-helper write|verify --plan-json --protocol-v3 (one JSON request on stdin)",
    )
}
fn run() -> Result<()> {
    let mode = validate_invocation(&std::env::args().skip(1).collect::<Vec<_>>())?;
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "helper requires Polkit authorization"
    );
    let uid: u32 = std::env::var("PKEXEC_UID")
        .context("invoke through pkexec, not a root GUI")?
        .parse()?;
    ensure!(uid != 0, "requesting user must be unprivileged");
    let stdin = io::stdin();
    let mut bytes = Vec::new();
    stdin
        .lock()
        .take(4 * 1024 * 1024 + 1)
        .read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024 && bytes.last() == Some(&b'\n'),
        "request must be a single bounded JSON line"
    );
    let (plan, confirmation): (Plan, Option<String>) = match mode {
        Mode::Write => {
            let request: WriteRequest = serde_json::from_slice(&bytes)?;
            (request.plan, Some(request.confirmation))
        }
        Mode::Verify => (serde_json::from_slice::<VerifyRequest>(&bytes)?.plan, None),
    };
    ensure!(
        matches!(plan.target_kind, TargetKind::Block(_)),
        "helper only accepts physical block-device plans"
    );
    // Open with the requesting user's filesystem credentials: no privileged source reads.
    let old_groups = groups()?;
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut resolved = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    ensure!(
        unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut resolved,
            )
        } == 0
            && !resolved.is_null(),
        "cannot resolve requesting user's credentials"
    );
    let pwd = unsafe { pwd.assume_init() };
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    ensure!(
        unsafe { libc::initgroups(name.as_ptr(), pwd.pw_gid) } == 0,
        "cannot assume requesting user's groups"
    );
    let old_gid = unsafe { libc::setfsgid(pwd.pw_gid) };
    let gid_switched = unsafe { libc::setfsgid(u32::MAX) } as u32 == pwd.pw_gid;
    let old = unsafe { libc::setfsuid(uid) };
    let switched = unsafe { libc::setfsuid(u32::MAX) } as u32 == uid;
    let source = if switched && gid_switched {
        forgeusb_image::open_source(&plan.image.path)
    } else {
        Err(anyhow::anyhow!(
            "could not assume requesting user's filesystem credentials"
        ))
    };
    unsafe {
        libc::setfsuid(old as u32);
        libc::setfsgid(old_gid as u32);
    };
    let groups_restored = unsafe { libc::setgroups(old_groups.len(), old_groups.as_ptr()) } == 0;
    ensure!(
        groups_restored
            && groups()? == old_groups
            && unsafe { libc::setfsgid(u32::MAX) } == old_gid,
        "could not restore filesystem group credentials"
    );
    ensure!(
        unsafe { libc::setfsuid(u32::MAX) } == old,
        "could not restore filesystem credentials"
    );
    let source = source.context("source must be readable by the requesting user")?;
    ensure!(
        source.metadata()?.uid() == uid,
        "source must be owned by the requesting user; copy it into your own image directory first"
    );
    ensure!(
        forgeusb_image::FileStamp::of(&source)? == plan.image.stamp,
        "source identity changed"
    );
    let cancel = Cancellation::default();
    let c = cancel.clone();
    std::thread::spawn(move || {
        let mut line = Vec::new();
        // A disconnected or malformed control stream cancels, never authorizes continued writes.
        let _ = io::stdin().lock().take(65).read_until(b'\n', &mut line);
        c.cancel();
    });
    let report = match confirmation {
        Some(confirmation) => {
            forgeusb_linux::execute_opened(&plan, &confirmation, source, &cancel, |p| {
                emit(HelperEvent::Progress(p))
            })?
        }
        None => forgeusb_linux::verify_opened(&plan, source, &cancel, |p| {
            emit(HelperEvent::Progress(p))
        })?,
    };
    emit(HelperEvent::Complete(report));
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        emit(HelperEvent::Error(format!("{e:#}")));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_rejects_legacy_and_unknown_protocols_before_request_io() {
        for args in [
            vec!["write", "--plan-json"],
            vec!["write", "--plan-json", "--protocol-v1"],
            vec!["write", "--plan-json", "--protocol-v2"],
            vec!["write", "--plan-json", "--protocol-v4"],
            vec!["verify", "--plan-json", "--protocol-v2"],
            vec!["erase", "--plan-json", "--protocol-v3"],
            vec!["verify", "--plan-json", "--protocol-v3", "extra"],
            vec!["--plan-json", "verify", "--protocol-v3"],
        ] {
            assert!(
                validate_invocation(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .is_err()
            );
        }
        for (name, mode) in [("write", Mode::Write), ("verify", Mode::Verify)] {
            assert_eq!(
                validate_invocation(&[
                    name.into(),
                    "--plan-json".into(),
                    forgeusb_linux::HELPER_PROTOCOL_ARGUMENT.into()
                ])
                .unwrap(),
                mode
            );
        }
    }
}
