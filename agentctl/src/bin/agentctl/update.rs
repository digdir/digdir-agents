//! `agentctl update`: bringing the agent definitions in a checkout up to date with this version of
//! agentctl, with the agent builder in its `update` session.
//!
//! The builder works out what changed since each definition's version, proposes the changes and
//! writes them once the person agrees, as the `agent-definition` skill says. What is particular to
//! updating is only how the session opens, and what must hold before it does: the definitions read
//! nothing from an environment file, since moving off it is the person's to do.
//!
//! An agent that runs it, as people ask the agent their definitions make to, has no terminal and
//! follows the same skill itself, so it gets the task as the builder's session would open with it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use agent::{control_api::Client, control_plane::DefinitionSummary, local::home::ControlPlaneHome};
use clap::Args;

use crate::{
    CommandError, CommandResult,
    builder::{self, Builder, Task},
    prompt,
};

#[derive(Args)]
pub(crate) struct UpdateArguments {
    /// The checkout whose agent definitions to update. Defaults to the repository of the current
    /// folder.
    directory: Option<PathBuf>,
}

/// Updates the definitions in the checkout with the builder, starting it there or going back to it.
/// Without a terminal, it prints the update for the agent that ran it to do instead.
pub(crate) async fn update(arguments: UpdateArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    let folder = builder::folder(arguments.directory)?;
    // Definitions are found anywhere in the repository, so the update covers all of it.
    let checkout = agent::local::repository_root(&folder).unwrap_or(&folder).to_path_buf();
    let definitions = client
        .definitions(Some(checkout.clone()))
        .await?
        .into_iter()
        .filter(|definition| definition.reference.is_none() && definition.path.starts_with(&checkout))
        .collect::<Vec<_>>();
    if definitions.is_empty() {
        return Err(CommandError::Message(format!(
            "{} holds no agent definitions to update. To make one, run `agentctl new` there",
            checkout.display()
        )));
    }
    let reading = environment_file_readers(&checkout, &definitions);
    if !prompt::interactive() {
        print!("{}", task(&checkout, &definitions, &reading));
        return Ok(());
    }
    if agent::local::home::user_home_directory().is_some_and(|home| home == checkout) {
        return Err(CommandError::Message(
            "the agent builder would get your whole home folder. To update a personal agent, name its folder, \
             such as `agentctl update ~/agents/NAME`"
                .into(),
        ));
    }
    if !reading.is_empty() {
        return Err(CommandError::Message(format!(
            "move these definitions off the environment file before updating them, as {} describes, then run \
             `agentctl update` again:\n{}",
            agent::exposed::ENVIRONMENT_FILE_GUIDE,
            reading.join("\n")
        )));
    }

    let builder = Builder::bundled()?;
    let task = Task {
        session: "update",
        prompt: format!(
            "Bring the agent definitions in the checkout you have up to date with agentctl {}: {}.",
            agent::build_version(),
            definitions
                .iter()
                .map(|definition| relative(&checkout, &definition.path))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let name = match builder.find(client, &checkout).await? {
        Some(existing) if builder.current(&existing) => {
            eprintln!("Back to the agent builder on {}.", checkout.display());
            builder.resume(home, client, &existing, &task).await?;
            existing.metadata.name
        }
        Some(existing) => {
            let name = existing.metadata.name;
            let started_by = format!(
                "agent/{name}, the agent builder on {}, was started by another version of agentctl. It keeps \
                 the image it was made with, so it would try the definitions with that version's agentctl.",
                checkout.display()
            );
            if !prompt::confirm_lines(&[
                &started_by,
                "Delete it and start one with this version's? Its conversations end, and what it wrote stays \
                 where it is.",
            ])? {
                return Err(CommandError::Message(format!(
                    "kept agent/{name}. Updating takes an agent builder of this version"
                )));
            }
            client.delete(&name).await?;
            builder.start(home, client, &checkout, &task).await?
        }
        None => builder.start(home, client, &checkout, &task).await?,
    };
    println!(
        "The agent builder keeps running, and `agentctl update` in {} goes back to it.\nReview what it changed with \
         `git diff` and commit it. Run `agentctl delete {name}` once you are done with it.",
        checkout.display()
    );
    Ok(())
}

/// The update of the `definitions` in `checkout` for an agent to do itself, as the skill every agent
/// has describes, with the ones `reading` an environment file, which it moves off it first.
///
/// The version is the one the agents run with, which inside an agent is its host's, not that of
/// the agentctl its image may hold.
fn task(checkout: &Path, definitions: &[DefinitionSummary], reading: &[String]) -> String {
    let mut task = format!(
        "Bring the agent definitions in {} up to date with agentctl {} or, inside an agent, the agentd its agent \
         sandbox section names. Follow \"Bringing a definition up to date\" in the `agent-definition` skill: work \
         out the version each was written for, read the release notes since then, and propose each change, with its \
         reason, before you write it. The definitions:\n",
        checkout.display(),
        agent::build_version()
    );
    for definition in definitions {
        let _ = writeln!(task, "  {}", relative(checkout, &definition.path));
    }
    if !reading.is_empty() {
        task.push_str(
            "Some still read an environment file, which a later release stops reading. Moving them off it is one of \
             the changes, as that section says:\n",
        );
        for line in reading {
            task.push_str(line);
            task.push('\n');
        }
    }
    task
}

/// The lines naming the `definitions` in `checkout` that read an environment file, which a later
/// release stops reading: the variables they take from one, and the `.env` beside them.
fn environment_file_readers(checkout: &Path, definitions: &[DefinitionSummary]) -> Vec<String> {
    // A variant reads what the manifest it extends does, so the manifests of a folder that read the
    // same variables share a line.
    let mut reading = BTreeMap::<(&Path, &[String]), Vec<String>>::new();
    for definition in definitions
        .iter()
        .filter(|definition| !definition.environment_file_variables.is_empty())
    {
        let folder = definition.path.parent().unwrap_or(checkout);
        let file = definition
            .path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        reading
            .entry((folder, &definition.environment_file_variables))
            .or_default()
            .push(file);
    }
    let mut lines = reading
        .into_iter()
        .map(|((folder, variables), files)| {
            let folder = relative(checkout, folder);
            let files = files
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    if index == 0 && !folder.is_empty() {
                        format!("{folder}/{file}")
                    } else {
                        file.clone()
                    }
                })
                .collect::<Vec<_>>();
            format!(
                "  {} {} {} from the environment file",
                agent::exposed::sentence_list(&files),
                if files.len() == 1 { "reads" } else { "read" },
                variables.join(", ")
            )
        })
        .collect::<Vec<_>>();
    let beside = definitions
        .iter()
        .filter_map(|definition| definition.environment_file.as_deref())
        .collect::<BTreeSet<_>>();
    lines.extend(beside.into_iter().map(|file| {
        format!(
            "  {} is the environment file of the definitions beside it",
            relative(checkout, file)
        )
    }));
    lines
}

