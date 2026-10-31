//! Sign-ins without the harnesses or `gh` on this machine. Setup boots a short-lived agent of the
//! base image, which has them all, relays the sign-in program's terminal here, and opens its
//! sign-in page in this machine's browser. The credential comes back to the host, where agentd
//! keeps it as it keeps any login; the agent being set up only ever sees placeholders.

use super::answers::{Answers, Choice};
use super::github::record_github;
use super::{BrowserSignIn, Draft, SetupArguments, SignInChoice, staging};
use crate::{CommandError, CommandResult};

use std::{collections::BTreeSet, io::Write as _, path::Path, time::Duration};

use agent::{
    Agent, Error, Harness, HarnessAuthMode, control_api::Client, control_plane::ApplyRequest,
    control_plane::WaitPolicy, harness::SandboxSignIn, local::home::ControlPlaneHome, sandbox::ExecutionTarget,
};
use bytes::Bytes;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use sandbox::{
    execution::ExecutionEvent,
    terminal::{StartTerminalExecutionRequest, TerminalEvent, TerminalSize},
};
use zeroize::Zeroizing;

/// The names of the short-lived sign-in agents, which `get` and the TUI leave out.
pub(crate) const SIGN_IN_AGENT_PREFIX: &str = "agentctl-signin-";

/// The image of the sign-in agent: the base every agent image builds on, with every harness and
/// `gh` in it.
const SIGN_IN_IMAGE: &str = "ghcr.io/digdir/agentctl-base:ubuntu-26.04";

/// Whether `name` is one of setup's short-lived sign-in agents.
pub(crate) fn is_sign_in_agent(name: &str) -> bool {
    name.starts_with(SIGN_IN_AGENT_PREFIX)
}

/// The logins the harnesses of `agent` cannot start without: its default harness, and the others
/// it does not mark optional.
pub(super) struct Logins {
    /// Those agentd does not hold.
    pub(super) missing: Vec<Harness>,
    /// Those agentd holds already.
    pub(super) stored: Vec<Harness>,
}

/// Which logins `agent` needs, and which of them agentd holds.
pub(super) async fn logins(client: &Client, agent: &Agent) -> Result<Logins, Error> {
    let ready: BTreeSet<Harness> = client
        .auth_status()
        .await?
        .into_iter()
        .filter(|status| status.ready)
        .map(|status| status.harness)
        .collect();
    let (stored, missing) = agent
        .spec
        .harnesses
        .iter()
        .filter(|harness| harness.auth == HarnessAuthMode::Mediated && (harness.default || !harness.optional))
        .map(|harness| harness.kind)
        .partition(|harness| ready.contains(harness));
    Ok(Logins { missing, stored })
}

/// A running sign-in agent, deleted by [`Self::finish`].
pub(super) struct SignInAgent<'a> {
    client: &'a Client,
    home: &'a ControlPlaneHome,
    name: String,
    target: ExecutionTarget,
}

