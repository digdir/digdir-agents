//! `agentctl init`: hands setting up an agent to the coding agent the person already uses here,
//! which does it by reading `agentctl guide setup`.

use std::{
    ffi::OsStr,
    io::IsTerminal as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use agent::Harness;

use crate::{CommandError, CommandResult, launch, prompt, tui};

/// Starts a coding agent found on `PATH` in the repository holding the current directory, on the
/// handover prompt, and exits with its exit code. Without one, prints the prompt to paste.
pub(crate) fn run() -> CommandResult<ExitCode> {
    require_terminal(std::io::stdin().is_terminal(), std::io::stdout().is_terminal())?;
    let current = std::env::current_dir().map_err(agent::Error::from)?;
    let root = tui::repository_root(&current);
    let directory = root.unwrap_or(&current);
    let handover = handover_prompt(directory, root.is_some());
    let found = installed(std::env::var_os("PATH").as_deref());
    let Some((harness, program)) = choose(&found, prompt::choose)? else {
        let names = Harness::ALL.map(Harness::display_name).join(", ");
        eprintln!("None of {names} is installed here. Paste this into the coding agent you use:\n");
        println!("{handover}");
        return Ok(ExitCode::SUCCESS);
    };
    let name = harness.display_name();
    eprintln!("Starting {name} to set up an agent with you.");
    let mut command = Command::new(program);
    command.args(harness.host_arguments(&handover)).current_dir(directory);
    crate::run_in_place(command).map_err(|error| CommandError::Message(format!("could not start {name}: {error}")))
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
         this machine. Run `agentctl guide setup` and follow it. Start by asking me what I want the agent to do."
    )
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
             you in sandboxed Linux VMs on this machine. Run `agentctl guide setup` and follow it. Start by asking \
             me what I want the agent to do."
        );
        let outside = handover_prompt(directory, false);
        assert!(
            outside.starts_with("Help me set up an agentctl agent for my work in /work/app. agentctl runs"),
            "{outside}"
        );
        assert!(!outside.contains("repository"), "{outside}");
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
