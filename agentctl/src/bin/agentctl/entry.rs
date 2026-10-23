//! What agentctl offers at a terminal when it runs on its own, or when a command that uses the
//! current directory's agent finds none or finds it stopped: opening it, starting it or setting
//! one up. Without a terminal, nothing is asked and the command behaves as it always has.

use std::{process::ExitCode, time::Duration};

use agent::{
    Agent, AgentVariantName, Error, RunState, control_api::Client, local::home::ControlPlaneHome, sessions::Session,
};

use crate::{CommandError, CommandResult, init, prompt, tui};

/// How long a start offered here may take, as `agentctl start` waits by default.
const START_TIMEOUT: Duration = Duration::from_mins(10);

/// What agentctl offers for the current directory's agent, as resolving it ended.
#[derive(Debug, PartialEq, Eq)]
enum Offer {
    /// Open the TUI; with the agent here, or without a name when several are.
    Open(Option<String>),
    /// Start the stopped agent here.
    Start(String),
    /// Set an agent up with `agentctl init`.
    Init,
}

/// The offer for `resolved`, the current directory's agent, from bare `agentctl` (`bare`) or a
/// command that uses it. Nothing is offered without a person at the terminal, nor for a
/// command whose agent is running or ambiguous, nor for an error other than finding none.
fn offer(resolved: &Result<Agent, Error>, bare: bool, interactive: bool) -> Option<Offer> {
    if !interactive {
        return None;
    }
    match resolved {
        Ok(agent) if agent.spec.is_stopped() => Some(Offer::Start(agent.metadata.name.clone())),
        Ok(agent) => bare.then(|| Offer::Open(Some(agent.metadata.name.clone()))),
        Err(Error::Rpc(error)) if error.is_not_found() => Some(Offer::Init),
        // Several agents tie for this directory.
        Err(Error::Rpc(error)) if error.is_invalid_params() => bare.then_some(Offer::Open(None)),
        Err(_) => None,
    }
}

/// What the offer says and asks, the question last.
fn question(offer: &Offer, bare: bool) -> [String; 2] {
    match offer {
        Offer::Open(Some(name)) => [
            format!("This directory has the agent {name}."),
            "Open it in agentctl tui?".into(),
        ],
        Offer::Open(None) => [
            "This directory has several agents.".into(),
            "Open them in agentctl tui?".into(),
        ],
        Offer::Start(name) if bare => [
            format!("This directory has the agent {name}, which is stopped."),
            "Start it and open agentctl tui?".into(),
        ],
        Offer::Start(name) => [
            format!("This directory has the agent {name}, which is stopped."),
            "Start it?".into(),
        ],
        Offer::Init => [
            "No agent is set up for this directory.".into(),
            "Set one up now with `agentctl init`?".into(),
        ],
    }
}

fn accepted(offer: &Offer, bare: bool) -> CommandResult<bool> {
    let [about, ask] = question(offer, bare);
    Ok(prompt::confirm_lines(&[&about, &ask])?)
}

/// `agentctl` on its own at a terminal: offers to open, start or set up the current
/// directory's agent, and prints the help when declined.
pub(crate) async fn bare(home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let manifest_here = directory.join(agent::manifest::MANIFEST_FILE).is_file();
    let resolved = client.resolve_agent_variant(directory, None).await;
    let Some(offer) = offer(&resolved, true, true) else {
        return Err(crate::inference_error(
            resolved.err().unwrap_or(Error::NotFound),
            manifest_here,
        ));
    };
    if !accepted(&offer, true)? {
        use clap::CommandFactory as _;
        crate::Arguments::command().print_help().map_err(Error::from)?;
        return Ok(ExitCode::SUCCESS);
    }
    match offer {
        Offer::Open(_) => tui::run(home, client).await,
        Offer::Start(name) => {
            start(client, &name).await?;
            tui::run(home, client).await
        }
        Offer::Init => init::run(),
    }
}

/// The agent a command goes on with, or how it ended instead.
pub(crate) enum Target {
    Agent(String),
    Done(ExitCode),
}

/// The current directory's agent, for a command given no agent. At a terminal, offers to set
/// one up when there is none, which ends the command with init's exit code, and to start it when
/// it is stopped; declining either gives the error the command gives without a terminal.
pub(crate) async fn current_agent(client: &Client, variant: Option<AgentVariantName>) -> CommandResult<Target> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let manifest_here = directory.join(agent::manifest::MANIFEST_FILE).is_file();
    let resolved = client.resolve_agent_variant(directory, variant).await;
    match (offer(&resolved, false, prompt::interactive()), resolved) {
        (Some(offer @ Offer::Init), Err(error)) => {
            if accepted(&offer, false)? {
                return init::run().map(Target::Done);
            }
            Err(crate::inference_error(error, manifest_here))
        }
        (Some(offer @ Offer::Start(_)), Ok(agent)) => {
            let name = agent.metadata.name;
            if !accepted(&offer, false)? {
                return Err(Error::Stopped(name).into());
            }
            start(client, &name).await?;
            Ok(Target::Agent(name))
        }
        (_, Ok(agent)) => Ok(Target::Agent(agent.metadata.name)),
        (_, Err(error)) => Err(crate::inference_error(error, manifest_here)),
    }
}

