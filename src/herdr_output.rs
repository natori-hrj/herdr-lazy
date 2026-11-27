use std::io;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

pub(crate) const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// Capture both Herdr response streams under one byte budget and deadline.
///
/// This is intentionally separate from the downloader: downloads have one payload stream and
/// discard stderr, while a failed Herdr command needs both streams for diagnosis. Stdin remains
/// closed, matching `Command::output` and preserving Herdr's existing invocation contract.
pub(crate) fn capture(program: &str, args: &[&str]) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    capture_with_timeout(program, args, RESPONSE_TIMEOUT)
}

pub(crate) fn capture_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut command = Command::new(program);
    command.args(args);
    capture_command(command, MAX_STREAM_BYTES, Instant::now() + timeout)
}

fn capture_command(
    command: Command,
    max_stream_bytes: usize,
    deadline: Instant,
) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    let (mut child, mut pipes) = crate::download::spawn_response(command)?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let mut buffer = [0; 64 * 1024];

    let result = 'capture: loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(found) => status = found,
                Err(error) => break Err(error),
            }
        }
        if stdout_done && stderr_done {
            if let Some(status) = status {
                break Ok((status, stdout, stderr));
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(response_error_with_kind(
                io::ErrorKind::TimedOut,
                format!(
                    "Herdr response timed out after {} milliseconds",
                    timeout.as_millis()
                ),
                &stderr,
            ));
        }
        if stdout_done && stderr_done {
            // EOF can precede direct-child exit (for example after it closes both streams).
            // Match the downloader's bounded child wait instead of polling try_wait at full CPU.
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
            continue;
        }
        for stream in [Stream::Stdout, Stream::Stderr] {
            let done = match stream {
                Stream::Stdout => &mut stdout_done,
                Stream::Stderr => &mut stderr_done,
            };
            if *done {
                continue;
            }
            let slice_deadline = deadline.min(Instant::now() + Duration::from_millis(1));
            let read = match stream {
                Stream::Stdout => pipes.read_stdout(&mut buffer, slice_deadline),
                Stream::Stderr => pipes.read_stderr(&mut buffer, slice_deadline),
            };
            match read {
                Ok(0) => *done = true,
                Ok(count) => {
                    let target = match stream {
                        Stream::Stdout => &mut stdout,
                        Stream::Stderr => &mut stderr,
                    };
                    if count > max_stream_bytes.saturating_sub(target.len()) {
                        let name = match stream {
                            Stream::Stdout => "stdout",
                            Stream::Stderr => "stderr",
                        };
                        stop_child(&mut child);
                        drain_stderr(
                            &mut pipes,
                            &mut stderr,
                            max_stream_bytes,
                            Instant::now() + Duration::from_millis(100),
                        );
                        break 'capture Err(response_error(
                            format!("Herdr {name} exceeded {max_stream_bytes} bytes"),
                            &stderr,
                        ));
                    }
                    target.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => {
                    let name = match stream {
                        Stream::Stdout => "stdout",
                        Stream::Stderr => "stderr",
                    };
                    break 'capture Err(response_error(
                        format!("could not read Herdr {name}: {error}"),
                        &stderr,
                    ));
                }
            }
        }
    };

    if result.is_err() {
        stop_child(&mut child);
        return result;
    }
    result
}

fn stop_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drain_stderr(
    pipes: &mut crate::download::ResponsePipes,
    stderr: &mut Vec<u8>,
    max_bytes: usize,
    deadline: Instant,
) {
    let mut buffer = [0; 8192];
    loop {
        match pipes.read_stderr(&mut buffer, deadline) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                let keep = count.min(max_bytes.saturating_sub(stderr.len()));
                stderr.extend_from_slice(&buffer[..keep]);
            }
        }
    }
}

fn response_error(message: String, stderr: &[u8]) -> io::Error {
    response_error_with_kind(io::ErrorKind::Other, message, stderr)
}

fn response_error_with_kind(kind: io::ErrorKind, message: String, stderr: &[u8]) -> io::Error {
    let diagnostic = String::from_utf8_lossy(stderr);
    let diagnostic = diagnostic.trim();
    if diagnostic.is_empty() {
        io::Error::new(kind, message)
    } else {
        io::Error::new(kind, format!("{message}; stderr: {diagnostic}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread;

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "herdr_output::tests::child_fixture",
                "--nocapture",
                "--format",
                "terse",
                "--color",
                "never",
            ])
            .env("HERDR_LAZY_TEST_RESPONSE_CHILD", mode);
        command
    }

    #[test]
    fn child_fixture() {
        let Ok(mode) = std::env::var("HERDR_LAZY_TEST_RESPONSE_CHILD") else {
            return;
        };
        match mode.as_str() {
            "stall" => thread::sleep(Duration::from_secs(3)),
            "output" => {
                let mut stdout = io::stdout().lock();
                let mut stderr = io::stderr().lock();
                stderr.write_all(b"diagnostic").unwrap();
                stderr.flush().unwrap();
                stdout.write_all(&[b'x'; 65]).unwrap();
                stdout.flush().unwrap();
                std::process::exit(0);
            }
            "retain-writers" => {
                let _child = fixture_command("stall").spawn().unwrap();
                std::process::exit(0);
            }
            #[cfg(unix)]
            "close-streams-before-exit" => {
                // SAFETY: this subprocess fixture intentionally closes its inherited response
                // descriptors, then remains alive long enough to exercise EOF-before-exit.
                unsafe {
                    libc::close(libc::STDOUT_FILENO);
                    libc::close(libc::STDERR_FILENO);
                }
                thread::sleep(Duration::from_millis(100));
                std::process::exit(0);
            }
            _ => panic!("unexpected fixture mode"),
        }
    }

    #[test]
    fn deadline_cancels_and_reaps_the_direct_child() {
        let start = Instant::now();
        let error = capture_command(
            fixture_command("stall"),
            1024,
            start + Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn separate_stream_budgets_keep_failure_diagnostics() {
        let (_, stdout, _) = capture_command(
            fixture_command("output"),
            4096,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        let cap = stdout.len() - 1;
        let error = capture_command(
            fixture_command("output"),
            cap,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&format!("stdout exceeded {cap} bytes")));
        assert!(message.contains("diagnostic"));
    }

    #[test]
    fn descendant_retaining_both_writers_does_not_leave_reader_threads() {
        let start = Instant::now();
        let error = capture_command(
            fixture_command("retain-writers"),
            1024,
            start + Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn eof_before_direct_child_exit_uses_bounded_child_wait() {
        let start = Instant::now();
        let (status, _, _) = capture_command(
            fixture_command("close-streams-before-exit"),
            1024,
            start + Duration::from_secs(1),
        )
        .unwrap();
        assert!(status.success());
        assert!(start.elapsed() >= Duration::from_millis(75));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
