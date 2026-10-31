//! Sign-ins without the harnesses or `gh` on this machine. A short-lived agent of the base image,
//! which has them all, runs the sign-in program with its terminal relayed here, and its sign-in
//! page opens in this machine's browser. The credential comes back to the host, where agentd keeps
//! it as it keeps any login; agents only ever see placeholders.

mod staging;

pub(crate) use staging::{Leftovers, Staging, sweep};

use std::{collections::BTreeSet, io::Write as _, path::Path, time::Duration};

use agent::{
    Agent, Error, Harness,
    control_api::Client,
    control_plane::ApplyRequest,
    control_plane::WaitPolicy,
    harness::{LoginName, SandboxSignIn},
    local::home::ControlPlaneHome,
    sandbox::ExecutionTarget,
};
use bytes::Bytes;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use sandbox::{
    execution::ExecutionEvent,
    terminal::{StartTerminalExecutionRequest, TerminalEvent, TerminalSize},
};
use zeroize::Zeroizing;

use crate::{CommandError, CommandResult};

/// The names of the short-lived sign-in agents, which `get` and the TUI leave out.
pub(crate) const SIGN_IN_AGENT_PREFIX: &str = "agentctl-signin-";

/// How long a sign-in agent may take to start, which includes pulling its image the first time.
pub(crate) const START_TIMEOUT: Duration = Duration::from_mins(20);

/// The image of the sign-in agent: the base every agent image builds on, with every harness and
/// `gh` in it.
const SIGN_IN_IMAGE: &str = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";

/// Whether `name` is one of the short-lived sign-in agents.
pub(crate) fn is_sign_in_agent(name: &str) -> bool {
    name.starts_with(SIGN_IN_AGENT_PREFIX)
}

/// A running sign-in agent, deleted by [`Self::finish`].
pub(crate) struct SignInAgent<'a> {
    client: &'a Client,
    home: &'a ControlPlaneHome,
    name: String,
    target: ExecutionTarget,
}

impl<'a> SignInAgent<'a> {
    /// Creates the sign-in agent and waits until commands run in it. Its home is the empty
    /// `sign-in-home` in the run's folder, which also records its name, so a later run deletes it
    /// if this one is killed before it can.
    pub(crate) async fn start(
        client: &'a Client,
        home: &'a ControlPlaneHome,
        staging: &Staging,
        timeout: Duration,
    ) -> crate::CommandResult<Self> {
        let name = format!("{SIGN_IN_AGENT_PREFIX}{:04x}", rand_suffix());
        staging.record_sign_in_agent(&name)?;
        let staging = staging.path();
        let agent_home = staging.join("sign-in-home");
        std::fs::create_dir_all(&agent_home).map_err(Error::from)?;
        let manifest = format!(
            "apiVersion: agents.platform/v1alpha1\n\
             kind: Agent\n\
             metadata: {{name: {name}}}\n\
             spec:\n  \
               sandbox:\n    \
                 image: {{type: reference, reference: {SIGN_IN_IMAGE}}}\n    \
                 platform: {{os: linux}}\n    \
                 initSystem: image\n    \
                 resources: {{cpu: '2', memory: 2Gi, rootFilesystem: {{capacity: 16Gi, mode: layered}}}}\n  \
               home: {{source: sign-in-home}}\n  \
               harnesses: [{{type: pi}}]\n  \
               network: {{mode: mediated, allow: all}}\n"
        );
        let agent = agent::manifest::decode(manifest.as_bytes())?;
        eprintln!("Starting a short-lived agent to sign in from; it is deleted afterwards.");
        client
            .apply(ApplyRequest {
                source_directory: staging.to_path_buf(),
                manifest_path: None,
                env_file: None,
                create_only: true,
                agent,
            })
            .await?;
        let started = async {
            crate::wait_until_converged(client, &name, timeout).await?;
            Ok::<_, crate::CommandError>(client.ensure_execution(&name, WaitPolicy::UntilConverged).await?)
        }
        .await;
        match started {
            Ok(target) => Ok(Self {
                client,
                home,
                name,
                target,
            }),
            Err(error) => {
                client.delete(&name).await.ok();
                Err(error)
            }
        }
    }

