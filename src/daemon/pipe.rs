//! A daemon session that is a program on pipes, not a shell on a pty: the
//! host of an ACP agent. It is a [`ByteHandle`] like a pty, so the daemon
//! relays it, holds it back for a slow window and ends it the same way.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::process::{Child, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use gpui_cn::terminal::{ByteHandle, ByteSink, ExitStatus, frame::Viewport};

use super::proto::Spec;

pub struct Pipe {
    pid: u32,
    /// None when adopted from another daemon: not this one's child.
    child: Mutex<Option<Child>>,
    input: mpsc::Sender<Vec<u8>>,
    /// Kept for a handover: the program's standard input and output.
    fds: (RawFd, RawFd),
}

impl Pipe {
    pub fn start(spec: &Spec, sink: ByteSink) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        let (program, args) = spec.program.split_first().ok_or_else(|| std::io::Error::other("no program"))?;
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: setsid only changes the child's session: the daemon ends
        // it, and all it started, by its group.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
        let (input, output) = (OwnedFd::from(stdin), OwnedFd::from(stdout));
        Ok(Self::with(child.id(), Some(child), input, output, sink))
    }

    /// A pipe session another daemon handed over.
    pub fn adopt(pid: u32, input: OwnedFd, output: OwnedFd, sink: ByteSink) -> Self {
        Self::with(pid, None, input, output, sink)
    }

    fn with(pid: u32, child: Option<Child>, input: OwnedFd, output: OwnedFd, sink: ByteSink) -> Self {
        let fds = (input.as_raw_fd(), output.as_raw_fd());
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut input = std::fs::File::from(input);
            while let Ok(bytes) = rx.recv() {
                if input.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut output = std::fs::File::from(output);
            let mut buf = vec![0u8; 64 << 10];
            loop {
                if !sink.wait_for_room() {
                    break;
                }
                match output.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if !sink.write(&buf[..n]) {
                            break;
                        }
                    }
                }
            }
            sink.finished();
        });
        Self { pid, child: Mutex::new(child), input: tx, fds }
    }

    /// The program's pid, and copies of its input and output for another daemon.
    pub fn handover(&self) -> Option<(u32, OwnedFd, OwnedFd)> {
        Some((self.pid, copy(self.fds.0)?, copy(self.fds.1)?))
    }
}

/// A copy of a descriptor that no program started from here gets: a plain
/// `dup` is open in the daemon's successor, and in every shell and agent,
/// for as long as they run.
pub fn copy(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC returns a new descriptor this side owns.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    (copy >= 0).then(|| unsafe { OwnedFd::from_raw_fd(copy) })
}

/// What the daemon holds as the session's handle; the session keeps the
/// pipe too, for a handover.
pub struct Handle(pub std::sync::Arc<Pipe>);

impl ByteHandle for Handle {
    fn write(&self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.input.send(bytes.to_vec()).map_err(|_| std::io::Error::other("the program is gone"))?;
        Ok(bytes.len())
    }

    fn resize(&self, _: Viewport) -> std::io::Result<()> {
        Ok(())
    }

    fn try_wait(&self) -> std::io::Result<Option<ExitStatus>> {
        let mut child = self.0.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match child.as_mut() {
            Some(child) => Ok(child.try_wait()?.map(|status| {
                use std::os::unix::process::ExitStatusExt as _;
                ExitStatus::new(status.code(), status.signal().map(|s| format!("signal {s}")))
            })),
            // SAFETY: signal 0 only asks whether the process is there.
            None if unsafe { libc::kill(self.0.pid as i32, 0) } != 0 => Ok(Some(ExitStatus::new(None, None::<String>))),
            None => Ok(None),
        }
    }

    fn close(&self) {
        let pid = self.0.pid as i32;
        // SAFETY: signals the program's own process group, made by setsid.
        unsafe { libc::kill(-pid, libc::SIGTERM) };
        let child = self.0.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            if let Some(mut child) = child {
                let _ = child.wait();
            }
        });
    }
}
