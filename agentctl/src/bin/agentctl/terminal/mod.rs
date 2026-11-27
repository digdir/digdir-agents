//! Relays this terminal to a terminal agentd serves, such as a session's or a sign-in's.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::Input;
#[cfg(windows)]
use windows::Input;

use std::{io::Write as _, time::Duration};

use agent::{
    Error,
    control_api::{Client, Ended, Output, RemoteTerminal},
    local::home::ControlPlaneHome,
    sandbox::TerminalSpec,
    sessions::SessionName,
};
use bytes::Bytes;
use sandbox::terminal::TerminalSize;

/// How long an attachment waits for agentd to come back after it stopped, such as for an upgrade, before it starts
/// agentd itself.
const REATTACH_WAIT: Duration = Duration::from_secs(30);

/// What this terminal does while it is relayed.
enum Local {
    /// Bytes typed or pasted.
    Input(Bytes),
    /// The terminal changed size.
    Resize(TerminalSize),
    /// The person is leaving, such as by closing the terminal window.
    Leave,
}

/// Attaches this terminal to the session `name` of `agent` until the person detaches or the session ends. When
/// agentd stops meanwhile, as it does for an upgrade, it attaches again once agentd is back.
///
/// # Errors
///
/// Returns an error when the session cannot be attached, ends with an error, or agentd does not come back.
pub(crate) async fn attach_session(
    home: &ControlPlaneHome,
    client: &Client,
    agent: &str,
    name: &SessionName,
) -> Result<(), Error> {
    loop {
        let (_session, terminal) = client.attach_session(agent, name.clone(), size()).await?;
        match relay(terminal, async |_| {}).await {
            Ok(Ended::Exited(0) | Ended::Detached) => return Ok(()),
            Ok(Ended::Exited(code)) => {
                return Err(Error::Session(format!(
                    "the session's terminal exited with code {code}"
                )));
            }
            Ok(ended) if ended.agentd_stopped() => {}
            Ok(Ended::Error(message)) => return Err(Error::Session(message)),
            // agentd went away without saying so, such as when it was killed.
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {}
            Err(error) => return Err(error),
        }
        wait_for_agentd(home, client).await?;
    }
}

/// Runs `command` in a terminal of its own in the agent `agent`'s sandbox, relayed to this one, and returns how it
/// ended.
///
/// # Errors
///
/// Returns an error when the command cannot start or the connection to agentd fails.
pub(crate) async fn run(client: &Client, agent: &str, command: Vec<String>) -> Result<Ended, Error> {
    let spec = TerminalSpec {
        command,
        ..TerminalSpec::default()
    };
    let terminal = client.open_terminal(agent, spec, size()).await?;
    relay(terminal, async |_| {}).await
}

/// Waits for agentd to answer again, saying so when it takes more than a moment, and starts it when it does not
/// come back by itself. An upgrade starts the new agentd itself, so this waits for that first.
async fn wait_for_agentd(home: &ControlPlaneHome, client: &Client) -> Result<(), Error> {
    let started = tokio::time::Instant::now();
    let mut told = false;
    while started.elapsed() < REATTACH_WAIT {
        if let Ok(daemon) = client.health().await {
            return daemon.require_compatible();
        }
        if !told && started.elapsed() >= Duration::from_secs(1) {
            eprintln!("agentd stopped. Attaching again once it is back.");
            told = true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    crate::ensure_daemon(home, client).await
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
