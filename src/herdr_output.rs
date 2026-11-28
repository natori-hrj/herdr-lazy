use std::io;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

pub(crate) const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const INITIAL_CHILD_BACKOFF: Duration = Duration::from_micros(200);
const MAX_CHILD_BACKOFF: Duration = Duration::from_millis(5);

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
    capture_response(&mut child, &mut pipes, max_stream_bytes, deadline, timeout)
}

fn capture_response(
    child: &mut Child,
    pipes: &mut crate::download::ResponsePipes,
    max_stream_bytes: usize,
    deadline: Instant,
    timeout: Duration,
) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let mut child_backoff = INITIAL_CHILD_BACKOFF;
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
            std::thread::sleep(next_child_delay(&mut child_backoff, remaining));
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
                        stop_child(child);
                        drain_stderr(
                            pipes,
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
        stop_child(child);
        return result;
    }
    result
}

fn next_child_delay(backoff: &mut Duration, remaining: Duration) -> Duration {
    let delay = (*backoff).min(remaining);
    *backoff = (*backoff * 2).min(MAX_CHILD_BACKOFF);
    delay
}

fn stop_child(child: &mut Child) {
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

    const EOF_FIXTURE_EXIT_CODE: i32 = 37;

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
            "stderr-overflow" => {
                io::stderr().write_all(&[b'x'; 4097]).unwrap();
                std::process::exit(0);
            }
            "retain-writers" => {
                let _child = fixture_command("stall").spawn().unwrap();
                std::process::exit(0);
            }
            "close-streams-before-exit" => close_streams_and_exit(Duration::from_secs(1)),
            "close-streams-before-deadline" => close_streams_and_exit(Duration::from_secs(15)),
            _ => panic!("unexpected fixture mode"),
        }
    }

    #[cfg(unix)]
    fn close_streams_and_exit(delay: Duration) -> ! {
        io::stdout().flush().unwrap();
        io::stderr().flush().unwrap();
        // SAFETY: only this disposable subprocess closes its inherited response descriptors.
        unsafe {
            assert_eq!(libc::close(libc::STDOUT_FILENO), 0);
            assert_eq!(libc::close(libc::STDERR_FILENO), 0);
        }
        thread::sleep(delay);
        // SAFETY: bypass stdio cleanup; the response descriptors are already closed.
        unsafe { libc::_exit(EOF_FIXTURE_EXIT_CODE) }
    }

    #[cfg(windows)]
    fn close_streams_and_exit(delay: Duration) -> ! {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::ExitProcess;

        io::stdout().flush().unwrap();
        io::stderr().flush().unwrap();
        let stdout = io::stdout().as_raw_handle();
        let stderr = io::stderr().as_raw_handle();
        assert!(!stdout.is_null() && stdout != INVALID_HANDLE_VALUE);
        assert!(!stderr.is_null() && stderr != INVALID_HANDLE_VALUE);
        assert_ne!(stdout, stderr);
        // SAFETY: this disposable subprocess has two distinct inherited pipe writers. Close
        // them once and do not use Rust stdio again; ExitProcess bypasses Rust stdio cleanup.
        unsafe {
            assert_ne!(CloseHandle(stdout), 0);
            assert_ne!(CloseHandle(stderr), 0);
        }
        thread::sleep(delay);
        // SAFETY: this terminates only the fixture process.
        unsafe { ExitProcess(EOF_FIXTURE_EXIT_CODE as u32) }
    }

    #[test]
    fn child_backoff_doubles_to_five_milliseconds_and_respects_remaining_time() {
        let mut backoff = INITIAL_CHILD_BACKOFF;
        for micros in [200, 400, 800, 1600, 3200, 5000, 5000] {
            assert_eq!(
                next_child_delay(&mut backoff, Duration::from_secs(1)),
                Duration::from_micros(micros)
            );
        }

        for initial in [INITIAL_CHILD_BACKOFF, MAX_CHILD_BACKOFF] {
            for remaining in [Duration::ZERO, Duration::from_nanos(1), initial] {
                let mut backoff = initial;
                assert_eq!(next_child_delay(&mut backoff, remaining), remaining);
            }
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

        let error = capture_command(
            fixture_command("stderr-overflow"),
            4096,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.to_string().contains("stderr exceeded 4096 bytes"));
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

    fn capture_after_observed_eof(
        mode: &str,
        timeout: Duration,
    ) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let command = fixture_command(mode);
        let (mut child, mut pipes) = crate::download::spawn_response(command).unwrap();
        let setup_deadline = Instant::now() + Duration::from_secs(5);
        let mut buffer = [0; 1024];
        let mut observe_eof = || -> io::Result<Option<ExitStatus>> {
            // Drain libtest's banner and verify EOF on both real pipes while the child is alive.
            // EOF remains observable when capture_response reads the pipes again.
            while pipes.read_stdout(&mut buffer, setup_deadline)? != 0 {}
            while pipes.read_stderr(&mut buffer, setup_deadline)? != 0 {}
            child.try_wait()
        };
        let observed = observe_eof();
        if !matches!(observed, Ok(None)) {
            stop_child(&mut child);
            panic!("expected EOF on both streams before exit, got {observed:?}");
        }
        let deadline = Instant::now() + timeout;
        let result = capture_response(&mut child, &mut pipes, 1024, deadline, timeout);
        assert_child_reaped(&child);
        if result.is_err() {
            // Only inspect the exit code after the OS probe has verified capture did not
            // leave a live or waitable child behind.
            assert_ne!(child.wait().unwrap().code(), Some(EOF_FIXTURE_EXIT_CODE));
        }
        result
    }

    #[cfg(unix)]
    fn assert_child_reaped(child: &Child) {
        let pid = child.id() as libc::pid_t;
        // SAFETY: probe this direct child without blocking or writing an exit status. A zombie
        // would still be waitable here; do not use Child::wait for the assertion.
        let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        let error = io::Error::last_os_error();
        assert_eq!(waited, -1, "capture left a live or waitable child");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }

    #[cfg(windows)]
    fn assert_child_reaped(child: &Child) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: Child still owns the process handle. A zero-time wait verifies termination
        // without waiting for a live process; dropping Child releases the handle on Windows.
        assert_eq!(
            unsafe { WaitForSingleObject(child.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn eof_before_direct_child_exit_returns_reaped_status() {
        let mode = "close-streams-before-exit";
        let (status, _, _) = capture_after_observed_eof(mode, Duration::from_secs(5)).unwrap();
        assert_eq!(status.code(), Some(EOF_FIXTURE_EXIT_CODE));
    }

    #[test]
    fn eof_before_deadline_cancels_and_reaps_the_direct_child() {
        let mode = "close-streams-before-deadline";
        let error = capture_after_observed_eof(mode, Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error
            .to_string()
            .contains("timed out after 300 milliseconds"));
    }
}
