//! `agentctl update`: bringing the agent definitions in a checkout up to date with this version of
//! agentctl, with the agent builder in its `update` session.
//!
//! The builder works out what changed since each definition's version, proposes the changes and
//! writes them once the person agrees, as the `agent-definition` skill says. What is particular to
//! updating is only the task the session opens with. An agent working in the checkout, which has no
//! terminal for the builder's conversation, follows the same skill itself, so `--print-task` gives
//! it the same task.

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
    /// Print the update for the agent that runs this to do itself, instead of starting the agent
    /// builder, such as an agent working in the checkout.
    #[arg(long)]
    print_task: bool,
}

/// Updates the definitions in the checkout with the builder, starting it there or going back to it,
/// or prints the update for the agent that ran it.
pub(crate) async fn update(arguments: UpdateArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    if !arguments.print_task && !prompt::interactive() {
        return Err(CommandError::Message(
            "updating agent definitions is a conversation with the agent builder, which needs a terminal. An agent \
             can do the update itself: run `agentctl update --print-task` for what to do"
                .into(),
        ));
    }
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
    if arguments.print_task {
        print!("{}", task(&checkout.display().to_string(), &checkout, &definitions));
        return Ok(());
    }
    if agent::local::home::user_home_directory().is_some_and(|home| home == checkout) {
        return Err(CommandError::Message(
            "the agent builder would get your whole home folder. To update a personal agent, name its folder, \
             such as `agentctl update ~/agents/NAME`"
                .into(),
        ));
    }
    // agentd lets no agent read such a file, so the builder cannot be given the checkout until it goes.
    let files = environment_files(&definitions);
    if !files.is_empty() {
        return Err(CommandError::Message(format!(
            "{} {} the values of the agents beside {}, which no agent may read. Move them as {} describes and \
             delete the {}, then run `agentctl update` again",
            agent::exposed::sentence_list(&files.iter().map(|file| relative(&checkout, file)).collect::<Vec<_>>()),
            if files.len() == 1 { "holds" } else { "hold" },
            if files.len() == 1 { "it" } else { "them" },
            agent::exposed::ENVIRONMENT_FILE_GUIDE,
            if files.len() == 1 { "file" } else { "files" },
        )));
    }

    let builder = Builder::bundled()?;
    let task = Task {
        session: "update",
        prompt: task("this checkout", &checkout, &definitions),
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

/// The update of the `definitions` in `checkout`, which the task calls `location`, for the agent
/// builder or another agent to do as the skill every agent has describes. Moving off an environment
/// file is part of it.
///
/// The version is the one the agents run with, which inside an agent is its host's, not that of an
/// agentctl its image may hold.
fn task(location: &str, checkout: &Path, definitions: &[DefinitionSummary]) -> String {
    let mut task = format!(
        "Bring the agent definitions in {location} up to date with agentctl {} or, inside an agent, the agentd its \
         agent sandbox section names. Follow \"Bringing a definition up to date\" in the `agent-definition` skill: \
         work out the version each was written for, read the release notes since then, and propose each change, \
         with its reason, before you write it. The definitions:\n",
        agent::build_version()
    );
    for definition in definitions {
        let _ = writeln!(task, "  {}", relative(checkout, &definition.path));
    }
    let mut reading = environment_file_variables(checkout, definitions);
    reading.extend(environment_files(definitions).into_iter().map(|file| {
        format!(
            "  {} is the environment file of the definitions beside it",
            relative(checkout, file)
        )
    }));
    if !reading.is_empty() {
        task.push_str(
            "Some still read an environment file, which a later release stops reading. Moving them off it is one of \
             the changes, as that section says:\n",
        );
        for line in reading {
            let _ = writeln!(task, "{line}");
        }
    }
    task
}

/// The lines naming the `definitions` in `checkout` that take variables from an environment file.
fn environment_file_variables(checkout: &Path, definitions: &[DefinitionSummary]) -> Vec<String> {
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
    reading
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
        .collect()
}

/// The `.env` files beside the `definitions`, each once.
fn environment_files(definitions: &[DefinitionSummary]) -> BTreeSet<&Path> {
    definitions
        .iter()
        .filter_map(|definition| definition.environment_file.as_deref())
        .collect()
}

/// `path` as the person knows it in `checkout`.
fn relative(checkout: &Path, path: &Path) -> String {
    path.strip_prefix(checkout).unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use agent::control_plane::DefinitionSummary;

    use super::task;

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
    fn the_task_names_every_definition_and_the_version_to_bring_them_to() {
        let checkout = Path::new("/src/app");
        let unreadable = DefinitionSummary {
            name: None,
            error: Some("unknown field".into()),
            ..definition("/src/app/agents/old/agent.yaml")
        };

        let task = task(
            "this checkout",
            checkout,
            &[definition("/src/app/agents/dev/agent.yaml"), unreadable],
        );

        assert!(
            task.starts_with("Bring the agent definitions in this checkout"),
            "{task}"
        );
        assert!(task.contains(agent::build_version()) && task.contains("agent sandbox section"));
        assert!(
            task.ends_with("\n  agents/dev/agent.yaml\n  agents/old/agent.yaml\n"),
            "{task}"
        );
        assert!(!task.contains("environment file"));
    }

    #[test]
    fn moving_off_the_environment_file_is_part_of_the_task_and_names_each_reader_once() {
        let checkout = Path::new("/src/app");
        let file = PathBuf::from("/src/app/agents/dev/.env");
        let reading = |path: &str, variables: &[&str]| DefinitionSummary {
            environment_file_variables: variables.iter().map(|&variable| variable.to_owned()).collect(),
            environment_file: Some(file.clone()),
            ..definition(path)
        };

        let task = task(
            "this checkout",
            checkout,
            &[
                reading("/src/app/agents/dev/agent.yaml", &["GITHUB_TOKEN"]),
                reading("/src/app/agents/dev/agent.nested.yaml", &["GITHUB_TOKEN"]),
                reading("/src/app/agents/dev/agent.mine.yaml", &["GITHUB_TOKEN", "EDITOR"]),
                reading("/src/app/agents/dev/agent.off.yaml", &[]),
                definition("/src/app/agents/other/agent.yaml"),
            ],
        );

        let (_, readers) = task.split_once("environment file").expect("the environment file part");
        let lines = readers.lines().skip(1).map(str::trim).collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{task}");
        assert!(lines.contains(&"agents/dev/agent.mine.yaml reads GITHUB_TOKEN, EDITOR from the environment file"));
        assert!(
            lines.contains(&"agents/dev/agent.yaml and agent.nested.yaml read GITHUB_TOKEN from the environment file")
        );
        assert!(lines.iter().any(|line| line.starts_with("agents/dev/.env ")));
    }
}
