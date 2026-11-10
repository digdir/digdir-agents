//! `agentctl update`: bringing the agent definitions in a checkout up to date with this version of
//! agentctl, with the agent builder in its `update` session.
//!
//! The builder works out what changed since each definition's version, proposes the changes and
//! writes them once the person agrees, as its instructions say. What is particular to updating is
//! only how the session opens, and what must hold before it does: the definitions read nothing
//! from an environment file, since moving off it is the person's to do.

use std::{
    collections::BTreeSet,
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
pub(crate) async fn update(arguments: UpdateArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    if !prompt::interactive() {
        return Err(CommandError::Message(
            "updating agent definitions is a conversation with the agent builder, which needs a terminal".into(),
        ));
    }
    let folder = builder::folder(arguments.directory)?;
    // The builder gets the whole repository, since its definitions are found anywhere in it.
    let checkout = agent::local::repository_root(&folder).unwrap_or(&folder).to_path_buf();
    if agent::local::home::user_home_directory().is_some_and(|home| home == checkout) {
        return Err(CommandError::Message(
            "the agent builder would get your whole home folder. To update a personal agent, name its folder, \
             such as `agentctl update ~/agents/NAME`"
                .into(),
        ));
    }
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
    if let Some(refusal) = environment_file_refusal(&checkout, &definitions) {
        return Err(CommandError::Message(refusal));
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

/// Why the `definitions` in `checkout` cannot be updated yet, when any reads an environment file:
/// moving its values is the person's to do, as the guide describes, and the builder may not mount
/// one beside a manifest.
fn environment_file_refusal(checkout: &Path, definitions: &[DefinitionSummary]) -> Option<String> {
    let mut lines = definitions
        .iter()
        .filter(|definition| !definition.environment_file_variables.is_empty())
        .map(|definition| {
            format!(
                "  {} reads {} from its environment file",
                relative(checkout, &definition.path),
                definition.environment_file_variables.join(", ")
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
    (!lines.is_empty()).then(|| {
        format!(
            "move these definitions off the environment file before updating them, as {} describes, then run \
             `agentctl update` again:\n{}",
            agent::exposed::ENVIRONMENT_FILE_GUIDE,
            lines.join("\n")
        )
    })
}

/// `path` as the person knows it in `checkout`.
fn relative(checkout: &Path, path: &Path) -> String {
    path.strip_prefix(checkout).unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use agent::control_plane::DefinitionSummary;

    use super::environment_file_refusal;

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
            environment_file_refusal(checkout, &[definition("/src/app/agents/dev/agent.yaml"), unreadable]),
            None
        );
    }

    #[test]
    fn every_definition_that_reads_an_environment_file_is_named_once() {
        let checkout = Path::new("/src/app");
        let file = PathBuf::from("/src/app/agents/dev/.env");
        let reads = DefinitionSummary {
            environment_file_variables: vec!["GITHUB_TOKEN".into()],
            environment_file: Some(file.clone()),
            ..definition("/src/app/agents/dev/agent.yaml")
        };
        let variant = DefinitionSummary {
            environment_file: Some(file),
            ..definition("/src/app/agents/dev/agent.mine.yaml")
        };
        let refusal = environment_file_refusal(
            checkout,
            &[reads, variant, definition("/src/app/agents/other/agent.yaml")],
        )
        .expect("a refusal");
        assert!(refusal.contains("agents/dev/agent.yaml reads GITHUB_TOKEN"));
        assert_eq!(refusal.matches("agents/dev/.env").count(), 1);
        assert!(!refusal.contains("agent.mine.yaml") && !refusal.contains("agents/other"));
    }
}
