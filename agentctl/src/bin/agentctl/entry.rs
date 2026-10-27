//! What agentctl offers at a terminal when a command that uses the current directory's agent
//! finds none or finds it stopped: setting one up, starting it from its files or starting it; and
//! the manifest `apply` finds here when not given one. Without a terminal, nothing is asked and the
//! command behaves as it always has.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use agent::{
    Agent, AgentVariantName, Error, Harness, RunState, control_api::Client, local::home::ControlPlaneHome,
    sessions::Session,
};

use crate::{CommandError, CommandResult, Unapplied, init, prompt};

/// How long a start offered here may take, as `agentctl start` waits by default.
const START_TIMEOUT: Duration = Duration::from_mins(10);

/// What agentctl offers for the current directory's agent, as resolving it ended.
#[derive(Debug, PartialEq, Eq)]
enum Offer {
    /// Start the stopped agent here.
    Start(String),
    /// Set an agent up with `agentctl init`.
    Init,
    /// Start the agent whose manifest is here, in `agent.yaml` or `agents/<name>/agent.yaml`, but
    /// which agentd does not run; the manifest as found from the current directory.
    Apply(PathBuf),
}

/// The offer for `resolved`, the current directory's agent, to a command that uses it, with what
/// `unapplied` found here. Nothing is offered without a person at the terminal, nor for an agent
/// that is running, nor for an error other than finding none.
fn offer(resolved: &Result<Agent, Error>, interactive: bool, unapplied: &Unapplied) -> Option<Offer> {
    if !interactive {
        return None;
    }
    match resolved {
        Ok(agent) if agent.spec.is_stopped() => Some(Offer::Start(agent.metadata.name.clone())),
        Err(Error::Rpc(error)) if error.is_not_found() => Some(match unapplied {
            Unapplied::Written { manifest, .. } => Offer::Apply(manifest.clone()),
            Unapplied::Here => Offer::Apply(PathBuf::from(agent::manifest::MANIFEST_FILE)),
            Unapplied::None => Offer::Init,
        }),
        Ok(_) | Err(_) => None,
    }
}

/// What the offer says and asks, the question last.
fn question(offer: &Offer) -> [String; 2] {
    match offer {
        Offer::Start(name) => [
            format!("This directory has the agent {name:?}, which is stopped."),
            "Start it?".into(),
        ],
        Offer::Init => [
            "No agent is set up for this directory.".into(),
            "Set one up now with `agentctl init`?".into(),
        ],
        Offer::Apply(manifest) => [
            format!("The agent in {} isn't running.", manifest.display()),
            "Start it from its files?".into(),
        ],
    }
}

fn accepted(offer: &Offer) -> CommandResult<bool> {
    let [about, ask] = question(offer);
    Ok(prompt::confirm_lines(&[&about, &ask])?)
}

/// The agent a command goes on with, or how it ended instead.
pub(crate) enum Target {
    Agent(String),
    Done(ExitCode),
}

/// The current directory's agent, for a command given no agent. At a terminal, offers to set
/// one up when there is none, which ends the command with init's exit code, to start it from its
/// files when agentd does not run it, and to start it when it is stopped; declining any gives the
/// error the command gives without a terminal.
pub(crate) async fn current_agent(
    home: &ControlPlaneHome,
    client: &Client,
    variant: Option<AgentVariantName>,
) -> CommandResult<Target> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let unapplied = crate::unapplied(&directory);
    let resolved = client.resolve_agent_variant(directory.clone(), variant).await;
    match (offer(&resolved, prompt::interactive(), &unapplied), resolved) {
        (Some(offer @ Offer::Init), Err(error)) => {
            if accepted(&offer)? {
                return Box::pin(init::run(home, client)).await.map(Target::Done);
            }
            Err(crate::inference_error(error, &unapplied))
        }
        (Some(Offer::Apply(manifest)), Err(error)) => {
            if !accepted(&Offer::Apply(manifest.clone()))? {
                return Err(crate::inference_error(error, &unapplied));
            }
            match start_from_files(client, &directory.join(&manifest)).await {
                Ok(name) => Ok(Target::Agent(name)),
                Err(error) => {
                    eprintln!("{}", crate::error_text(&error));
                    Err(CommandError::Message(format!(
                        "the agent in {} was not started; `agentctl init` has your coding agent change it first",
                        manifest.display()
                    )))
                }
            }
        }
        (Some(offer @ Offer::Start(_)), Ok(agent)) => {
            let name = agent.metadata.name;
            if !accepted(&offer)? {
                return Err(Error::Stopped(name).into());
            }
            start(client, &name).await?;
            Ok(Target::Agent(name))
        }
        (_, Ok(agent)) => Ok(Target::Agent(agent.metadata.name)),
        (_, Err(error)) => Err(crate::inference_error(error, &unapplied)),
    }
}

