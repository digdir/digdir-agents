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
/// Where the directory already has an agent, first asks whether to change it or set up another;
/// where it has an agent's files that agentd does not run, whether to start it from them, which
/// needs no coding agent unless that fails, or have the coding agent change it first.
pub(crate) async fn run(home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    require_terminal(std::io::stdin().is_terminal(), std::io::stdout().is_terminal())?;
    let current = std::env::current_dir().map_err(agent::Error::from)?;
    let root = tui::repository_root(&current);
    let directory = root.unwrap_or(&current);
    let agents = here(home, client, directory).await;
    let unapplied = match crate::unapplied(directory) {
        crate::Unapplied::Written { name, manifest } => Some((name, manifest)),
        crate::Unapplied::Here => Some((manifest_name(directory), PathBuf::from(agent::manifest::MANIFEST_FILE))),
        crate::Unapplied::None => None,
    };
    let task = match (agents.as_slice(), unapplied) {
        ([], Some((name, manifest))) => {
            if from_files(&manifest, prompt::choose)? {
                match crate::entry::start_from_files(client, &directory.join(&manifest)).await {
                    Ok(name) => {
                        let ready = format!("Your agent {name:?} is ready.");
                        let opened = open_session(home, client, &ready, &name, root.is_some()).await?;
                        return Ok(opened.unwrap_or(ExitCode::SUCCESS));
                    }
                    Err(error) => {
                        eprintln!("{}", crate::error_text(&error));
                        if !prompt::confirm_lines(&["Have your coding agent fix it?"])? {
                            return Err(CommandError::Message(format!(
                                "the agent in {} was not started",
                                manifest.display()
                            )));
                        }
                        Task::Change(name)
                    }
                }
            } else {
                Task::Change(name)
            }
        }
        _ => task(&agents, prompt::choose)?,
    };
    let handover = request(&task, directory, root.is_some());
    let found = installed(std::env::var_os("PATH").as_deref());
    let Some((harness, program)) = choose(&found, &task, prompt::choose)? else {
        eprintln!(
            "No coding agent found here ({}). Paste this into the one you use:\n",
            either(&Harness::ALL.map(Harness::display_name))
        );
        println!("{handover}");
        return Ok(ExitCode::SUCCESS);
    };
    let name = harness.display_name();
    prompt::proceed(
        &[
            &match &task {
                Task::Setup => format!("{name} opens next and sets up the agent with you."),
                Task::Change(agent) => format!("{name} opens next and changes the agent {agent:?} with you."),
            },
            "It may first ask whether you trust this folder.",
            &format!("When the agent is ready, exit {name} and agentctl takes you into it."),
        ],
        &format!("open {name}"),
    )?;
    let mut command = Command::new(program);
    command.args(harness.host_arguments(&handover)).current_dir(directory);
    let code = crate::run_and_wait(command)
        .map_err(|error| CommandError::Message(format!("could not start {name}: {error}")))?;
    let resolved = match crate::ensure_daemon(home, client).await {
        Ok(()) => client.resolve_agent_variant(directory.to_path_buf(), None).await,
        Err(error) => Err(error),
    };
    let onward = onward(&resolved, &crate::unapplied(directory));
    if let Onward::Offer(name) = &onward {
        let opened = open_session(home, client, &ready(&task, name), name, root.is_some()).await?;
        return Ok(opened.unwrap_or(code));
    }
    eprintln!("{}", next_step(&onward, root.is_some()));
    Ok(code)
}

/// Says `ready` of the agent `name` and offers to open a session in it, returning the session's
/// exit code; when declined, says how to open one later and returns `None`.
async fn open_session(
    home: &ControlPlaneHome,
    client: &Client,
    ready: &str,
    name: &str,
    in_repository: bool,
) -> CommandResult<Option<ExitCode>> {
    if prompt::confirm_lines(&[ready, "Open a session in it now?"])? {
        let selection = crate::SessionSelection::default();
        return Box::pin(crate::attach(home, client, None, None, None, None, selection))
            .await
            .map(Some);
    }
    eprintln!("{}", next_step(&Onward::Offer(name.to_owned()), in_repository));
    Ok(None)
}

