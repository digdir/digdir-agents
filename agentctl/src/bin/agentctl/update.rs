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

use agent::{
    control_api::Client, control_plane::DefinitionSummary, definitions::Detail, local::home::ControlPlaneHome,
    release::Release,
};
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
    /// Delete the agent builder when this command ends, such as when you leave its session,
    /// instead of keeping it to go back to. What it wrote stays.
    #[arg(long, conflicts_with = "print_task")]
    rm: bool,
}

/// Updates the definitions in the checkout with the builder, starting it there or going back to it,
/// or prints the update for the agent that ran it.
pub(crate) async fn update(arguments: UpdateArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    if !arguments.print_task && !prompt::interactive() {
        let folder = arguments
            .directory
            .map(|directory| format!(" {}", directory.display()))
            .unwrap_or_default();
        return Err(CommandError::Message(format!(
            "updating agent definitions is a conversation with the agent builder, which needs a terminal. An agent \
             can do the update itself: run `agentctl update --print-task{folder}` for what to do"
        )));
    }
    let folder = builder::folder(arguments.directory)?;
    // Definitions are found anywhere in the repository, so the update covers all of it.
    let repository = agent::local::repository_root(&folder);
    let checkout = repository.unwrap_or(&folder).to_path_buf();
    // Outside a repository, agents are found from the folder that holds `agents/`, so a folder of
    // agents, such as the personal agents in `~/agents`, is looked at from there.
    let from = match checkout.parent() {
        Some(parent) if repository.is_none() && checkout.file_name() == Some("agents".as_ref()) => parent,
        _ => &checkout,
    };
    let definitions = client
        .definitions(Some(from.to_path_buf()), Detail::Update)
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
        // Inside an agent, the version its agents run with is that of the agentd that runs it, which
        // an agentctl its image holds may predate.
        let version = agent::sandbox::platform::host_version().unwrap_or_else(|| agent::build_version().to_owned());
        print!(
            "{}",
            task(&checkout.display().to_string(), &checkout, &definitions, &version)
        );
        return Ok(());
    }
    if agent::local::home::user_home_directory().is_some_and(|home| home == checkout) {
        return Err(CommandError::Message(
            "the agent builder would get your whole home folder. To update a personal agent, name its folder, \
             such as `agentctl update ~/agents/NAME`"
                .into(),
        ));
    }
    let builder = Builder::bundled()?;
    let mut task = Task {
        session: "update",
        prompt: task("this checkout", &checkout, &definitions, agent::build_version()),
        moves_off: true,
    };
    builder::note_git_outside(&checkout, &mut task);
    let existing = builder.find(client, &checkout).await?;
    let name = match existing {
        Some(existing) if arguments.rm => return Err(builder::kept_already("update", &checkout, &existing)),
        Some(existing) if builder.current(&existing) => {
            builder.resume(home, client, &existing, &checkout, &task).await?;
            existing.metadata.name
        }
        existing => {
            // Every question comes before anything changes, and a builder of another version goes
            // only once the folders are agreed to, so that a no or a refusal leaves it where it is.
            if let Some(existing) = &existing {
                let name = &existing.metadata.name;
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
            }
            let agreed = builder.agree(client, &checkout, &mut task).await?;
            if let Some(existing) = existing {
                client.delete(&existing.metadata.name).await?;
            }
            builder
                .start(home, client, &checkout, &task, agreed, arguments.rm)
                .await?
        }
    };
    if arguments.rm {
        println!("The agent builder is deleted. Review what it changed with `git diff` and commit it.");
    } else {
        println!(
            "The agent builder keeps running, and `agentctl update` in {} goes back to it.\nReview what it changed \
             with `git diff` and commit it. Run `agentctl delete agent/{name}` once you are done with it.",
            checkout.display()
        );
    }
    Ok(())
}