/// Applies `manifest` and waits for the agent as `agentctl apply --wait` does, returning its name.
pub(crate) async fn start_from_files(client: &Client, manifest: &Path) -> CommandResult<String> {
    let request = crate::read_apply_request(manifest.to_path_buf(), None)?;
    let name = request.agent.metadata.name.clone();
    let applied = client
        .apply(request)
        .await
        .map_err(|error| crate::apply_error(error, &name))?;
    let name = applied.metadata.name;
    eprintln!("agent/{name} applied");
    crate::wait_until_converged(client, &name, START_TIMEOUT).await?;
    eprintln!("agent/{name} started");
    Ok(name)
}

async fn start(client: &Client, name: &str) -> CommandResult<()> {
    client.set_run_state(name, RunState::Running).await?;
    crate::wait_until_converged(client, name, START_TIMEOUT).await?;
    eprintln!("agent/{name} started");
    Ok(())
}

/// The manifest `agentctl apply` applies in `directory` when not given one and it has no
/// `agent.yaml`: at a terminal, the one found there, once confirmed, or the one chosen of several.
/// `None` without a terminal, or with nothing found, so `apply` refuses and names what it found.
pub(crate) fn manifest_to_apply(directory: &Path) -> CommandResult<Option<PathBuf>> {
    if !prompt::interactive() || directory.join(agent::manifest::MANIFEST_FILE).is_file() {
        return Ok(None);
    }
    let found: Vec<(PathBuf, Option<String>)> = crate::found_manifests(directory)
        .into_iter()
        .map(|path| {
            let name = agent::manifest::resolve(&path)
                .ok()
                .map(|resolved| resolved.agent.metadata.name);
            (path, name)
        })
        .collect();
    pick_manifest(&found, directory, prompt::confirm_lines, prompt::choose)
}

/// Of `found`, manifests with the agent each names, the one to apply: the only one once `confirm`
/// agrees, else the one `choose` picks by index. Paths are shown from `directory`.
fn pick_manifest(
    found: &[(PathBuf, Option<String>)],
    directory: &Path,
    confirm: impl FnOnce(&[&str]) -> Result<bool, Error>,
    choose: impl FnOnce(&str, &[&str]) -> Result<usize, Error>,
) -> CommandResult<Option<PathBuf>> {
    let shown = |(path, name): &(PathBuf, Option<String>)| {
        let path = path.strip_prefix(directory).unwrap_or(path).display();
        name.as_ref()
            .map_or_else(|| path.to_string(), |name| format!("{path}, the agent {name:?}"))
    };
    match found {
        [] => Ok(None),
        [only] => {
            let about = format!("No agent.yaml here, but there is {}.", shown(only));
            if confirm(&[&about, "Apply it?"])? {
                Ok(Some(only.0.clone()))
            } else {
                Err(CommandError::Message("nothing was applied".into()))
            }
        }
        several => {
            let options: Vec<String> = several.iter().map(shown).collect();
            let options: Vec<&str> = options.iter().map(String::as_str).collect();
            let index = choose("No agent.yaml here. Which manifest should be applied?", &options)?;
            Ok(several.get(index).map(|(path, _)| path.clone()))
        }
    }
}

/// The session `agentctl attach` opens when given none: the agent's only one, a new one when it
/// has none, or the one chosen at the terminal. A new one is named after its harness, `harness`
/// or else the agent's default.
pub(crate) async fn session(client: &Client, agent: &str, harness: Option<Harness>) -> CommandResult<String> {
    if !prompt::interactive() {
        return Err(CommandError::Message(
            "attach needs a terminal; name the session to attach to, for example `agentctl attach session/claude-1`"
                .into(),
        ));
    }
    let harness = match harness {
        Some(harness) => Some(harness),
        None => client
            .list_agents()
            .await?
            .into_iter()
            .find(|candidate| candidate.metadata.name == agent)
            .and_then(|found| found.spec.default_harness().map(|installation| installation.kind)),
    };
    let sessions = client.list_sessions(Some(agent)).await?;
    let (name, new) = choose_session(agent, harness, &sessions, prompt::choose)?;
    if new {
        eprintln!("Starting session/{name}; detach with ctrl-b d, and run `agentctl attach` here to return.");
    }
    Ok(name)
}