    /// Runs `sign_in` with this terminal relayed to it, and returns the credential it leaves.
    pub(crate) async fn run(&self, sign_in: &SandboxSignIn) -> Result<Zeroizing<String>, Error> {
        let base =
            agent::sandbox::platform::execution_spec(&self.target.operating_system, &["/bin/true".into()], true)?;
        let mut spec = sign_in.execution.clone().with_environment(base.environment().clone());
        if let Some(directory) = base.working_directory() {
            spec = spec.with_working_directory(directory.clone());
        }
        let output = relay(self.home.path(), &self.target, spec).await?;
        let artifact = match &sign_in.artifact {
            Some(path) => Some(self.read(path.as_str()).await?),
            None => None,
        };
        sign_in.credential(&output, artifact.as_ref().map(|artifact| artifact.as_slice()))
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

    /// Deletes the sign-in agent, and everything signed in inside it.
    pub(crate) async fn finish(self) {
        if let Err(error) = self.client.delete(&self.name).await {
            eprintln!(
                "warning: could not delete the sign-in agent {}: {error}; `{} delete agent/{0}` does",
                self.name,
                crate::command_line(self.home)
            );
        }
    }
}

fn rand_suffix() -> u16 {
    use rand_core::RngCore as _;
    let mut bytes = [0; 2];
    rand_core::OsRng.fill_bytes(&mut bytes);
    u16::from_le_bytes(bytes)
}

/// Runs `spec` in a terminal relayed to this one until it exits, opening each sign-in page it
/// shows in this machine's browser, and returns everything it printed.
async fn relay(
    home: &Path,
    target: &ExecutionTarget,
    spec: sandbox::execution::ExecutionSpec,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let size = crossterm::terminal::size()
        .ok()
        .and_then(|(columns, rows)| TerminalSize::new(rows, columns).ok())
        .unwrap_or(TerminalSize::DEFAULT);
    let started =
        agent::sandbox::start_terminal_execution(home, &target.sandbox, StartTerminalExecutionRequest::new(spec, size))
            .await?;
    crossterm::terminal::enable_raw_mode()?;
    let result = pump(started).await;
    crossterm::terminal::disable_raw_mode().ok();
    // The program may have hidden the cursor or turned on modes this terminal keeps after it.
    print!("\x1b[?25h\x1b[?2004l\x1b[0m");
    std::io::stdout().flush().ok();
    let (output, status) = result?;
    if !status.success() {
        return Err(Error::Invalid(format!(
            "the sign-in ended without signing in ({})",
            status.code
        )));
    }
    Ok(output)
}

async fn pump(
    mut started: sandbox::terminal::StartedTerminalExecution,
) -> Result<(Zeroizing<Vec<u8>>, sandbox::execution::ExitStatus), Error> {
    let mut output = Zeroizing::new(Vec::new());
    let mut opened = BTreeSet::new();
    let mut copied = false;
    let mut keys = EventStream::new();
    let mut stdout = std::io::stdout();
    loop {
        tokio::select! {
            event = started.events.next() => match event.transpose()? {
                Some(TerminalEvent::Output(bytes)) => {
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                    output.extend_from_slice(&bytes);
                    for link in agent::harness::sign_in_links(&output) {
                        if !opened.insert(link.to_string()) {
                            continue;
                        }
                        // A GitHub sign-in shows the code to enter on its page before the link,
                        // so it is on the clipboard here once the page opens.
                        if !copied && let Some(device) = agent::harness::github::device_code(&output) {
                            copied = true;
                            copy_code(&mut stdout, &device.code).await?;
                        }
                        crate::launch::open_url(link.as_str()).await.ok();
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

/// Puts a sign-in's one-time `code` on this machine's clipboard, and says whether it did.
async fn copy_code(stdout: &mut std::io::Stdout, code: &str) -> std::io::Result<()> {
    let note = match crate::launch::copy_to_clipboard(code).await {
        Ok(program) => format!("agentctl copied the code to your clipboard with {program}"),
        Err(reason) => format!("agentctl did not copy the code to your clipboard: {reason}"),
    };
    write!(stdout, "\r\n({note})\r\n")?;
    stdout.flush()
}

/// The bytes a terminal sends for `key`, as the program on the other side expects them.
fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let bytes = match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let byte = u8::try_from(character.to_ascii_lowercase()).ok()?;
            vec![byte & 0x1f]
        }
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// Deletes the sign-in agents of runs that ended before they could, such as one stopped with
/// Ctrl-C while its sign-in agent booted. Those of runs still going are left alone.
pub(crate) async fn remove_dead_sign_in_agents(client: &Client, agents: &[Agent], leftovers: &Leftovers) {
    for agent in agents {
        let name = &agent.metadata.name;
        let dead = is_sign_in_agent(name)
            && agent.metadata.deletion_timestamp.is_none()
            && !leftovers.live_sign_in_agents.contains(name);
        if dead && client.delete(name).await.is_ok() {
            eprintln!("Deleted {name}, left by a sign-in that did not finish.");
        }
    }
}

/// Signs in to `harness` in a short-lived agent and stores the login as `login`, for a machine
/// without the harness's own command. Leftover sign-in agents of runs that died go first.
pub(crate) async fn store_login(
    client: &Client,
    home: &ControlPlaneHome,
    harness: Harness,
    login: LoginName,
    timeout: Duration,
) -> CommandResult<agent::harness::ImportedAuthentication> {
    let Some(recipe) = agent::harness::sandbox_sign_in(harness) else {
        return Err(Error::Invalid(format!("{} has no login to sign in to", harness.display_name())).into());
    };
    if !crate::prompt::interactive() {
        return Err(CommandError::Message(format!(
            "{} is not installed here, and signing in without it needs a terminal and a browser",
            harness.display_name()
        )));
    }
    home.prepare()?;
    let leftovers = sweep(home.path());
    let staging = Staging::create(home.path())?;
    remove_dead_sign_in_agents(client, &client.list_agents().await?, &leftovers).await;
    let agent = SignInAgent::start(client, home, &staging, timeout).await?;
    println!(
        "\nSign in to {}. Its sign-in page opens in your browser.",
        harness.display_name()
    );
    let credential = agent.run(&recipe).await;
    agent.finish().await;
    let stored = client
        .auth_login(harness, login.clone(), credential?.to_string(), false)
        .await;
    match stored {
        Ok(stored) => Ok(stored),
        // A credential read off the sign-in's screen can come out garbled while the person still
        // sees it whole, so they can paste it instead.
        Err(error) if recipe.artifact.is_none() => Err(CommandError::Message(format!(
            "the {} credential read from the screen was refused: {error}; paste the one it printed into {} \
             with --from-stdin",
            harness.display_name(),
            agent::harness::login_command(harness, &login)
        ))),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::{is_sign_in_agent, key_bytes};

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
        let mut release = press(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_bytes(release), None);
    }

    #[test]
    fn sign_in_agents_are_known_by_their_prefix() {
        assert!(is_sign_in_agent("agentctl-signin-1a2b"));
        assert!(!is_sign_in_agent("dotnet-backend"));
    }
}
