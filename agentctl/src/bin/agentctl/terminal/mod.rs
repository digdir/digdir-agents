//! Relays this terminal to a terminal agentd serves, such as a session's or a sign-in's.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::Input;
#[cfg(windows)]
use windows::Input;

use std::io::Write as _;

use agent::{
    Error,
    control_api::{Ended, Output, RemoteTerminal},
};
use bytes::Bytes;
use sandbox::terminal::TerminalSize;

/// What this terminal does while it is relayed.
enum Local {
    /// Bytes typed or pasted.
    Input(Bytes),
    /// The terminal changed size.
    Resize(TerminalSize),
    /// The person is leaving, such as by closing the terminal window.
    Leave,
}

/// This terminal's size, to start a relayed terminal with.
pub(crate) fn size() -> TerminalSize {
    crossterm::terminal::size()
        .ok()
        .and_then(|(columns, rows)| TerminalSize::new(rows, columns).ok())
        .unwrap_or(TerminalSize::DEFAULT)
}

/// Relays this terminal to `terminal` until agentd says it ended, and returns why. `on_output` sees each piece of
/// output after it is drawn.
///
/// Closing this terminal, or a `SIGTERM`, detaches, and agentd then puts the terminal back before it ends.
///
/// # Errors
///
/// Returns an error when this is not an interactive terminal, or the connection to agentd fails.
pub(crate) async fn relay(terminal: RemoteTerminal, mut on_output: impl AsyncFnMut(&[u8])) -> Result<Ended, Error> {
    let mut local = Input::open()?;
    let _raw = RawMode::enable()?;
    let (mut input, mut output) = terminal.split();
    let mut stdout = std::io::stdout();
    let mut leaving = false;
    loop {
        tokio::select! {
            piece = output.next() => match piece? {
                Output::Bytes(bytes) => {
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                    on_output(&bytes).await;
                }
                Output::Ended(ended) => return Ok(ended),
            },
            event = local.next(), if !leaving => match event? {
                Some(Local::Input(bytes)) => input.input(bytes).await?,
                Some(Local::Resize(size)) => input.resize(size).await?,
                Some(Local::Leave) | None => {
                    input.detach().await?;
                    leaving = true;
                }
            },
        }
    }
}

/// This terminal in raw mode, as the relayed program needs it, until dropped, which happens on a panic too.
struct RawMode;

impl RawMode {
    fn enable() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        crossterm::terminal::disable_raw_mode().ok();
        // The program may have hidden the cursor or turned on modes this terminal keeps after it.
        print!("\x1b[?25h\x1b[?2004l\x1b[0m");
        std::io::stdout().flush().ok();
    }
}
