//! `agentctl status`: agentd, the harness logins, this directory's agent and every agent at a
//! glance, with the commands that would change what it shows.

use std::{fmt::Write as _, path::PathBuf};

use agent::{
    Agent, Error,
    control_api::Client,
    harness::{Harness, HarnessAuthMode, HarnessLogin},
    local::home::ControlPlaneHome,
    sessions::{Session, State},
};
use serde::Serialize;

use crate::{
    CommandResult, print_json,
    tui::{AgentPhase, agent_state},
};

/// What `agentctl status --json` prints, for agents that set up and check agentctl. The object is a
/// contract, described key by key in `agentctl/guide/status.md` (`agentctl guide status`).
#[derive(Debug, Serialize)]
pub(crate) struct Status {
    version: u32,
    agentd: Agentd,
    harnesses: Vec<HarnessStatus>,
    directory: Directory,
    agents: Agents,
    next: Vec<NextStep>,
}

#[derive(Debug, Serialize)]
struct Agentd {
    version: Option<String>,
    home: PathBuf,
}

#[derive(Debug, Serialize)]
struct HarnessStatus {
    harness: Harness,
    auth: HarnessAuthMode,
    login_stored: Option<bool>,
}

#[derive(Debug, Serialize)]
struct Directory {
    path: PathBuf,
    agent: Option<DirectoryAgent>,
    problem: Option<String>,
}

#[derive(Debug, Serialize)]
struct DirectoryAgent {
    name: String,
    state: AgentPhase,
    reason: Option<String>,
    sessions: usize,
    sessions_waiting: usize,
}

#[derive(Debug, Default, Serialize)]
struct Agents {
    total: usize,
    running: usize,
    starting: usize,
    failing: usize,
    stopped: usize,
    sessions_waiting: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct NextStep {
    command: String,
    purpose: String,
}

/// What the current directory resolves to: an agent, none, or why it cannot tell.
type Resolution = Result<Option<Agent>, String>;

/// Prints the status as text, or as the [`Status`] object with `json`.
pub(crate) async fn run(home: &ControlPlaneHome, client: &Client, json: bool) -> CommandResult<()> {
    let daemon = client.health().await?;
    let logins = client.auth_logins().await?;
    let path = std::env::current_dir().map_err(Error::from)?;
    let resolution = match client.resolve_agent_variant(path.clone(), None).await {
        Ok(agent) => Ok(Some(agent)),
        Err(Error::Rpc(error)) if error.is_not_found() => Ok(None),
        Err(Error::Rpc(error)) if error.is_invalid_params() => Err(error.message),
        Err(error) => return Err(error.into()),
    };
    let manifest_here = path.join(agent::manifest::MANIFEST_FILE).is_file();
    let daemon = Agentd {
        version: daemon.build_version,
        home: home.path().to_path_buf(),
    };
    let agents = client.list_agents().await?;
    let sessions = client.list_sessions(None).await?;
    let status = Status::new(daemon, &logins, path, &resolution, &agents, &sessions, manifest_here);
    if json {
        print_json(&status)
    } else {
        print!("{}", status.text());
        Ok(())
    }
}

impl Status {
    fn new(
        daemon: Agentd,
        logins: &[HarnessLogin],
        path: PathBuf,
        resolution: &Resolution,
        agents: &[Agent],
        sessions: &[Session],
        manifest_here: bool,
    ) -> Self {
        let sessions: Vec<&Session> = sessions.iter().filter(|session| !session.is_archived()).collect();
        let waiting = |agent: Option<&str>| {
            sessions
                .iter()
                .filter(|session| agent.is_none_or(|agent| session.agent == agent))
                .filter(|session| session.status.state == State::WaitingForInput)
                .count()
        };
        let mut summary = Agents {
            sessions_waiting: waiting(None),
            ..Agents::default()
        };
        for agent in agents {
            match agent_state(agent).phase {
                AgentPhase::Terminating => continue,
                AgentPhase::Ready => summary.running += 1,
                AgentPhase::Stopped | AgentPhase::Stopping => summary.stopped += 1,
                AgentPhase::Failed | AgentPhase::Retrying | AgentPhase::Unresponsive => summary.failing += 1,
                AgentPhase::Pending | AgentPhase::Starting | AgentPhase::Provisioning => summary.starting += 1,
            }
            summary.total += 1;
        }
        let (here, problem) = match resolution {
            Ok(agent) => (agent.as_ref(), None),
            Err(problem) => (None, Some(problem.clone())),
        };
        let directory = Directory {
            path,
            agent: here.map(|agent| {
                let name = agent.metadata.name.clone();
                let state = agent_state(agent);
                DirectoryAgent {
                    sessions: sessions.iter().filter(|session| session.agent == name).count(),
                    sessions_waiting: waiting(Some(&name)),
                    // agentd records an invalid agent's reason as the error it was, prefix and all.
                    reason: Some(state.detail.trim_start_matches("invalid Agent: ").to_owned())
                        .filter(|reason| state.failure && !reason.is_empty()),
                    state: state.phase,
                    name,
                }
            }),
            problem,
        };
        let next = next_steps(logins, resolution, &directory, &summary, manifest_here);
        Self {
            version: 1,
            agentd: daemon,
            harnesses: logins
                .iter()
                .map(|login| HarnessStatus {
                    harness: login.harness,
                    auth: login.harness.auth_mode(),
                    login_stored: login.stored,
                })
                .collect(),
            directory,
            agents: summary,
            next,
        }
    }