/// The update of the `definitions` in `checkout`, which the task calls `location`, to `version`, for
/// the agent builder or another agent to do as the skill every agent has describes. Moving off an
/// environment file is part of it.
fn task(location: &str, checkout: &Path, definitions: &[DefinitionSummary], version: &str) -> String {
    let mut task = format!(
        "Bring the agent definitions in {location} up to date with agentctl {version}. Follow \"Bringing a \
         definition up to date\" in the `agent-definition` skill: work out the version each was written for, read \
         the release notes since then, and propose each change, with its reason, before you write it.\n"
    );
    let listed = definitions
        .iter()
        .map(|definition| {
            let path = relative(checkout, &definition.path);
            // Changes to a file Git ignores, such as a personal variant, don't show in `git diff`.
            if definition.ignored {
                format!("  {path}, which Git ignores")
            } else {
                format!("  {path}")
            }
        })
        .collect();
    let unreadable = definitions
        .iter()
        .filter_map(|definition| {
            let error = definition.error.as_deref()?;
            Some(format!(
                "  {}: {}",
                relative(checkout, &definition.path),
                error.lines().next().unwrap_or_default()
            ))
        })
        .collect();
    let mut reading = environment_file_variables(checkout, definitions);
    reading.extend(environment_files(definitions).into_iter().map(|file| {
        format!(
            "  {} is the environment file of the definitions beside it",
            relative(checkout, file)
        )
    }));
    let sections: [(String, Vec<String>); 4] = [
        ("The definitions:".into(), listed),
        (
            "Some cannot be read by this version, which updating them fixes:".into(),
            unreadable,
        ),
        (
            format!("The platform's images they use, where those are not of {version}:"),
            platform_images(checkout, definitions, version),
        ),
        (
            "Some still read an environment file, which a later release stops reading. Moving them off it is one of \
             the changes, as that section says:"
                .into(),
            reading,
        ),
    ];
    for (heading, lines) in sections.into_iter().filter(|(_, lines)| !lines.is_empty()) {
        let _ = writeln!(task, "{heading}");
        for line in lines {
            let _ = writeln!(task, "{line}");
        }
    }
    task
}