impl<'a> SignInAgent<'a> {
    /// Creates the sign-in agent and waits until commands run in it. Its home is the empty
    /// `sign-in-home` in the run's staging folder, which also records its name, so a later run
    /// deletes it if this one is killed before it can.
    pub(super) async fn start(
        client: &'a Client,
        home: &'a ControlPlaneHome,
        staging: &super::staging::Staging,
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
    pub(super) async fn run(&self, sign_in: &SandboxSignIn) -> Result<Zeroizing<String>, Error> {
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
    pub(super) async fn finish(self) {
        if let Err(error) = self.client.delete(&self.name).await {
            eprintln!(
                "warning: could not delete the sign-in agent {}: {error}; `{} delete agent/{0}` does",
                self.name,
                super::command_line(self.home)
            );
        }
    }
}

pub(super) fn rand_suffix() -> u16 {
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

/// Decides which harnesses to sign in to: those with no login yet, and those whose login agentd
/// holds already when the person would rather sign in again, as when it stopped working.
pub(super) async fn choose_logins(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    draft: &mut Draft,
) -> CommandResult<()> {
    let Logins { missing, stored } = logins(client, &draft.agent).await?;
    if arguments.sign_in == Some(SignInChoice::Skip) {
        if !missing.is_empty() {
            let logins: Vec<_> = missing
                .iter()
                .map(|harness| format!("`agentctl {} login`", login_command(*harness)))
                .collect();
            eprintln!("warning: the agent waits until you run {}", logins.join(" and "));
        }
        draft.access.reused_logins = stored;
        return Ok(());
    }
    draft.access.sign_in_to = missing;
    for harness in stored {
        let again = match arguments.sign_in {
            Some(SignInChoice::Again) => true,
            Some(_) => false,
            None if !answers.interactive() => false,
            None => answers.choose(
                "--sign-in",
                &format!(
                    "{} is signed in already on this machine, for every agent. Use that login?",
                    harness.display_name()
                ),
                vec![
                    Choice::new(false, "Yes, use it").suggested(),
                    Choice::new(true, "No, sign in again"),
                ],
                0,
            )?,
        };
        if again {
            draft.access.sign_in_to.push(harness);
        } else {
            draft.access.reused_logins.push(harness);
        }
    }
    Ok(())
}

/// Signs in, in a short-lived sandbox, to the harnesses chosen for it, and to GitHub when the
/// browser was chosen.
pub(super) async fn sign_in_where_needed(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    home: &ControlPlaneHome,
    staging: &staging::Staging,
    draft: &mut Draft,
) -> CommandResult<()> {
    let harnesses = std::mem::take(&mut draft.access.sign_in_to);
    let github = draft.access.github_browser.take();
    if harnesses.is_empty() && github.is_none() {
        return Ok(());
    }
    if !answers.interactive() {
        let logins: Vec<_> = harnesses
            .iter()
            .map(|harness| format!("`agentctl {} login`", login_command(*harness)))
            .collect();
        return Err(CommandError::Message(format!(
            "signing in needs a terminal and a browser; run setup at a terminal, or {} first",
            if logins.is_empty() {
                "--github token".to_owned()
            } else {
                logins.join(" and ")
            }
        )));
    }
    let agent = SignInAgent::start(client, home, staging, arguments.timeout).await?;
    let signed_in = async {
        for harness in harnesses {
            let Some(recipe) = agent::harness::sandbox_sign_in(harness) else {
                continue;
            };
            println!(
                "\nSign in to {}. Its sign-in page opens in your browser.",
                harness.display_name()
            );
            let credential = agent.run(&recipe).await?;
            let stored = match client.auth_login(harness, credential.to_string(), false).await {
                Ok(stored) => stored,
                // A credential read off the sign-in's screen can come out garbled, while the person
                // still sees it whole: let them paste it. One read from a file has no such cause.
                Err(error) if recipe.artifact.is_none() => {
                    eprintln!(
                        "\nThe {} credential read from the screen was refused: {error}",
                        harness.display_name()
                    );
                    let pasted = answers.secret(
                        "--sign-in skip",
                        &["Copy the credential the sign-in printed above, all of it, and paste it here"],
                    )?;
                    let pasted: String = pasted.chars().filter(|character| !character.is_whitespace()).collect();
                    client.auth_login(harness, pasted, false).await?
                }
                Err(error) => return Err(error.into()),
            };
            println!("\r{} authentication stored", stored.provider);
        }
        if let Some(BrowserSignIn { account }) = github {
            println!("\nSign in to GitHub. Its device page opens in your browser; enter the code shown here.");
            let token = agent.run(&agent::harness::github::sign_in()).await?;
            record_github(arguments, answers, draft, token, account.as_deref()).await?;
        }
        Ok::<_, CommandError>(())
    }
    .await;
    agent.finish().await;
    signed_in
}

/// Deletes the sign-in agents of setup runs that ended before they could, such as one stopped with
/// Ctrl-C while its sign-in agent booted. Those of runs still going are left alone.
pub(super) async fn remove_dead_sign_in_agents(client: &Client, agents: &[Agent], leftovers: &staging::Leftovers) {
    for agent in agents {
        let name = &agent.metadata.name;
        let dead = is_sign_in_agent(name)
            && agent.metadata.deletion_timestamp.is_none()
            && !leftovers.live_sign_in_agents.contains(name);
        if dead && client.delete(name).await.is_ok() {
            eprintln!("Deleted {name}, left by a setup that did not finish.");
        }
    }
}

/// The `agentctl` subcommand that stores a harness's login on this machine.
pub(super) const fn login_command(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude",
        Harness::Codex => "codex",
        Harness::Pi => "pi",
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
