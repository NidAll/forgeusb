use anyhow::{Context, Result, bail, ensure};
use forgeusb_core::Cancellation;
use std::os::fd::AsRawFd;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn tool(name: &str) -> Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !dir.is_absolute() {
            continue;
        }
        let p = dir.join(name);
        if let Ok(m) = fs::metadata(&p)
            && m.is_file()
            && m.mode() & 0o111 != 0
            && m.mode() & 0o022 == 0
        {
            return Ok(p);
        }
    }
    bail!(
        "required tool {name} is unavailable; install the corresponding OS package (no installation is performed automatically)"
    )
}
struct Guard {
    child: Child,
    reaped: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.reaped {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "cannot configure bounded tool output"
    );
    Ok(())
}
/// Commands use argument vectors, a clean environment, a deadline/cancellation,
/// independent bounded output pipes, and a dedicated process group for cleanup.
pub fn run(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    cancel: &Cancellation,
    deadline: Instant,
    limit: u64,
    sink: &mut dyn Write,
) -> Result<u64> {
    cancel.check()?;
    ensure!(Instant::now() < deadline, "media operation timed out");
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("PATH", "/usr/bin:/bin")
        .env("MTOOLSRC", "/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // Cap parser/formatter address-space usage without changing the parent.
    unsafe {
        cmd.pre_exec(|| {
            let l = libc::rlimit {
                rlim_cur: 4 * 1024 * 1024 * 1024,
                rlim_max: 4 * 1024 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &l) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Guard {
        child: cmd
            .spawn()
            .with_context(|| format!("starting {}", program.display()))?,
        reaped: false,
    };
    let mut stdout = child.child.stdout.take().context("missing tool stdout")?;
    let mut stderr = child.child.stderr.take().context("missing tool stderr")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let mut out_eof = false;
    let mut err_eof = false;
    let mut output = 0u64;
    let mut errors = Vec::new();
    let mut status = None;
    let mut buf = [0u8; 65536];
    loop {
        cancel.check()?;
        ensure!(
            Instant::now() < deadline,
            "media tool {} timed out",
            program.display()
        );
        for _ in 0..16 {
            match stdout.read(&mut buf) {
                Ok(0) => {
                    out_eof = true;
                    break;
                }
                Ok(n) => {
                    output = output
                        .checked_add(n as u64)
                        .context("tool output overflow")?;
                    ensure!(output <= limit, "tool output exceeds declared byte budget");
                    sink.write_all(&buf[..n])?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("reading tool output"),
            }
        }
        for _ in 0..16 {
            match stderr.read(&mut buf) {
                Ok(0) => {
                    err_eof = true;
                    break;
                }
                Ok(n) => {
                    let remaining = 65536usize.saturating_sub(errors.len());
                    errors.extend_from_slice(&buf[..n.min(remaining)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("reading tool diagnostics"),
            }
        }
        if status.is_none() && out_eof && err_eof {
            status = child.child.try_wait()?;
            child.reaped = status.is_some();
        }
        if let Some(status) = status
            && out_eof
            && err_eof
        {
            ensure!(
                status.success(),
                "{} failed ({status}): {}",
                program
                    .file_name()
                    .unwrap_or(OsStr::new("tool"))
                    .to_string_lossy(),
                String::from_utf8_lossy(&errors)
            );
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_tool_output_and_deadline() {
        let shell = tool("sh").unwrap();
        let temp = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        let script_args = args(&["-c", "while :; do printf '0123456789'; done"]);
        let result = run(
            &shell,
            &script_args,
            temp.path(),
            &cancel,
            Instant::now() + Duration::from_secs(2),
            32,
            &mut std::io::sink(),
        );
        assert!(result.unwrap_err().to_string().contains("byte budget"));
        let args = super::args(&["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        assert!(
            run(
                &shell,
                &args,
                temp.path(),
                &cancel,
                start + Duration::from_millis(100),
                32,
                &mut std::io::sink()
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
