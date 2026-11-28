use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
#[path = "download/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "download/windows.rs"]
mod platform;

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(remaining)
}

pub(crate) struct ResponsePipes {
    stdout: platform::Pipe,
    stderr: platform::Pipe,
}

pub(crate) fn spawn_response(mut command: Command) -> io::Result<(Child, ResponsePipes)> {
    command.stdin(Stdio::null());
    let (child, stdout, stderr) = platform::Pipe::spawn_both(command)?;
    Ok((child, ResponsePipes { stdout, stderr }))
}

impl ResponsePipes {
    pub(crate) fn read_stdout(
        &mut self,
        buffer: &mut [u8],
        deadline: Instant,
    ) -> io::Result<usize> {
        self.stdout.read(buffer, deadline)
    }

    pub(crate) fn read_stderr(
        &mut self,
        buffer: &mut [u8],
        deadline: Instant,
    ) -> io::Result<usize> {
        self.stderr.read(buffer, deadline)
    }
}

/// Capture a bounded response and reap the direct child before returning.
/// The caller can share the same deadline with a fallback downloader.
pub(crate) fn capture(
    program: &str,
    args: &[&str],
    max_bytes: usize,
    deadline: Instant,
) -> Option<(ExitStatus, Vec<u8>)> {
    let mut command = Command::new(program);
    command.args(args);
    capture_command(command, max_bytes, deadline)
}

fn capture_command(
    mut command: Command,
    max_bytes: usize,
    deadline: Instant,
) -> Option<(ExitStatus, Vec<u8>)> {
    remaining(deadline).ok()?;
    command.stdin(Stdio::null()).stderr(Stdio::null());
    let (mut child, mut pipe) = platform::Pipe::spawn(command).ok()?;
    let result = collect(&mut child, &mut pipe, max_bytes, deadline);
    drop(pipe);
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result.ok()
}

fn collect(
    child: &mut Child,
    pipe: &mut platform::Pipe,
    max_bytes: usize,
    deadline: Instant,
) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut body = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        remaining(deadline)?;
        // Read at most one byte past the cap to distinguish an exact fit from overflow.
        let available = max_bytes.saturating_sub(body.len()).saturating_add(1);
        let chunk = available.min(buffer.len());
        let count = pipe.read(&mut buffer[..chunk], deadline)?;
        if count == 0 {
            break;
        }
        if count > max_bytes - body.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        body.extend_from_slice(&buffer[..count]);
    }
    loop {
        let remaining = remaining(deadline)?;
        if let Some(status) = child.try_wait()? {
            return Ok((status, body));
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "download::tests::child_fixture",
                "--nocapture",
                "--format",
                "terse",
                "--color",
                "never",
            ])
            .env("HERDR_LAZY_TEST_DOWNLOAD_CHILD", mode);
        command
    }

    #[test]
    fn child_fixture() {
        let Ok(mode) = std::env::var("HERDR_LAZY_TEST_DOWNLOAD_CHILD") else {
            return;
        };
        match mode.as_str() {
            "body" => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(b"1234").unwrap();
                stdout.flush().unwrap();
                std::process::exit(0);
            }
            "error" => std::process::exit(8),
            "stall" => thread::sleep(Duration::from_secs(3)),
            "retain-writer" => {
                // Only the descendant holds stdout after the direct child exits.
                let _child = fixture_command("stall").spawn().unwrap();
                std::process::exit(0);
            }
            _ => panic!("unexpected fixture mode"),
        }
    }

    fn capture_fixture(mode: &str, cap: usize) -> Option<(ExitStatus, Vec<u8>)> {
        capture_command(
            fixture_command(mode),
            cap,
            Instant::now() + Duration::from_secs(3),
        )
    }

    #[test]
    fn cap_accepts_exact_output_and_rejects_overflow() {
        let (status, bytes) = capture_fixture("body", 128).unwrap();
        assert!(status.success());
        // The libtest prefix counts towards the subprocess output limit too.
        assert!(bytes.ends_with(b"1234"));
        let (_, exact) = capture_fixture("body", bytes.len()).unwrap();
        assert_eq!(exact, bytes);
        assert!(capture_fixture("body", bytes.len() - 1).is_none());
    }

    #[test]
    fn captures_an_unsuccessful_exit_for_status_probes() {
        let (status, _) = capture_fixture("error", 128).unwrap();
        assert_eq!(status.code(), Some(8));
    }

    #[test]
    fn stalled_child_and_retained_writer_respect_shared_deadline() {
        for mode in ["stall", "retain-writer"] {
            let start = Instant::now();
            let deadline = start + Duration::from_millis(350);
            assert!(capture_command(fixture_command(mode), 128, deadline).is_none());
            assert!(start.elapsed() < Duration::from_secs(2));
            assert!(capture_command(fixture_command("body"), 128, deadline).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn stdout_closed_before_exit_respects_deadline() {
        let start = Instant::now();
        assert!(capture(
            "sh",
            &["-c", "exec 1>&-; exec sleep 3"],
            64,
            start + Duration::from_millis(350),
        )
        .is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