/// The name of the agent `directory/agent.yaml` describes, else the directory's own name.
fn manifest_name(directory: &Path) -> String {
    agent::manifest::resolve(&directory.join(agent::manifest::MANIFEST_FILE)).map_or_else(
        |_| {
            directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        |resolved| resolved.agent.metadata.name,
    )
}

/// Whether to start the agent in `manifest`, which agentd does not run, from its files, as `ask`
/// picks by index, rather than have the coding agent change it first.
fn from_files(manifest: &Path, ask: impl FnOnce(&str, &[&str]) -> Result<usize, agent::Error>) -> CommandResult<bool> {
    let question = format!("The agent in {} isn't running. What should happen?", manifest.display());
    Ok(ask(
        &question,
        &["Start it from its files", "Have your coding agent change it first"],
    )? == 0)
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
    /// No agent here, but the files of one in `agents/<name>`, which agentd does not run.
    Written(String),
    /// Several agents here.
    Several,
    /// agentd could not say, for this reason.
    Unknown(String),
}

fn onward(resolved: &Result<Agent, Error>, unapplied: &crate::Unapplied) -> Onward {
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
        Err(Error::Rpc(error)) if error.is_not_found() => match unapplied {
            crate::Unapplied::Written { name, .. } => Onward::Written(name.clone()),
            crate::Unapplied::None | crate::Unapplied::Here => Onward::Nothing,
        },
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
            format!("Your agent {name:?} is not running; `agentctl status` shows why and what to do next.")
        }
        Onward::Nothing => "Nothing was set up; run `agentctl init` again to continue.".into(),
        Onward::Written(name) => {
            format!(
                "agents/{name} has an agent that isn't running; `agentctl apply -f agents/{name}/agent.yaml` starts it \
                 from its files."
            )
        }
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

/// What the coding agent is asked: the goal, the directory, the guide and that the session is for
/// nothing else. The person's own words keep it to setup more firmly than the guide does.
fn handover_prompt(directory: &Path, in_repository: bool) -> String {
    let directory = directory.display();
    let subject = if in_repository {
        format!("for this repository, {directory}")
    } else {
        format!("for this folder, {directory}")
    };
    format!(
        "Set up an agentctl agent {subject}. Run `agentctl guide setup` and follow it; this session is only for that."
    )
}

/// The agents `directory` already has, as [`existing`] finds them; none when agentd does not answer.
async fn here(home: &ControlPlaneHome, client: &Client, directory: &Path) -> Vec<String> {
    if crate::ensure_daemon(home, client).await.is_err() {
        return Vec::new();
    }
    let resolved = client.resolve_agent_variant(directory.to_path_buf(), None).await;
    let listed = match &resolved {
        Err(Error::Rpc(error)) if error.is_invalid_params() => client.list_agents().await.unwrap_or_default(),
        _ => Vec::new(),
    };
    existing(&resolved, &listed, directory)
}

/// What the coding agent is asked to do here.
#[derive(Debug, PartialEq, Eq)]
enum Task {
    /// Set up an agent, as when the directory has none.
    Setup,
    /// Change this agent, which the directory already has.
    Change(String),
}

/// The agents `directory` already has, by name: the one resolving it finds or, when several tie,
/// those of `listed` whose place holds it most closely. None when agentd has no agent here or could
/// not say.
fn existing(resolved: &Result<Agent, Error>, listed: &[Agent], directory: &Path) -> Vec<String> {
    match resolved {
        Ok(agent) => vec![agent.metadata.name.clone()],
        Err(Error::Rpc(error)) if error.is_invalid_params() => {
            let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let directory = canonical(directory);
            let holding: Vec<(usize, &str)> = listed
                .iter()
                .filter_map(|agent| {
                    let source = canonical(&agent.status.provenance.as_ref()?.source_directory);
                    let place = place(&source);
                    directory
                        .starts_with(place)
                        .then(|| (place.components().count(), agent.metadata.name.as_str()))
                })
                .collect();
            let closest = holding.iter().map(|(depth, _)| *depth).max();
            let mut names: Vec<String> = holding
                .into_iter()
                .filter(|(depth, _)| Some(*depth) == closest)
                .map(|(_, name)| name.to_owned())
                .collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

/// Where an agent applied from `source` belongs, as agentd resolves directories: its repository's
/// root, else the folder holding its `agents/` folder, else `source` itself.
fn place(source: &Path) -> &Path {
    tui::repository_root(source)
        .or_else(|| {
            source
                .parent()
                .filter(|agents| agents.file_name() == Some(OsStr::new("agents")))
                .and_then(Path::parent)
        })
        .unwrap_or(source)
}

/// Setup where there are no `agents`; otherwise what `ask` picks by index: changing one of them,
/// listed first, or setting up another.
fn task(agents: &[String], ask: impl FnOnce(&str, &[&str]) -> Result<usize, agent::Error>) -> CommandResult<Task> {
    if agents.is_empty() {
        return Ok(Task::Setup);
    }
    let options: Vec<String> = agents
        .iter()
        .map(|name| format!("Change {name:?}"))
        .chain(["Set up another agent".to_owned()])
        .collect();
    let options: Vec<&str> = options.iter().map(String::as_str).collect();
    let question = if agents.len() == 1 {
        "This directory already has an agent. What should your coding agent do?"
    } else {
        "This directory already has agents. What should your coding agent do?"
    };
    let index = ask(question, &options)?;
    Ok(agents.get(index).map_or(Task::Setup, |name| Task::Change(name.clone())))
}

/// What is said of the agent `name` found after the coding agent did `task`.
fn ready(task: &Task, name: &str) -> String {
    match task {
        Task::Setup => format!("Your agent {name:?} is set up."),
        Task::Change(_) => format!("Your agent {name:?} is ready."),
    }
}

/// What the coding agent is asked for `task`.
fn request(task: &Task, directory: &Path, in_repository: bool) -> String {
    match task {
        Task::Setup => handover_prompt(directory, in_repository),
        Task::Change(name) => change_prompt(name, directory, in_repository),
    }
}

/// What the coding agent is asked when it changes the agent `name`, as [`handover_prompt`] asks
/// for setup.
fn change_prompt(name: &str, directory: &Path, in_repository: bool) -> String {
    let directory = directory.display();
    let subject = if in_repository {
        format!("for this repository, {directory}")
    } else {
        format!("for this folder, {directory}")
    };
    format!(
        "Change the agentctl agent {name:?} {subject}. Run `agentctl guide change` and follow it; this session is only \
         for that."
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

/// The one harness found, or the one `ask` picks by index when there are several, asked for `task`.
fn choose(
    found: &[(Harness, PathBuf)],
    task: &Task,
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
            let question = match task {
                Task::Setup => "Which coding agent should help you set up the agent?".to_owned(),
                Task::Change(agent) => format!("Which coding agent should help you change the agent {agent:?}?"),
            };
            let index = ask(&question, &names)?;
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
            "Set up an agentctl agent for this repository, /work/app. Run `agentctl guide setup` and follow it; this session is only for that."
        );
        assert_eq!(
            handover_prompt(directory, false),
            "Set up an agentctl agent for this folder, /work/app. Run `agentctl guide setup` and follow it; this session is only for that."
        );
    }

    fn agent(name: &str, reason: &str, ready: bool, failure: Option<agent::FailureKind>) -> Agent {
        let mut agent: Agent =
            serde_yaml_ng::from_str(include_str!("../../../tests/fixtures/agent.yaml")).expect("test manifest");
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
    fn an_agents_files_that_agentd_does_not_run_can_start_or_go_to_the_coding_agent() {
        for (picked, start) in [(0, true), (1, false)] {
            let mut asked = (String::new(), Vec::new());
            let started = from_files(Path::new("agents/notes/agent.yaml"), |question, options| {
                asked = (
                    question.to_owned(),
                    options.iter().map(|option| (*option).to_owned()).collect(),
                );
                Ok(picked)
            })
            .expect("answered");
            assert_eq!(started, start);
            assert_eq!(
                asked,
                (
                    "The agent in agents/notes/agent.yaml isn't running. What should happen?".to_owned(),
                    vec![
                        "Start it from its files".to_owned(),
                        "Have your coding agent change it first".to_owned()
                    ]
                )
            );
        }
    }

    #[test]
    fn after_the_coding_agent_a_session_is_offered_in_an_agent_ready_or_on_its_way() {
        let ready = agent("dev", "SandboxReady", true, None);
        assert_eq!(onward(&Ok(ready), &crate::Unapplied::None), Onward::Offer("dev".into()));
        let starting = agent("dev", agent::Condition::REASON_STARTING, false, None);
        assert_eq!(
            onward(&Ok(starting), &crate::Unapplied::None),
            Onward::Offer("dev".into())
        );
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
            assert_eq!(
                onward(&Ok(failed), &crate::Unapplied::None),
                Onward::Failed("dev".into()),
                "{failure:?}"
            );
        }
        assert_eq!(
            next_step(&Onward::Failed("dev".into()), true),
            "Your agent \"dev\" is not running; `agentctl status` shows why and what to do next."
        );
    }

    #[test]
    fn after_the_coding_agent_no_agent_several_or_no_agentd_say_where_to_go() {
        // agentd's codes for no agent here, and for several.
        assert_eq!(onward(&rpc(-32004), &crate::Unapplied::None), Onward::Nothing);
        assert_eq!(
            onward(
                &rpc(-32004),
                &crate::Unapplied::Written {
                    name: "dev".into(),
                    manifest: "agents/dev/agent.yaml".into()
                }
            ),
            Onward::Written("dev".into())
        );
        assert_eq!(
            next_step(&Onward::Written("dev".into()), true),
            "agents/dev has an agent that isn't running; `agentctl apply -f agents/dev/agent.yaml` starts it from \
             its files."
        );
        assert_eq!(onward(&rpc(-32602), &crate::Unapplied::None), Onward::Several);
        let unreachable = onward(
            &Err(Error::Daemon("automatic startup exited".into())),
            &crate::Unapplied::None,
        );
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
    fn the_change_prompt_names_the_agent_the_repository_or_the_directory_and_the_guide() {
        let directory = Path::new("/work/app");
        assert_eq!(
            change_prompt("dev", directory, true),
            "Change the agentctl agent \"dev\" for this repository, /work/app. Run `agentctl guide change` and follow it; this session is only for that."
        );
        assert_eq!(
            change_prompt("dev", directory, false),
            "Change the agentctl agent \"dev\" for this folder, /work/app. Run `agentctl guide change` and follow it; this session is only for that."
        );
    }

    #[test]
    fn the_coding_agent_is_asked_for_the_task_chosen() {
        let directory = Path::new("/work/app");
        assert_eq!(request(&Task::Setup, directory, true), handover_prompt(directory, true));
        assert_eq!(
            request(&Task::Change("dev".into()), directory, false),
            change_prompt("dev", directory, false)
        );
    }

    #[test]
    fn without_an_agent_here_setup_follows_without_asking() {
        let never = |_: &str, _: &[&str]| -> Result<usize, agent::Error> { panic!("asked") };
        assert_eq!(task(&[], never).expect("setup"), Task::Setup);
    }

    #[test]
    fn with_an_agent_here_the_person_chooses_to_change_it_or_set_up_another() {
        let dev = ["dev".to_owned()];
        for (picked, expected) in [(0, Task::Change("dev".into())), (1, Task::Setup)] {
            let mut asked = (String::new(), Vec::new());
            let chosen = task(&dev, |question, options| {
                asked = (
                    question.to_owned(),
                    options.iter().map(|option| (*option).to_owned()).collect(),
                );
                Ok(picked)
            })
            .expect("one agent");
            assert_eq!(chosen, expected);
            assert_eq!(
                asked.0,
                "This directory already has an agent. What should your coding agent do?"
            );
            assert_eq!(asked.1, ["Change \"dev\"", "Set up another agent"]);
        }
    }

    #[test]
    fn with_several_agents_here_each_is_offered_by_name() {
        let agents = ["api".to_owned(), "web".to_owned()];
        let mut asked = Vec::new();
        let chosen = task(&agents, |question, options| {
            assert!(question.contains("already has agents"), "{question}");
            asked = options.iter().map(|option| (*option).to_owned()).collect();
            Ok(1)
        })
        .expect("several");
        assert_eq!(chosen, Task::Change("web".into()));
        assert_eq!(asked, ["Change \"api\"", "Change \"web\"", "Set up another agent"]);
        assert_eq!(task(&agents, |_, _| Ok(2)).expect("another"), Task::Setup);
    }

    fn applied_from(name: &str, source: &str) -> Agent {
        let mut agent = agent(name, "SandboxReady", true, None);
        agent.status.provenance = Some(agent::Provenance {
            source_directory: source.into(),
            manifest_path: None,
            env_file: None,
        });
        agent
    }

    #[test]
    fn the_agents_here_are_the_one_resolved_or_those_tied_for_the_directory() {
        let directory = Path::new("/work/app");
        let dev = agent("dev", "SandboxReady", true, None);
        assert_eq!(existing(&Ok(dev), &[], directory), ["dev"]);
        assert_eq!(existing(&rpc(-32004), &[], directory), Vec::<String>::new());
        assert_eq!(
            existing(&Err(Error::Daemon("down".into())), &[], directory),
            Vec::<String>::new()
        );
        let mut listed = [
            applied_from("web", "/work/app/agents/web"),
            applied_from("other", "/work/other/agents/other"),
            applied_from("api", "/work/app/agents/api"),
            applied_from("projects", "/work/agents/projects"),
            applied_from("unrecorded", "/work/app/agents/unrecorded"),
        ];
        listed[4].status.provenance = None;
        // The folder holding `agents/` is each one's place; the closest place to the directory wins.
        assert_eq!(existing(&rpc(-32602), &listed, directory), ["api", "web"]);
        assert_eq!(existing(&rpc(-32602), &listed, Path::new("/work/docs")), ["projects"]);
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
        assert_eq!(choose(&[], &Task::Setup, never).expect("none"), None);
        let codex = (Harness::Codex, PathBuf::from("/bin/codex"));
        assert_eq!(
            choose(std::slice::from_ref(&codex), &Task::Setup, never).expect("one"),
            Some(codex.clone())
        );

        let pi = (Harness::Pi, PathBuf::from("/bin/pi"));
        let mut asked = Vec::new();
        let chosen = choose(&[codex, pi.clone()], &Task::Setup, |_, names| {
            asked = names.iter().map(|name| (*name).to_owned()).collect();
            Ok(1)
        })
        .expect("several");
        assert_eq!(asked, ["Codex", "Pi"]);
        assert_eq!(chosen, Some(pi));
    }

    #[test]
    fn a_change_is_named_as_one_in_the_questions_around_the_coding_agent() {
        let mut questions = Vec::new();
        for task in [Task::Setup, Task::Change("dev".into())] {
            let found = [
                (Harness::Codex, PathBuf::from("/bin/codex")),
                (Harness::Pi, PathBuf::from("/bin/pi")),
            ];
            choose(&found, &task, |question, _| {
                questions.push(question.to_owned());
                Ok(0)
            })
            .expect("several");
        }
        assert_eq!(
            questions,
            [
                "Which coding agent should help you set up the agent?",
                "Which coding agent should help you change the agent \"dev\"?"
            ]
        );
        assert_eq!(ready(&Task::Setup, "dev"), "Your agent \"dev\" is set up.");
        assert_eq!(
            ready(&Task::Change("dev".into()), "dev"),
            "Your agent \"dev\" is ready."
        );
    }
}
