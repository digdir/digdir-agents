//! `agentctl init`: hands setting up an agent to the coding agent the person already uses here,
//! which does it by reading `agentctl guide setup`, and takes the person into that agent after.

use std::{
    ffi::OsStr,
    io::IsTerminal as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use agent::{Agent, Error, Harness, control_api::Client, local::home::ControlPlaneHome};

use crate::{
    CommandError, CommandResult, launch, prompt,
    tui::{self, AgentPhase},
};

/// Starts a coding agent found on `PATH` in the repository holding the current directory, on the
/// handover prompt, and waits for it. Then offers to open a session in the agent it set up, or
/// says what to do next, and exits with the coding agent's exit code unless the session opens.
/// Without a coding agent, prints the prompt to paste.
pub(crate) async fn run(home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    require_terminal(std::io::stdin().is_terminal(), std::io::stdout().is_terminal())?;
    let current = std::env::current_dir().map_err(agent::Error::from)?;
    let root = tui::repository_root(&current);
    let directory = root.unwrap_or(&current);
    let handover = handover_prompt(directory, root.is_some());
    let found = installed(std::env::var_os("PATH").as_deref());
    let Some((harness, program)) = choose(&found, prompt::choose)? else {
        eprintln!(
            "No coding agent found here ({}). Paste this into the one you use:\n",
            either(&Harness::ALL.map(Harness::display_name))
        );
        println!("{handover}");
        return Ok(ExitCode::SUCCESS);
    };
    let name = harness.display_name();
    eprintln!("Starting {name} to set up an agent with you.");
    let mut command = Command::new(program);
    command.args(harness.host_arguments(&handover)).current_dir(directory);
    let code = crate::run_and_wait(command)
        .map_err(|error| CommandError::Message(format!("could not start {name}: {error}")))?;
    let resolved = match crate::ensure_daemon(home, client).await {
        Ok(()) => client.resolve_agent_variant(directory.to_path_buf(), None).await,
        Err(error) => Err(error),
    };
    let onward = onward(&resolved);
    if let Onward::Offer(name) = &onward
        && prompt::confirm_lines(&[&format!("Your agent {name} is set up."), "Open a session in it now?"])?
    {
        let selection = crate::SessionSelection::default();
        return Box::pin(crate::attach(home, client, None, None, None, None, selection)).await;
    }
    eprintln!("{}", next_step(&onward, root.is_some()));
    Ok(code)
}

/// What follows the coding agent's exit, from the agent found here afterwards.
#[derive(Debug, PartialEq, Eq)]
enum Onward {
    /// The agent is ready or on its way: a session in it is offered.
    Offer(String),
    /// The agent failed, or is being deleted.
    Failed(String),
    /// No agent here.
    Nothing,
    /// Several agents here.
    Several,
    /// agentd could not say, for this reason.
    Unknown(String),
}

fn onward(resolved: &Result<Agent, Error>) -> Onward {
    match resolved {
        Ok(agent) => {
            let name = agent.metadata.name.clone();
            match tui::agent_state(agent).phase {
                AgentPhase::Failed | AgentPhase::Retrying | AgentPhase::Unresponsive | AgentPhase::Terminating => {
                    Onward::Failed(name)
                }
                _ => Onward::Offer(name),
            }
        }
        Err(Error::Rpc(error)) if error.is_not_found() => Onward::Nothing,
        Err(Error::Rpc(error)) if error.is_invalid_params() => Onward::Several,
        Err(error) => Onward::Unknown(error.to_string()),
    }
}

/// The line that says where to go from `onward`, when no session is opened.
fn next_step(onward: &Onward, in_repository: bool) -> String {
    let place = if in_repository {
        "from anywhere in the repository"
    } else {
        "in this directory"
    };
    match onward {
        Onward::Offer(_) => format!("Open a session in it later with `agentctl attach` {place}."),
        Onward::Failed(name) => {
            format!("Your agent {name} is not running; `agentctl status` shows why and what to do next.")
        }
        Onward::Nothing => "Nothing was set up; run `agentctl init` again to continue.".into(),
        Onward::Several => "This directory has several agents; open one with `agentctl attach --agent NAME`, or \
                            all of them in `agentctl tui`."
            .into(),
        Onward::Unknown(reason) => {
            format!("Could not look up the agent here: {reason}. `agentctl attach` {place} opens it once agentd runs.")
        }
    }
}

fn require_terminal(stdin: bool, stdout: bool) -> CommandResult<()> {
    if stdin && stdout {
        return Ok(());
    }
    Err(CommandError::Message(
        "init starts a coding agent and needs a terminal; from a coding agent, run `agentctl guide setup` instead"
            .into(),
    ))
}

/// What the coding agent is asked: the goal, the directory and the guide, nothing more.
fn handover_prompt(directory: &Path, in_repository: bool) -> String {
    let directory = directory.display();
    let subject = if in_repository {
        format!("for this repository, {directory}")
    } else {
        format!("for my work in {directory}")
    };
    format!(
        "Help me set up an agentctl agent {subject}. agentctl runs coding agents like you in sandboxed Linux VMs on \
         this machine. Run `agentctl guide setup` and follow it. This conversation is only for setting up the agent: \
         when it is running, tell me to exit, and agentctl will take me into it."
    )
}

/// `names` as alternatives: `A`, `A or B`, `A, B or C`.
fn either(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// Every harness whose program is on `path`, in [`Harness::ALL`] order, with where it is.
fn installed(path: Option<&OsStr>) -> Vec<(Harness, PathBuf)> {
    Harness::ALL
        .into_iter()
        .filter_map(|harness| Some((harness, launch::find_program(&[harness.program()], path)?)))
        .collect()
}

/// The one harness found, or the one `ask` picks by index when there are several.
fn choose(
    found: &[(Harness, PathBuf)],
    ask: impl FnOnce(&str, &[&str]) -> Result<usize, agent::Error>,
) -> CommandResult<Option<(Harness, PathBuf)>> {
    match found {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        several => {
            let names = several
                .iter()
                .map(|(harness, _)| harness.display_name())
                .collect::<Vec<_>>();
            let index = ask("Which coding agent should help you set up the agent?", &names)?;
            Ok(several.get(index).cloned())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_prompt_names_the_repository_or_the_directory_and_the_guide() {
        let directory = Path::new("/work/app");
        assert_eq!(
            handover_prompt(directory, true),
            "Help me set up an agentctl agent for this repository, /work/app. agentctl runs coding agents like \
             you in sandboxed Linux VMs on this machine. Run `agentctl guide setup` and follow it. This \
             conversation is only for setting up the agent: when it is running, tell me to exit, and agentctl will \
             take me into it."
        );
        let outside = handover_prompt(directory, false);
        assert!(
            outside.starts_with("Help me set up an agentctl agent for my work in /work/app. agentctl runs"),
            "{outside}"
        );
        assert!(!outside.contains("repository"), "{outside}");
    }

    fn agent(name: &str, reason: &str, ready: bool, failure: Option<agent::FailureKind>) -> Agent {
        let mut agent: Agent =
            serde_yaml_ng::from_str(include_str!("../../../examples/minimal/agent.yaml")).expect("example manifest");
        agent.metadata.name = name.into();
        agent.status.conditions.push(agent::Condition {
            kind: agent::Condition::READY.into(),
            status: if ready {
                agent::ConditionStatus::True
            } else {
                agent::ConditionStatus::False
            },
            reason: reason.into(),
            message: String::new(),
            last_transition_time: None,
        });
        agent.status.failure = failure;
        agent
    }

    fn rpc(code: i32) -> Result<Agent, Error> {
        Err(Error::Rpc(agent::control_api::ResponseError {
            code,
            message: "from agentd".into(),
        }))
    }

    #[test]
    fn after_the_coding_agent_a_session_is_offered_in_an_agent_ready_or_on_its_way() {
        let ready = agent("dev", "SandboxReady", true, None);
        assert_eq!(onward(&Ok(ready)), Onward::Offer("dev".into()));
        let starting = agent("dev", agent::Condition::REASON_STARTING, false, None);
        assert_eq!(onward(&Ok(starting)), Onward::Offer("dev".into()));
        assert_eq!(
            next_step(&Onward::Offer("dev".into()), true),
            "Open a session in it later with `agentctl attach` from anywhere in the repository."
        );
        assert_eq!(
            next_step(&Onward::Offer("dev".into()), false),
            "Open a session in it later with `agentctl attach` in this directory."
        );
    }

    #[test]
    fn after_the_coding_agent_a_failed_agent_points_to_status() {
        for failure in [agent::FailureKind::Invalid, agent::FailureKind::Transient] {
            let failed = agent("dev", "ImageBuildFailed", false, Some(failure));
            assert_eq!(onward(&Ok(failed)), Onward::Failed("dev".into()), "{failure:?}");
        }
        assert_eq!(
            next_step(&Onward::Failed("dev".into()), true),
            "Your agent dev is not running; `agentctl status` shows why and what to do next."
        );
    }

    #[test]
    fn after_the_coding_agent_no_agent_several_or_no_agentd_say_where_to_go() {
        // agentd's codes for no agent here, and for several.
        assert_eq!(onward(&rpc(-32004)), Onward::Nothing);
        assert_eq!(onward(&rpc(-32602)), Onward::Several);
        let unreachable = onward(&Err(Error::Daemon("automatic startup exited".into())));
        assert!(
            matches!(&unreachable, Onward::Unknown(reason) if reason.contains("automatic startup exited")),
            "{unreachable:?}"
        );
        assert_eq!(
            next_step(&Onward::Nothing, true),
            "Nothing was set up; run `agentctl init` again to continue."
        );
        assert!(next_step(&Onward::Several, true).contains("`agentctl attach --agent NAME`"));
        let unknown = next_step(&unreachable, true);
        assert!(
            unknown.contains("automatic startup exited") && unknown.contains("`agentctl attach`"),
            "{unknown}"
        );
    }

    #[test]
    fn the_harnesses_are_listed_as_alternatives() {
        assert_eq!(
            either(&Harness::ALL.map(Harness::display_name)),
            "Claude Code, Codex or Pi"
        );
        assert_eq!(either(&["Codex", "Pi"]), "Codex or Pi");
        assert_eq!(either(&["Pi"]), "Pi");
    }

    #[test]
    fn without_a_terminal_init_points_to_the_guide() {
        require_terminal(true, true).expect("a terminal");
        for (stdin, stdout) in [(false, true), (true, false), (false, false)] {
            let Err(CommandError::Message(message)) = require_terminal(stdin, stdout) else {
                panic!("stdin {stdin}, stdout {stdout} is no terminal");
            };
            assert!(message.contains("agentctl guide setup"), "{message}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn harnesses_are_found_on_path_in_their_order() {
        let first = tempfile::tempdir().expect("directory");
        let second = tempfile::tempdir().expect("directory");
        let path = std::env::join_paths([first.path(), second.path()]).expect("PATH");
        assert_eq!(installed(Some(&path)), []);
        std::fs::write(second.path().join(Harness::Pi.program()), "").expect("pi");
        std::fs::write(first.path().join(Harness::Codex.program()), "").expect("codex");
        assert_eq!(
            installed(Some(&path)),
            [
                (Harness::Codex, first.path().join(Harness::Codex.program())),
                (Harness::Pi, second.path().join(Harness::Pi.program())),
            ]
        );
    }

    #[test]
    fn one_harness_is_used_without_asking_and_several_are_asked_about() {
        let never = |_: &str, _: &[&str]| -> Result<usize, agent::Error> { panic!("asked") };
        assert_eq!(choose(&[], never).expect("none"), None);
        let codex = (Harness::Codex, PathBuf::from("/bin/codex"));
        assert_eq!(
            choose(std::slice::from_ref(&codex), never).expect("one"),
            Some(codex.clone())
        );

        let pi = (Harness::Pi, PathBuf::from("/bin/pi"));
        let mut asked = Vec::new();
        let chosen = choose(&[codex, pi.clone()], |_, names| {
            asked = names.iter().map(|name| (*name).to_owned()).collect();
            Ok(1)
        })
        .expect("several");
        assert_eq!(asked, ["Codex", "Pi"]);
        assert_eq!(chosen, Some(pi));
    }
}
