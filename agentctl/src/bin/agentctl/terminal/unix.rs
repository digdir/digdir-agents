//! This terminal's input on Unix: its bytes as they are, so mouse reports, keys with modifiers and pastes reach the
//! program unchanged.

use std::{fs::File, io::Read as _, os::fd::AsFd as _};

use agent::Error;
use bytes::Bytes;
use rustix::fs::{Mode, OFlags};
use tokio::{
    io::unix::AsyncFd,
    signal::unix::{Signal, SignalKind, signal},
};

use super::Local;

/// The most bytes one read takes, which is more than a paste usually sends at once.
const READ_SIZE: usize = 16 * 1024;

/// This terminal's input and size changes.
pub(super) struct Input {
    tty: AsyncFd<File>,
    buffer: Vec<u8>,
    resized: Signal,
}

impl Input {
    /// Opens this terminal for reading without blocking.
    ///
    /// It opens the terminal again rather than reading standard input, for two reasons. A blocking read of standard
    /// input would keep the runtime from shutting down once the relay ends. And a new open file is non-blocking on
    /// its own, while standard input, which the shell shares, stays as it was.
    #[expect(
        unsafe_code,
        reason = "tokio registers a file descriptor for readiness only through AsyncFd::register, which is unsafe"
    )]
    pub(super) fn open() -> Result<Self, Error> {
        let stdin = std::io::stdin();
        if !rustix::termios::isatty(stdin.as_fd()) {
            return Err(Error::Invalid("this requires an interactive local terminal".into()));
        }
        let path = rustix::termios::ttyname(stdin.as_fd(), Vec::new()).map_err(std::io::Error::from)?;
        let tty = rustix::fs::open(
            path.as_c_str(),
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        // SAFETY: the `File` owns the descriptor it was opened with, and nothing replaces or closes it while the
        // `AsyncFd` holds it, which only reads through `get_ref`.
        let tty = unsafe { AsyncFd::register(File::from(tty)) }.map_err(std::io::Error::from)?;
        Ok(Self {
            tty,
            buffer: vec![0; READ_SIZE],
            resized: signal(SignalKind::window_change())?,
        })
    }

    /// What the terminal does next, or `None` when its input ends. Dropping the call before it completes loses
    /// nothing.
    pub(super) async fn next(&mut self) -> Result<Option<Local>, Error> {
        loop {
            tokio::select! {
                readable = self.tty.readable() => {
                    let mut ready = readable?;
                    match ready.try_io(|tty| tty.get_ref().read(&mut self.buffer)) {
                        Ok(Ok(0)) => return Ok(None),
                        Ok(Ok(read)) => return Ok(Some(Local::Input(Bytes::copy_from_slice(&self.buffer[..read])))),
                        Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Ok(Err(error)) => return Err(error.into()),
                        Err(_would_block) => {}
                    }
                }
                _ = self.resized.recv() => return Ok(Some(Local::Resize(super::size()))),
            }
        }
    }
}