/// The lines naming the `definitions` in `checkout` that use images of the platform's from another
/// release than `version`, or that name none, grouped by those images.
fn platform_images(checkout: &Path, definitions: &[DefinitionSummary], version: &str) -> Vec<String> {
    let target = Release::of_build(version);
    let mut using = BTreeMap::<Vec<String>, Vec<String>>::new();
    for definition in definitions {
        let images = definition
            .platform_images
            .iter()
            .filter_map(|image| {
                let shown = image.reference.split_once("@sha256:").map_or_else(
                    || image.reference.clone(),
                    |(named, digest)| format!("{named}@sha256:{}…", digest.get(..12).unwrap_or(digest)),
                );
                let release = image.release.as_deref().and_then(Release::parse);
                match (release, target) {
                    (None, _) => Some(format!("{shown} names no release")),
                    (Some(release), Some(target)) if release == target => None,
                    (Some(release), Some(target)) if release < target => {
                        Some(format!("{shown} is {release}, older than {target}"))
                    }
                    (Some(release), Some(target)) => Some(format!("{shown} is {release}, later than {target}")),
                    (Some(release), None) => Some(format!("{shown} is {release}")),
                }
            })
            .collect::<Vec<_>>();
        if !images.is_empty() {
            using
                .entry(images)
                .or_default()
                .push(relative(checkout, &definition.path));
        }
    }
    using
        .into_iter()
        .map(|(images, definitions)| {
            format!(
                "  {}: {}",
                agent::exposed::sentence_list(&definitions),
                images.join("; ")
            )
        })
        .collect()
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
            platform_images: Vec::new(),
            ignored: false,
        }
    }

    #[test]
    fn the_task_names_every_definition_and_the_version_to_bring_them_to() {
        let checkout = Path::new("/src/app");
        let personal = DefinitionSummary {
            ignored: true,
            ..definition("/src/app/agents/dev/agent.mine.yaml")
        };
        let unreadable = DefinitionSummary {
            name: None,
            error: Some("agent.yaml: unknown field `mounts`\nat line 3".into()),
            ..definition("/src/app/agents/old/agent.yaml")
        };

        let task = task(
            "this checkout",
            checkout,
            &[definition("/src/app/agents/dev/agent.yaml"), personal, unreadable],
            "v0.1.0-preview.14",
        );

        assert!(
            task.starts_with(
                "Bring the agent definitions in this checkout up to date with agentctl v0.1.0-preview.14."
            ),
            "{task}"
        );
        assert!(
            task.contains(
                "The definitions:\n  agents/dev/agent.yaml\n  agents/dev/agent.mine.yaml, which Git ignores\n  \
                 agents/old/agent.yaml\n"
            ),
            "{task}"
        );
        assert!(
            task.ends_with("fixes:\n  agents/old/agent.yaml: agent.yaml: unknown field `mounts`\n"),
            "{task}"
        );
        assert!(!task.contains("environment file"));
    }

    #[test]
    fn the_task_names_the_platforms_images_that_are_not_of_the_version() {
        let checkout = Path::new("/src/app");
        let image = |reference: &str, release: Option<&str>| agent::definitions::PlatformImage {
            reference: reference.into(),
            release: release.map(str::to_owned),
        };
        let using = |path: &str, images: Vec<agent::definitions::PlatformImage>| DefinitionSummary {
            platform_images: images,
            ..definition(path)
        };
        let old = || {
            vec![
                image(
                    "ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:2ee51d4619a9e13b1d1e",
                    None,
                ),
                image("ghcr.io/digdir/agentctl:v0.1.0-preview.13", Some("v0.1.0-preview.13")),
            ]
        };

        let task = task(
            "this checkout",
            checkout,
            &[
                using("/src/app/agents/minimal/agent.nested-build.yaml", old()),
                using("/src/app/agents/desktop/agent.nested-build.yaml", old()),
                using(
                    "/src/app/agents/current/agent.yaml",
                    vec![image(
                        "ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.14",
                        Some("v0.1.0-preview.14"),
                    )],
                ),
            ],
            "v0.1.0-preview.14",
        );

        let (_, images) = task.split_once("not of v0.1.0-preview.14:\n").expect("the images part");
        assert_eq!(
            images.lines().collect::<Vec<_>>(),
            [
                "  agents/minimal/agent.nested-build.yaml and agents/desktop/agent.nested-build.yaml: \
                 ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:2ee51d4619a9… names no release; \
                 ghcr.io/digdir/agentctl:v0.1.0-preview.13 is v0.1.0-preview.13, older than v0.1.0-preview.14"
            ]
        );
    }

    #[test]
    fn a_build_of_main_is_compared_by_the_release_it_extends_and_a_local_one_by_none() {
        let checkout = Path::new("/src/app");
        let definitions = [
            DefinitionSummary {
                platform_images: vec![agent::definitions::PlatformImage {
                    reference: "ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.14".into(),
                    release: Some("v0.1.0-preview.14".into()),
                }],
                ..definition("/src/app/agents/current/agent.yaml")
            },
            DefinitionSummary {
                platform_images: vec![agent::definitions::PlatformImage {
                    reference: "ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.13".into(),
                    release: Some("v0.1.0-preview.13".into()),
                }],
                ..definition("/src/app/agents/old/agent.yaml")
            },
        ];
        let images = |version: &str| {
            let task = task("this checkout", checkout, &definitions, version);
            let (_, images) = task
                .split_once(&format!("not of {version}:\n"))
                .expect("the images part");
            images.lines().map(str::to_owned).collect::<Vec<_>>()
        };

        assert_eq!(
            images("v0.1.0-preview.14.main.gabc1234"),
            [
                "  agents/old/agent.yaml: ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.13 is \
              v0.1.0-preview.13, older than v0.1.0-preview.14"
            ]
        );
        assert_eq!(
            images("v0.0.1-dev.20261108"),
            [
                "  agents/old/agent.yaml: ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.13 is \
                 v0.1.0-preview.13",
                "  agents/current/agent.yaml: ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.14 is \
                 v0.1.0-preview.14",
            ],
            "a local build extends no release, so each image's is named"
        );
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
            "v0.1.0-preview.14",
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
