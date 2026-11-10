//! Sign-ins without their programs on this machine. A short-lived agent, on an image with the
//! sign-in's program, runs it with its terminal relayed here, and its sign-in page opens in this
//! machine's browser. The credential comes back to the caller, which has agentd keep
//! it; agents only ever see placeholders.

use std::{collections::BTreeSet, io::Write as _, path::Path, time::Duration};

use agent::{
    Error,
    control_api::{AgentLease, Client},
    control_plane::WaitPolicy,
    local::home::ControlPlaneHome,
    sandbox::ExecutionTarget,
    sign_in::SandboxSignIn,
};
use bytes::Bytes;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use sandbox::{
    execution::ExecutionEvent,
    terminal::{StartTerminalExecutionRequest, TerminalEvent, TerminalSize},
};
use zeroize::Zeroizing;

use crate::CommandResult;

/// How long a sign-in agent may take to start, which includes pulling its image the first time.
const START_TIMEOUT: Duration = Duration::from_mins(20);

/// A running sign-in agent. It is ephemeral, so agentd deletes it once this is dropped or the
/// process ends, however it ends.
struct SignInAgent<'a> {
    home: &'a ControlPlaneHome,
    target: ExecutionTarget,
    _lease: AgentLease,
}

impl<'a> SignInAgent<'a> {
    /// Runs the agent `sign_in` runs in, to sign in to `service`, and waits until commands run in
    /// it, as one line about getting the sign-in ready: the agent is how the sign-in runs, not what
    /// the person asked for.
    async fn start(
        client: &Client,
        home: &'a ControlPlaneHome,
        service: &str,
        sign_in: &SandboxSignIn,
    ) -> crate::CommandResult<Self> {
        let wait = crate::progress::Wait::summarized(
            format!("Getting ready to sign in to {service}"),
            format!("Ready to sign in to {service}"),
        );
        let (ran, lease) = client.run_sign_in(sign_in.definition).await?;
        let name = ran.agent.metadata.name;
        crate::converge_shown(&wait, client, &name, START_TIMEOUT).await?;
        let target = client.ensure_execution(&name, WaitPolicy::UntilConverged).await?;
        wait.ready();
        Ok(Self {
            home,
            target,
            _lease: lease,
        })
    }

    /// Runs `sign_in` with this terminal relayed to it, and returns the credential it leaves. With
    /// `open_links`, each sign-in page it shows opens in this machine's browser.
    async fn run(&self, sign_in: &SandboxSignIn, open_links: bool) -> Result<Zeroizing<String>, SignInFailure> {
        let before = |error: Error| SignInFailure {
            error: Box::new(error),
            signed_in: false,
        };
        let base = agent::sandbox::platform::execution_spec(&self.target.operating_system, &["/bin/true".into()], true)
            .map_err(before)?;
        let mut spec = sign_in.execution.clone().with_environment(base.environment().clone());
        if let Some(directory) = base.working_directory() {
            spec = spec.with_working_directory(directory.clone());
        }
        let output = relay(self.home.path(), &self.target, spec, open_links.then_some(sign_in))
            .await
            .map_err(before)?;
        // The program signed in, so whatever fails now leaves a credential the host never gets.
        let after = |error: Error| SignInFailure {
            error: Box::new(error),
            signed_in: true,
        };
        let artifact = match &sign_in.artifact {
            Some(path) => Some(self.read(path.as_str()).await.map_err(after)?),
            None => None,
        };
        sign_in
            .credential(&output, artifact.as_ref().map(|artifact| artifact.as_slice()))
            .map_err(after)
    }

    /// The contents of a file in the sign-in agent.
    async fn read(&self, path: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        let spec = agent::sandbox::platform::execution_spec(
            &self.target.operating_system,
            &["/bin/cat".into(), path.into()],
            false,
        )?;
        let mut execution = agent::sandbox::start_execution(self.home.path(), &self.target, spec).await?;
        let mut contents = Zeroizing::new(Vec::new());
        while let Some(event) = execution.events.next().await {
            match event? {
                ExecutionEvent::Stdout(bytes) => contents.extend_from_slice(&bytes),
                ExecutionEvent::Exited(status) if status.success() => return Ok(contents),
                ExecutionEvent::Exited(_) => {
                    return Err(Error::Invalid("the sign-in left no credential file".into()));
                }
                ExecutionEvent::Failed { message } => return Err(Error::Invalid(message)),
                _ => {}
            }
        }
        Err(Error::Invalid("the sign-in agent stopped answering".into()))
    }
}