async fn start(client: &Client, name: &str) -> CommandResult<()> {
    client.set_run_state(name, RunState::Running).await?;
    crate::wait_until_converged(client, name, START_TIMEOUT).await?;
    eprintln!("agent/{name} started");
    Ok(())
}

/// The session `agentctl attach` opens when given none: the agent's only one, a new one when it
/// has none, or the one chosen at the terminal.
pub(crate) async fn session(client: &Client, agent: &str) -> CommandResult<String> {
    if !prompt::interactive() {
        return Err(CommandError::Message(
            "attach needs a terminal; name the session to attach to, for example `agentctl attach session/s1`".into(),
        ));
    }
    let sessions = client.list_sessions(Some(agent)).await?;
    let (name, new) = choose_session(agent, &sessions, prompt::choose)?;
    if new {
        eprintln!("Starting session/{name}; detach with ctrl-b d, and run `agentctl attach` here to return.");
    }
    Ok(name)
}

/// The session to open among `sessions` and whether it is new; `ask` picks among several by
/// index, the index after them for a new one. Archived sessions are not offered, but their
/// names stay taken.
fn choose_session(
    agent: &str,
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
            let index = ask(&format!("Which session of {agent}?"), &options)?;
            several.get(index).copied()
        }
    };
    Ok(chosen.map_or_else(|| (free_name(sessions), true), |name| (name.to_owned(), false)))
}

/// The first of `s1`, `s2` and so on that no session has.
fn free_name(sessions: &[Session]) -> String {
    // One more name than there are sessions always has a free one.
    (1..=sessions.len() + 1)
        .map(|number| format!("s{number}"))
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
            serde_yaml_ng::from_str(include_str!("../../../examples/minimal/agent.yaml")).expect("example manifest");
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
    fn bare_agentctl_offers_to_open_start_or_set_up_the_agent_here() {
        let not_found = rpc(NOT_FOUND);
        let several = rpc(INVALID_PARAMS);
        assert_eq!(
            offer(&Ok(agent("worker", false)), true, true),
            Some(Offer::Open(Some("worker".into())))
        );
        assert_eq!(
            offer(&Ok(agent("worker", true)), true, true),
            Some(Offer::Start("worker".into()))
        );
        assert_eq!(offer(&not_found, true, true), Some(Offer::Init));
        assert_eq!(offer(&several, true, true), Some(Offer::Open(None)));
        assert_eq!(offer(&rpc(-32603), true, true), None);
        assert_eq!(
            question(&Offer::Start("worker".into()), true)[1],
            "Start it and open agentctl tui?"
        );
    }

    #[test]
    fn commands_offer_to_set_up_a_missing_agent_or_start_a_stopped_one_and_nothing_else() {
        let not_found = rpc(NOT_FOUND);
        let several = rpc(INVALID_PARAMS);
        assert_eq!(offer(&Ok(agent("worker", false)), false, true), None);
        assert_eq!(
            offer(&Ok(agent("worker", true)), false, true),
            Some(Offer::Start("worker".into()))
        );
        assert_eq!(offer(&not_found, false, true), Some(Offer::Init));
        assert_eq!(offer(&several, false, true), None);
        assert_eq!(question(&Offer::Start("worker".into()), false)[1], "Start it?");
    }

    #[test]
    fn nothing_is_offered_without_a_terminal() {
        for bare in [true, false] {
            for resolved in [
                Ok(agent("worker", false)),
                Ok(agent("worker", true)),
                rpc(NOT_FOUND),
                rpc(INVALID_PARAMS),
            ] {
                assert_eq!(offer(&resolved, bare, false), None, "{resolved:?}");
            }
        }
    }

    #[test]
    fn attach_opens_the_only_session_or_a_new_one_without_asking() {
        let never = |_: &str, _: &[&str]| -> Result<usize, Error> { panic!("asked") };
        assert_eq!(
            choose_session("worker", &[], never).expect("none"),
            ("s1".to_owned(), true)
        );
        assert_eq!(
            choose_session("worker", &[session("main", false)], never).expect("one"),
            ("main".to_owned(), false)
        );
        // An archived session is not offered, and its name is not reused.
        assert_eq!(
            choose_session("worker", &[session("s1", true), session("main", false)], never).expect("one active"),
            ("main".to_owned(), false)
        );
        assert_eq!(
            choose_session("worker", &[session("s1", true)], never).expect("only archived"),
            ("s2".to_owned(), true)
        );
    }

    #[test]
    fn attach_asks_which_of_several_sessions_or_a_new_one() {
        let sessions = [session("s1", false), session("s3", true), session("s2", false)];
        let mut asked = Vec::new();
        let chosen = choose_session("worker", &sessions, |question, options| {
            assert_eq!(question, "Which session of worker?");
            asked = options.iter().map(|option| (*option).to_owned()).collect();
            Ok(1)
        })
        .expect("several");
        assert_eq!(asked, ["s1", "s2", "A new session"]);
        assert_eq!(chosen, ("s2".to_owned(), false));
        assert_eq!(
            choose_session("worker", &sessions, |_, _| Ok(2)).expect("new"),
            ("s4".to_owned(), true)
        );
    }
}