/// The session to open among `sessions` and whether it is new, named after `harness`; `ask` picks
/// among several by index, the index after them for a new one. Archived sessions are not offered,
/// but their names stay taken.
fn choose_session(
    agent: &str,
    harness: Option<Harness>,
    sessions: &[Session],
    ask: impl FnOnce(&str, &[&str]) -> Result<usize, Error>,
) -> Result<(String, bool), Error> {
    let active = sessions
        .iter()
        .filter(|session| !session.is_archived())
        .map(|session| session.name.as_str())
        .collect::<Vec<_>>();
    let chosen = match active.as_slice() {
        [] => None,
        [only] => Some(*only),
        several => {
            let mut options = several.to_vec();
            options.push("A new session");
            let index = ask(&format!("Which session of the agent {agent:?}?"), &options)?;
            several.get(index).copied()
        }
    };
    Ok(chosen.map_or_else(|| (free_name(harness, sessions), true), |name| (name.to_owned(), false)))
}

/// The first of `claude-1`, `claude-2` and so on that no session has, after `harness`, so a
/// terminal tab reads `agent/claude-1`.
fn free_name(harness: Option<Harness>, sessions: &[Session]) -> String {
    let prefix = match harness {
        Some(Harness::ClaudeCode) => "claude",
        Some(Harness::Codex) => "codex",
        Some(Harness::Pi) => "pi",
        None => "session",
    };
    // One more name than there are sessions always has a free one.
    (1..=sessions.len() + 1)
        .map(|number| format!("{prefix}-{number}"))
        .find(|name| !sessions.iter().any(|session| session.name.as_str() == name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use agent::control_api::ResponseError;

    use super::*;

    fn agent(name: &str, stopped: bool) -> Agent {
        let mut agent: Agent =
            serde_yaml_ng::from_str(include_str!("../../../tests/fixtures/agent.yaml")).expect("test manifest");
        agent.metadata.name = name.into();
        if stopped {
            agent.spec.run_state = Some(RunState::Stopped);
        }
        agent
    }

    /// agentd's codes for no agent here, and for several.
    const NOT_FOUND: i32 = -32004;
    const INVALID_PARAMS: i32 = -32602;

    fn rpc(code: i32) -> Result<Agent, Error> {
        Err(Error::Rpc(ResponseError {
            code,
            message: "from agentd".into(),
        }))
    }

    fn session(name: &str, archived: bool) -> Session {
        let mut session: Session = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-4000-8000-000000000002",
            "agentId": "00000000-0000-4000-8000-000000000001",
            "agent": "worker",
            "name": name,
            "harness": "pi",
            "createdAt": "2026-01-01T00:00:00Z",
        }))
        .expect("session");
        if archived {
            session.archived_at = Some(session.created_at);
        }
        session
    }

    #[test]
    fn commands_offer_to_set_up_a_missing_agent_or_start_a_stopped_one_and_nothing_else() {
        let not_found = rpc(NOT_FOUND);
        let several = rpc(INVALID_PARAMS);
        assert_eq!(offer(&Ok(agent("worker", false)), true, &Unapplied::None), None);
        assert_eq!(
            offer(&Ok(agent("worker", true)), true, &Unapplied::None),
            Some(Offer::Start("worker".into()))
        );
        assert_eq!(offer(&not_found, true, &Unapplied::None), Some(Offer::Init));
        assert_eq!(offer(&several, true, &Unapplied::None), None);
        assert_eq!(
            offer(
                &not_found,
                true,
                &Unapplied::Written {
                    name: "dev".into(),
                    manifest: "agents/dev/agent.yaml".into()
                }
            ),
            Some(Offer::Apply("agents/dev/agent.yaml".into()))
        );
        assert_eq!(
            offer(&not_found, true, &Unapplied::Here),
            Some(Offer::Apply("agent.yaml".into())),
            "a manifest beside you is started from too"
        );
        assert_eq!(
            question(&Offer::Apply("agents/dev/agent.yaml".into())),
            [
                "The agent in agents/dev/agent.yaml isn't running.".to_owned(),
                "Start it from its files?".to_owned()
            ]
        );
        assert_eq!(question(&Offer::Start("worker".into()))[1], "Start it?");
    }

    #[test]
    fn nothing_is_offered_without_a_terminal() {
        for resolved in [
            Ok(agent("worker", false)),
            Ok(agent("worker", true)),
            rpc(NOT_FOUND),
            rpc(INVALID_PARAMS),
        ] {
            assert_eq!(offer(&resolved, false, &Unapplied::None), None, "{resolved:?}");
        }
    }

    #[test]
    fn apply_asks_before_applying_a_manifest_it_found_and_which_of_several() {
        let directory = Path::new("/home/person");
        let notes = (directory.join("agents/notes/agent.yaml"), Some("notes".to_owned()));
        let broken = (directory.join("agents/broken/agent.yaml"), None);
        let never_choose = |_: &str, _: &[&str]| -> Result<usize, Error> { panic!("chose") };
        let never_confirm = |_: &[&str]| -> Result<bool, Error> { panic!("confirmed") };

        let mut asked = Vec::new();
        let picked = pick_manifest(
            std::slice::from_ref(&notes),
            directory,
            |lines| {
                asked = lines.iter().map(|line| (*line).to_owned()).collect();
                Ok(true)
            },
            never_choose,
        )
        .expect("confirmed");
        assert_eq!(picked, Some(notes.0.clone()));
        assert_eq!(
            asked,
            [
                "No agent.yaml here, but there is agents/notes/agent.yaml, the agent \"notes\".",
                "Apply it?"
            ]
        );
        let Err(CommandError::Message(declined)) =
            pick_manifest(std::slice::from_ref(&notes), directory, |_| Ok(false), never_choose)
        else {
            panic!("declining applies nothing");
        };
        assert_eq!(declined, "nothing was applied");

        let mut offered = Vec::new();
        let picked = pick_manifest(
            &[notes, broken.clone()],
            directory,
            never_confirm,
            |question, options| {
                assert_eq!(question, "No agent.yaml here. Which manifest should be applied?");
                offered = options.iter().map(|option| (*option).to_owned()).collect();
                Ok(1)
            },
        )
        .expect("chosen");
        assert_eq!(picked, Some(broken.0));
        assert_eq!(
            offered,
            [
                "agents/notes/agent.yaml, the agent \"notes\"",
                "agents/broken/agent.yaml"
            ]
        );
        assert_eq!(
            pick_manifest(&[], directory, never_confirm, never_choose).expect("nothing found"),
            None
        );
    }

    #[test]
    fn attach_opens_the_only_session_or_a_new_one_without_asking() {
        let never = |_: &str, _: &[&str]| -> Result<usize, Error> { panic!("asked") };
        let claude = Some(Harness::ClaudeCode);
        assert_eq!(
            choose_session("worker", claude, &[], never).expect("none"),
            ("claude-1".to_owned(), true)
        );
        assert_eq!(
            choose_session("worker", claude, &[session("main", false)], never).expect("one"),
            ("main".to_owned(), false)
        );
        // An archived session is not offered, and its name is not reused.
        assert_eq!(
            choose_session(
                "worker",
                claude,
                &[session("claude-1", true), session("main", false)],
                never
            )
            .expect("one active"),
            ("main".to_owned(), false)
        );
        assert_eq!(
            choose_session("worker", claude, &[session("claude-1", true)], never).expect("only archived"),
            ("claude-2".to_owned(), true)
        );
    }

    #[test]
    fn a_new_session_is_named_after_its_harness() {
        let never = |_: &str, _: &[&str]| -> Result<usize, Error> { panic!("asked") };
        for (harness, name) in [
            (Some(Harness::ClaudeCode), "claude-1"),
            (Some(Harness::Codex), "codex-1"),
            (Some(Harness::Pi), "pi-1"),
            (None, "session-1"),
        ] {
            assert_eq!(
                choose_session("worker", harness, &[], never).expect("none"),
                (name.to_owned(), true)
            );
        }
        // Another harness's numbering is its own.
        assert_eq!(
            choose_session("worker", Some(Harness::Codex), &[session("claude-1", true)], never).expect("codex"),
            ("codex-1".to_owned(), true)
        );
    }

    #[test]
    fn attach_asks_which_of_several_sessions_or_a_new_one() {
        let sessions = [
            session("claude-1", false),
            session("claude-3", true),
            session("claude-2", false),
        ];
        let mut asked = Vec::new();
        let claude = Some(Harness::ClaudeCode);
        let chosen = choose_session("worker", claude, &sessions, |question, options| {
            assert_eq!(question, "Which session of the agent \"worker\"?");
            asked = options.iter().map(|option| (*option).to_owned()).collect();
            Ok(1)
        })
        .expect("several");
        assert_eq!(asked, ["claude-1", "claude-2", "A new session"]);
        assert_eq!(chosen, ("claude-2".to_owned(), false));
        assert_eq!(
            choose_session("worker", claude, &sessions, |_, _| Ok(2)).expect("new"),
            ("claude-4".to_owned(), true)
        );
    }
}
