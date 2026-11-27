use std::io::{self, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Instant;

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::Errno;

pub(super) struct Pipe(ChildStdout);

impl Pipe {
    pub(super) fn spawn(mut command: Command) -> io::Result<(Child, Self)> {
        let mut child = command.stdout(Stdio::piped()).spawn()?;
        let pipe = child.stdout.take().expect("stdout was piped");
        let configured =
            fcntl_getfl(&pipe).and_then(|flags| fcntl_setfl(&pipe, flags | OFlags::NONBLOCK));
        if let Err(error) = configured {
            drop(pipe);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok((child, Self(pipe)))
    }

    pub(super) fn read(&mut self, buffer: &mut [u8], deadline: Instant) -> io::Result<usize> {
        loop {
            super::remaining(deadline)?;
            match self.0.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }

            let timeout = Timespec::try_from(super::remaining(deadline)?)
                .map_err(|_| io::ErrorKind::InvalidInput)?;
            let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}