/// `path` as the person knows it in `checkout`.
fn relative(checkout: &Path, path: &Path) -> String {
    path.strip_prefix(checkout).unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use agent::control_plane::DefinitionSummary;

    use super::{environment_file_readers, task};

    fn definition(path: &str) -> DefinitionSummary {
        DefinitionSummary {
            path: PathBuf::from(path),
            aliases: Vec::new(),
            reference: None,
            name: Some("worker".into()),
            error: None,
            slots: Vec::new(),
            environment_file_variables: Vec::new(),
            environment_file: None,
        }
    }

    #[test]
    fn definitions_off_the_environment_file_can_be_updated() {
        let checkout = Path::new("/src/app");
        let unreadable = DefinitionSummary {
            name: None,
            error: Some("unknown field".into()),
            ..definition("/src/app/agents/old/agent.yaml")
        };
        assert_eq!(
            environment_file_readers(checkout, &[definition("/src/app/agents/dev/agent.yaml"), unreadable]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_definition_that_reads_an_environment_file_is_named_once() {
        let checkout = Path::new("/src/app");
        let file = PathBuf::from("/src/app/agents/dev/.env");
        let reading = |path: &str, variables: &[&str]| DefinitionSummary {
            environment_file_variables: variables.iter().map(|&variable| variable.to_owned()).collect(),
            environment_file: Some(file.clone()),
            ..definition(path)
        };
        let readers = environment_file_readers(
            checkout,
            &[
                reading("/src/app/agents/dev/agent.yaml", &["GITHUB_TOKEN"]),
                reading("/src/app/agents/dev/agent.nested.yaml", &["GITHUB_TOKEN"]),
                reading("/src/app/agents/dev/agent.mine.yaml", &["GITHUB_TOKEN", "EDITOR"]),
                reading("/src/app/agents/dev/agent.off.yaml", &[]),
                definition("/src/app/agents/other/agent.yaml"),
            ],
        );
        let lines = readers.iter().map(|line| line.trim()).collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("agents/dev/agent.mine.yaml reads GITHUB_TOKEN, EDITOR"))
        );
        assert!(
            lines.contains(&"agents/dev/agent.yaml and agent.nested.yaml read GITHUB_TOKEN from the environment file")
        );
        assert!(lines.iter().any(|line| line.starts_with("agents/dev/.env ")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("agent.off.yaml") || line.contains("agents/other"))
        );
    }

    #[test]
    fn an_agent_is_given_every_definition_and_what_reads_an_environment_file() {
        let checkout = Path::new("/src/app");
        let definitions = [
            definition("/src/app/agents/dev/agent.yaml"),
            definition("/src/app/agents/dev/agent.nested.yaml"),
        ];

        let clean = task(checkout, &definitions, &[]);
        assert!(
            clean.contains("\n  agents/dev/agent.yaml\n  agents/dev/agent.nested.yaml\n"),
            "{clean}"
        );
        assert!(clean.contains(agent::build_version()) && clean.contains("agent sandbox section"));
        assert!(!clean.contains("environment file"));

        let reading = ["  agents/dev/agent.yaml reads GITHUB_TOKEN from the environment file".to_owned()];
        assert!(task(checkout, &definitions, &reading).ends_with(&format!("{}\n", reading[0])));
    }
}
