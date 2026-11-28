use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::Errno;

trait PipeRead: Read + AsFd {}
impl<T: Read + AsFd> PipeRead for T {}

pub(super) struct Pipe(Box<dyn PipeRead + Send>);

impl AsFd for Pipe {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

fn set_nonblocking(pipe: &impl AsFd) -> Result<(), Errno> {
    fcntl_getfl(pipe).and_then(|flags| fcntl_setfl(pipe, flags | OFlags::NONBLOCK))
}

impl Pipe {
    pub(super) fn spawn(mut command: Command) -> io::Result<(Child, Self)> {
        let mut child = command.stdout(Stdio::piped()).spawn()?;
        let pipe = child.stdout.take().expect("stdout was piped");
        let configured = set_nonblocking(&pipe);
        if let Err(error) = configured {
            drop(pipe);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok((child, Self(Box::new(pipe))))
    }

    pub(super) fn spawn_both(mut command: Command) -> io::Result<(Child, Self, Self)> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        if let Err(error) = set_nonblocking(&stdout) {
            drop(stdout);
            drop(stderr);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        if let Err(error) = set_nonblocking(&stderr) {
            drop(stdout);
            drop(stderr);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok((child, Self(Box::new(stdout)), Self(Box::new(stderr))))
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
            let mut fds = [PollFd::new(&*self, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}