    /// The status as people read it: one line per fact, then the next steps.
    fn text(&self) -> String {
        let daemon = format!(
            "answering, version {}, home {}",
            self.agentd.version.as_deref().unwrap_or("unknown"),
            self.agentd.home.display()
        );
        let logins = self
            .harnesses
            .iter()
            .map(|login| match login.login_stored {
                Some(true) => format!("{} signed in", login.harness.display_name()),
                Some(false) => format!("{} not signed in", login.harness.display_name()),
                None => format!("{} set up by each agent", login.harness.display_name()),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let here = match (&self.directory.agent, &self.directory.problem) {
            (Some(agent), _) => {
                let mut line = format!("{}: {}", agent.name, agent.state.label().to_lowercase());
                if let Some(reason) = &agent.reason {
                    let _ = write!(line, ": {reason}");
                } else {
                    let _ = write!(line, ", {}", count(agent.sessions, "session"));
                    if agent.sessions_waiting > 0 {
                        let _ = write!(line, ", {} waiting for you", agent.sessions_waiting);
                    }
                }
                line
            }
            (None, Some(problem)) => problem.clone(),
            (None, None) => "no agent".to_owned(),
        };
        let summary = &self.agents;
        let mut everywhere = if summary.total == 0 {
            "no agents".to_owned()
        } else {
            let states = [
                (summary.running, "running"),
                (summary.starting, "starting"),
                (summary.failing, "failing"),
                (summary.stopped, "stopped"),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, state)| format!("{count} {state}"))
            .collect::<Vec<_>>();
            format!("{}: {}", count(summary.total, "agent"), states.join(", "))
        };
        if summary.sessions_waiting > 0 {
            let _ = write!(
                everywhere,
                "; {} waiting for you",
                count(summary.sessions_waiting, "session")
            );
        }
        let mut text = String::new();
        for (label, value) in [
            ("agentd", daemon),
            ("Logins", logins),
            ("This directory", here),
            ("All agents", everywhere),
        ] {
            let _ = writeln!(text, "{label:<15} {value}");
        }
        if !self.next.is_empty() {
            let width = self.next.iter().map(|step| step.command.len()).max().unwrap_or(0);
            text.push_str("\nNext\n");
            for step in &self.next {
                let _ = writeln!(text, "  {:<width$}  {}", step.command, step.purpose);
            }
        }
        text
    }
}

/// `count` of `noun`, with an `s` unless it is one.
fn count(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// The agentctl command that stores a login for `harness`, when it has one.
const fn login_command(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::ClaudeCode => Some("agentctl claude login"),
        Harness::Codex => Some("agentctl codex login"),
        Harness::Pi => None,
    }
}

/// At most two commands that would change what status shows, the most pressing first: a login
/// the agent here, or every agent, needs; then what this directory's agent needs; then the
/// sessions waiting for the person.
fn next_steps(
    logins: &[HarnessLogin],
    resolution: &Resolution,
    directory: &Directory,
    agents: &Agents,
    manifest_here: bool,
) -> Vec<NextStep> {
    let step = |command: String, purpose: String| NextStep { command, purpose };
    let mut next = Vec::new();
    let missing: Vec<Harness> = logins
        .iter()
        .filter(|login| login.stored == Some(false))
        .map(|login| login.harness)
        .collect();
    let needed: Vec<Harness> = match resolution {
        Ok(Some(agent)) => agent
            .spec
            .harnesses
            .iter()
            .filter(|installation| !installation.optional && missing.contains(&installation.kind))
            .map(|installation| installation.kind)
            .collect(),
        _ if logins.iter().any(|login| login.stored == Some(true)) => Vec::new(),
        _ => missing.iter().copied().take(1).collect(),
    };
    // Without an agent here, any harness will do, so the others are alternatives.
    let anywhere = !matches!(resolution, Ok(Some(_)));
    for harness in needed {
        if let Some(command) = login_command(harness) {
            let alternatives = missing
                .iter()
                .filter(|other| anywhere && **other != harness)
                .filter_map(|other| {
                    login_command(*other).map(|command| format!(", or {} with `{command}`", other.display_name()))
                })
                .collect::<String>();
            next.push(step(
                command.to_owned(),
                format!("Let agents use {}{alternatives}", harness.display_name()),
            ));
        }
    }
    match (resolution, &directory.agent) {
        (Ok(None), _) if manifest_here => next.push(step(
            "agentctl apply".to_owned(),
            "Create the agent from ./agent.yaml".to_owned(),
        )),
        (Ok(None), _) => next.push(step(
            "agentctl init".to_owned(),
            "Set up an agent for this repository".to_owned(),
        )),
        (Ok(Some(agent)), Some(here)) if agent.spec.is_stopped() => next.push(step(
            format!("agentctl start agent/{}", here.name),
            format!("Start {} again", here.name),
        )),
        // A login it needs is what to do first, and usually the whole reason.
        (Ok(Some(_)), Some(here)) if here.reason.is_some() && next.is_empty() => next.push(step(
            format!("agentctl describe agent/{}", here.name),
            format!("See the details of what {} needs", here.name),
        )),
        _ => {}
    }
    if agents.sessions_waiting > 0 {
        next.push(step(
            "agentctl tui".to_owned(),
            format!(
                "Answer the {} waiting for you",
                count(agents.sessions_waiting, "session")
            ),
        ));
    }
    next.truncate(2);
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> Agent {
        let mut agent: Agent =
            serde_yaml_ng::from_str(include_str!("../../../examples/minimal/agent.yaml")).expect("example manifest");
        agent.metadata.name = name.into();
        agent
    }

    fn with_ready(name: &str, status: agent::ConditionStatus, reason: &str, message: &str) -> Agent {
        let mut agent = agent(name);
        agent.status.conditions = vec![agent::Condition {
            kind: agent::Condition::READY.into(),
            status,
            reason: reason.into(),
            message: message.into(),
            last_transition_time: None,
        }];
        agent
    }

    fn ready(name: &str) -> Agent {
        with_ready(name, agent::ConditionStatus::True, "Ready", "")
    }

    fn failed(name: &str, message: &str) -> Agent {
        let mut agent = with_ready(name, agent::ConditionStatus::False, "SandboxReconcileFailed", message);
        agent.status.failure = Some(agent::FailureKind::Invalid);
        agent
    }

    fn stopped(name: &str) -> Agent {
        let mut agent = with_ready(
            name,
            agent::ConditionStatus::False,
            agent::Condition::REASON_STOPPED,
            "",
        );
        agent.spec.run_state = Some(agent::RunState::Stopped);
        agent
    }

    fn session(agent: &str, name: &str, waiting: bool) -> Session {
        let mut session: Session = serde_json::from_value(serde_json::json!({
            "id": format!("00000000-0000-4000-8000-{:012}", name.len()),
            "agentId": "00000000-0000-4000-8000-000000000001",
            "agent": agent,
            "name": name,
            "harness": "pi",
            "createdAt": "2026-01-01T00:00:00Z",
        }))
        .expect("session");
        if waiting {
            session.status.state = State::WaitingForInput;
        }
        session
    }

    fn logins(stored: &[Harness]) -> Vec<HarnessLogin> {
        Harness::ALL
            .into_iter()
            .map(|harness| HarnessLogin {
                harness,
                stored: (harness.auth_mode() == HarnessAuthMode::Mediated).then(|| stored.contains(&harness)),
            })
            .collect()
    }

    fn status(stored: &[Harness], resolution: &Resolution, agents: &[Agent], sessions: &[Session]) -> Status {
        let daemon = Agentd {
            version: Some("1.2.3".into()),
            home: PathBuf::from("/home/person/.agent"),
        };
        Status::new(
            daemon,
            &logins(stored),
            PathBuf::from("/work/repo"),
            resolution,
            agents,
            sessions,
            false,
        )
    }

    fn commands(status: &Status) -> Vec<&str> {
        status.next.iter().map(|step| step.command.as_str()).collect()
    }

    #[test]
    fn the_json_keys_are_a_contract() {
        let worker = failed("worker", "the image does not exist");
        let value = serde_json::to_value(status(
            &[Harness::ClaudeCode],
            &Ok(Some(worker.clone())),
            &[worker],
            &[],
        ))
        .expect("json");
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "agentd": {"version": "1.2.3", "home": "/home/person/.agent"},
                "harnesses": [
                    {"harness": "claudeCode", "auth": "mediated", "login_stored": true},
                    {"harness": "codex", "auth": "mediated", "login_stored": false},
                    {"harness": "pi", "auth": "none", "login_stored": null},
                ],
                "directory": {
                    "path": "/work/repo",
                    "agent": {
                        "name": "worker",
                        "state": "failed",
                        "reason": "the image does not exist",
                        "sessions": 0,
                        "sessions_waiting": 0,
                    },
                    "problem": null,
                },
                "agents": {
                    "total": 1, "running": 0, "starting": 0, "failing": 1, "stopped": 0, "sessions_waiting": 0,
                },
                "next": [{"command": "agentctl describe agent/worker", "purpose": "See the details of what worker needs"}],
            })
        );
        let nothing = serde_json::to_value(status(&[], &Ok(None), &[], &[])).expect("json");
        assert_eq!(nothing["directory"]["agent"], serde_json::Value::Null);
        assert_eq!(nothing["directory"]["problem"], serde_json::Value::Null);
    }

    #[test]
    fn the_guide_shows_the_object_with_every_key() {
        // Every key, recursively, with each array's elements merged into one shape.
        fn keys(value: &serde_json::Value, path: &str, into: &mut std::collections::BTreeSet<String>) {
            match value {
                serde_json::Value::Object(object) => {
                    for (key, value) in object {
                        let path = format!("{path}.{key}");
                        keys(value, &path, into);
                        into.insert(path);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|item| keys(item, &format!("{path}[]"), into)),
                _ => {}
            }
        }
        let guide = include_str!("../../../guide/status.md");
        let example = guide
            .split_once("```json\n")
            .and_then(|(_, rest)| rest.split_once("```\n"))
            .map(|(example, _)| example)
            .expect("an example object");
        let example: serde_json::Value = serde_json::from_str(example).expect("the example is JSON");
        let worker = failed("worker", "the image does not exist");
        let actual = serde_json::to_value(status(&[], &Ok(Some(worker.clone())), &[worker], &[])).expect("json");
        let (mut documented, mut produced) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
        keys(&example, "", &mut documented);
        keys(&actual, "", &mut produced);
        assert_eq!(documented, produced);
        for key in &produced {
            let name = key.rsplit('.').next().expect("a key name");
            assert!(
                guide.contains(&format!("`{name}`")),
                "agentctl guide status describes {key}"
            );
        }
    }

    #[test]
    fn every_agent_phase_has_a_stable_json_name() {
        // Exhaustive, so a new phase cannot reach the JSON without a name chosen here.
        let name = |phase| match phase {
            AgentPhase::Ready => "ready",
            AgentPhase::Pending => "pending",
            AgentPhase::Starting => "starting",
            AgentPhase::Provisioning => "provisioning",
            AgentPhase::Retrying => "retrying",
            AgentPhase::Failed => "failed",
            AgentPhase::Unresponsive => "unresponsive",
            AgentPhase::Stopping => "stopping",
            AgentPhase::Stopped => "stopped",
            AgentPhase::Terminating => "terminating",
        };
        for phase in [
            AgentPhase::Ready,
            AgentPhase::Pending,
            AgentPhase::Starting,
            AgentPhase::Provisioning,
            AgentPhase::Retrying,
            AgentPhase::Failed,
            AgentPhase::Unresponsive,
            AgentPhase::Stopping,
            AgentPhase::Stopped,
            AgentPhase::Terminating,
        ] {
            assert_eq!(serde_json::to_value(phase).expect("json"), name(phase));
        }
    }

    #[test]
    fn next_steps_lead_with_a_missing_login_then_this_directory() {
        assert_eq!(
            commands(&status(&[], &Ok(None), &[], &[])),
            ["agentctl claude login", "agentctl init"]
        );
        assert_eq!(
            status(&[], &Ok(None), &[], &[]).next[0].purpose,
            "Let agents use Claude Code, or Codex with `agentctl codex login`"
        );
        assert_eq!(
            commands(&status(&[Harness::Codex], &Ok(None), &[], &[])),
            ["agentctl init"]
        );
        // The agent here needs its own harness signed in, even when another one is.
        let worker = ready("worker");
        assert_eq!(worker.spec.harnesses[0].kind, Harness::ClaudeCode);
        assert_eq!(
            commands(&status(
                &[Harness::Codex],
                &Ok(Some(worker.clone())),
                std::slice::from_ref(&worker),
                &[]
            )),
            ["agentctl claude login"]
        );
        assert_eq!(
            commands(&status(
                &[Harness::ClaudeCode],
                &Ok(Some(worker.clone())),
                &[worker],
                &[]
            )),
            Vec::<String>::new()
        );
        // An optional installation without a login is left out, not waited for.
        let mut offering = ready("offering");
        let mut codex = offering.spec.harnesses[0].clone();
        codex.kind = Harness::Codex;
        codex.optional = true;
        offering.spec.harnesses.push(codex);
        assert_eq!(
            commands(&status(
                &[Harness::ClaudeCode],
                &Ok(Some(offering.clone())),
                &[offering],
                &[]
            )),
            Vec::<String>::new()
        );
        let paused = stopped("paused");
        assert_eq!(
            commands(&status(
                &[Harness::ClaudeCode],
                &Ok(Some(paused.clone())),
                &[paused],
                &[]
            )),
            ["agentctl start agent/paused"]
        );
        let waiting = [session("worker", "s1", true)];
        assert_eq!(
            commands(&status(&[Harness::ClaudeCode], &Ok(None), &[ready("worker")], &waiting)),
            ["agentctl init", "agentctl tui"]
        );
        // Several agents here: the problem says how to choose, and no command guesses.
        assert_eq!(
            commands(&status(&[Harness::ClaudeCode], &Err("several".into()), &[], &[])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_text_has_one_line_per_fact_then_the_next_steps() {
        let worker = ready("worker");
        let agents = [worker.clone(), stopped("paused"), failed("broken", "no such image")];
        let sessions = [
            session("worker", "s1", true),
            session("worker", "s2", false),
            session("paused", "s3", false),
        ];
        assert_eq!(
            status(&[Harness::ClaudeCode], &Ok(Some(worker)), &agents, &sessions).text(),
            "\
agentd          answering, version 1.2.3, home /home/person/.agent
Logins          Claude Code signed in, Codex not signed in, Pi set up by each agent
This directory  worker: ready, 2 sessions, 1 waiting for you
All agents      3 agents: 1 running, 1 failing, 1 stopped; 1 session waiting for you

Next
  agentctl tui  Answer the 1 session waiting for you
"
        );
        let broken = failed("broken", "no such image");
        let text = status(&[Harness::ClaudeCode], &Ok(Some(broken.clone())), &[broken], &[]).text();
        let unsigned = failed("unsigned", "invalid Agent: Claude Code is not signed in");
        let without_login = status(&[], &Ok(Some(unsigned.clone())), &[unsigned], &[]);
        assert_eq!(commands(&without_login), ["agentctl claude login"]);
        assert!(
            without_login
                .text()
                .contains("This directory  unsigned: failed: Claude Code is not signed in\n"),
            "{}",
            without_login.text()
        );
        assert!(
            text.contains("This directory  broken: failed: no such image\n"),
            "{text}"
        );
        let text = status(&[], &Ok(None), &[], &[]).text();
        assert!(
            text.contains("This directory  no agent\nAll agents      no agents\n"),
            "{text}"
        );
        assert!(
            text.ends_with("  agentctl init          Set up an agent for this repository\n"),
            "{text}"
        );
    }

    #[test]
    fn a_manifest_here_without_its_agent_is_applied_rather_than_set_up() {
        let with_manifest = Status::new(
            Agentd {
                version: Some("1.2.3".into()),
                home: PathBuf::from("/home/person/.agent"),
            },
            &logins(&[Harness::ClaudeCode]),
            PathBuf::from("/work/repo/agents/dev"),
            &Ok(None),
            &[],
            &[],
            true,
        );
        assert_eq!(commands(&with_manifest), ["agentctl apply"]);
        assert_eq!(with_manifest.next[0].purpose, "Create the agent from ./agent.yaml");
    }
}
