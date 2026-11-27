use std::fs::OpenOptions;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::{Child, Command, Stdio};
use std::ptr::null;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

pub(super) struct Pipe {
    reader: OwnedHandle,
    event: OwnedHandle,
}

impl Pipe {
    pub(super) fn spawn(mut command: Command) -> io::Result<(Child, Self)> {
        let (pipe, writer) = Self::pair()?;
        let child = command.stdout(Stdio::from(writer)).spawn()?;
        drop(command);
        Ok((child, pipe))
    }

    pub(super) fn spawn_both(mut command: Command) -> io::Result<(Child, Self, Self)> {
        let (stdout, stdout_writer) = Self::pair()?;
        let (stderr, stderr_writer) = Self::pair()?;
        let child = command
            .stdout(Stdio::from(stdout_writer))
            .stderr(Stdio::from(stderr_writer))
            .spawn()?;
        drop(command);
        Ok((child, stdout, stderr))
    }

    fn pair() -> io::Result<(Self, std::fs::File)> {
        static NEXT_PIPE: AtomicU32 = AtomicU32::new(0);
        let sequence = NEXT_PIPE.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let path = format!(
            r"\\.\pipe\herdr-lazy-{}-{}-{sequence}",
            std::process::id(),
            now.as_nanos()
        );
        let wide: Vec<_> = path.encode_utf16().chain([0]).collect();
        // SAFETY: the name is NUL-terminated. With default security attributes the
        // reader is non-inheritable. Reject remote clients and refuse a preexisting pipe.
        let raw = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                8192,
                0,
                null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: we own the newly created handle.
        let reader = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: no security attributes or event name are supplied.
        let raw_event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if raw_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: we own the newly created event.
        let event = unsafe { OwnedHandle::from_raw_handle(raw_event) };
        // Opening the writer connects it to the server end. The caller moves it into one
        // command stream, leaving the parent with only the non-inheritable reader.
        let writer = OpenOptions::new().write(true).open(path)?;
        Ok((Self { reader, event }, writer))
    }

    pub(super) fn read(&mut self, buffer: &mut [u8], deadline: Instant) -> io::Result<usize> {
        super::remaining(deadline)?;
        let reader = self.reader.as_raw_handle();
        let event = self.event.as_raw_handle();
        // SAFETY: self owns the manual-reset event and no operation is pending.
        if unsafe { ResetEvent(event) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut operation = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        let operation = std::ptr::addr_of_mut!(operation);
        // SAFETY: the live pipe is overlapped. Both the buffer and the stack
        // operation remain live until the read finishes or cancellation is drained.
        let started = unsafe {
            ReadFile(
                reader,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                operation,
            )
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return eof_or(error);
            }
            let wait = super::remaining(deadline).map(|duration| {
                (duration.as_nanos().div_ceil(1_000_000)).min(u128::from(u32::MAX - 1)) as u32
            });
            // SAFETY: the event handle remains owned for the duration of the wait.
            let waited = wait.map(|timeout| unsafe { WaitForSingleObject(event, timeout) });
            if !matches!(waited, Ok(WAIT_OBJECT_0)) {
                let wait_error = match waited {
                    Ok(WAIT_TIMEOUT) | Err(_) => io::ErrorKind::TimedOut.into(),
                    _ => io::Error::last_os_error(),
                };
                // SAFETY: request cancellation for this operation only. The operation
                // may already have completed. Wait for its final status in either case
                // before allowing the stack operation or its buffer to be dropped.
                unsafe {
                    CancelIoEx(reader, operation);
                }
                let mut transferred = 0;
                // SAFETY: cancellation has been requested for this operation and the wait keeps
                // its stack state and buffer alive until Windows publishes the final result.
                if unsafe { GetOverlappedResult(reader, operation, &mut transferred, 1) } != 0 {
                    // The read won the race with cancellation. Those bytes belong to this slice;
                    // returning the earlier timeout would silently discard them.
                    return Ok(transferred as usize);
                }
                let completion_error = io::Error::last_os_error();
                return match completion_error.raw_os_error().map(|code| code as u32) {
                    Some(ERROR_OPERATION_ABORTED) => Err(wait_error),
                    Some(ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF) => Ok(0),
                    _ => Err(completion_error),
                };
            }
        }
        let mut transferred = 0;
        // SAFETY: the operation has completed and all pointers remain live.
        if unsafe { GetOverlappedResult(reader, operation, &mut transferred, 0) } == 0 {
            return eof_or(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }
}

fn eof_or(error: io::Error) -> io::Result<usize> {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF) => Ok(0),
        _ => Err(error),
    }
}