/// Runs `spec` in a terminal relayed to this one until it exits, opening each of the sign-in pages
/// of `opens` it shows in this machine's browser, and returns everything it printed.
async fn relay(
    home: &Path,
    target: &ExecutionTarget,
    spec: sandbox::execution::ExecutionSpec,
    opens: Option<&SandboxSignIn>,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let size = crossterm::terminal::size()
        .ok()
        .and_then(|(columns, rows)| TerminalSize::new(rows, columns).ok())
        .unwrap_or(TerminalSize::DEFAULT);
    let started =
        agent::sandbox::start_terminal_execution(home, &target.sandbox, StartTerminalExecutionRequest::new(spec, size))
            .await?;
    let raw = RawMode::enable()?;
    let result = pump(started, opens).await;
    drop(raw);
    let (output, status) = result?;
    if !status.success() {
        return Err(Error::Invalid(format!(
            "the sign-in ended without signing in (exit code {})",
            status.code
        )));
    }
    Ok(output)
}

/// This terminal in raw mode, as the relayed program needs it, until dropped, which happens on a
/// panic too.
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

async fn pump(
    mut started: sandbox::terminal::StartedTerminalExecution,
    opens: Option<&SandboxSignIn>,
) -> Result<(Zeroizing<Vec<u8>>, sandbox::execution::ExitStatus), Error> {
    let mut output = Zeroizing::new(Vec::new());
    let mut opened = BTreeSet::new();
    let mut keys = EventStream::new();
    let mut stdout = std::io::stdout();
    loop {
        tokio::select! {
            event = started.events.next() => match event.transpose()? {
                Some(TerminalEvent::Output(bytes)) => {
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                    output.extend_from_slice(&bytes);
                    let links = opens.map(|sign_in| sign_in.links(&output)).unwrap_or_default();
                    for link in links {
                        if !opened.insert(link.to_string()) {
                            continue;
                        }
                        // Written among what the program draws, which it may draw over, but the
                        // person would otherwise wait for a page that never opens.
                        if let Err(error) = crate::launch::open_url(link.as_str()).await {
                            write!(stdout, "\r\n(The sign-in page did not open: {error}. Open the link in your browser.)\r\n")?;
                            stdout.flush()?;
                        }
                    }
                }
                Some(TerminalEvent::Exited(status)) => return Ok((output, status)),
                Some(TerminalEvent::Failed { message }) => return Err(Error::Invalid(message)),
                Some(_) => {}
                None => return Err(Error::Invalid("the sign-in's terminal closed".into())),
            },
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) => {
                    if let Some(bytes) = key_bytes(key) {
                        started.control.write_input(Bytes::from(bytes)).await?;
                    }
                }
                Some(Ok(Event::Paste(text))) => started.control.write_input(Bytes::from(text.into_bytes())).await?,
                Some(Ok(Event::Resize(columns, rows))) => {
                    if let Ok(size) = TerminalSize::new(rows, columns) {
                        started.control.resize(size).await.ok();
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => started.control.close_input().await?,
            },
        }
    }
}

/// The bytes a terminal sends for `key`, as the program on the other side expects them.
fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    // Windows reports a character typed with AltGr, such as `@` on a Norwegian keyboard, as one
    // with Control and Alt held.
    let control = key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
    let alt = key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL);
    let bytes = match key.code {
        KeyCode::Char(character) if control => {
            let byte = u8::try_from(character.to_ascii_lowercase()).ok()?;
            vec![byte & 0x1f]
        }
        KeyCode::Char(character) if alt => format!("\x1b{character}").into_bytes(),
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// Why a sign-in gave no credential.
pub(crate) struct SignInFailure {
    pub(crate) error: Box<Error>,
    /// Whether the program had signed in, so that the service may have issued a credential this
    /// machine never got.
    pub(crate) signed_in: bool,
}

/// Runs `sign_in` in a short-lived agent to sign in to `service`, with the person at this terminal
/// and its sign-in page opening in this machine's browser. The outer result says whether the agent
/// started, the inner one what the sign-in gave. The agent is deleted, with everything signed in
/// inside it, before this returns.
pub(crate) async fn sign_in(
    client: &Client,
    home: &ControlPlaneHome,
    service: &str,
    sign_in: &SandboxSignIn,
) -> CommandResult<Result<Zeroizing<String>, SignInFailure>> {
    let agent = SignInAgent::start(client, home, service, sign_in).await?;
    // Where a browser here would not reach the person, such as over SSH, they open the link. Saying
    // so now keeps it out of what the program draws.
    let blocked = crate::launch::launch_blocked(|name| std::env::var_os(name));
    match &blocked {
        None => println!("Its sign-in page opens in your browser."),
        Some(reason) => println!("No browser here would reach you: {reason}. Open the link it shows in a browser."),
    }
    Ok(agent.run(sign_in, blocked.is_none()).await)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::key_bytes;

    #[test]
    fn keys_reach_the_program_as_a_terminal_sends_them() {
        let press = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            key_bytes(press(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(b"a".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            key_bytes(press(KeyCode::Enter, KeyModifiers::NONE)),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT)),
            Some(b"@".to_vec()),
            "AltGr on Windows types the character"
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Delete, KeyModifiers::NONE)),
            Some(b"\x1b[3~".to_vec())
        );
        let mut release = press(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_bytes(release), None);
    }
}
