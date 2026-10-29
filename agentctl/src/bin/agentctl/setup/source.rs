//! Where a new agent comes from: a standard agent from the catalog, or a team's from a path or
//! a GitHub address, with the manifest chosen among those it holds, and, for a standard agent
//! whose image cannot be pulled, an image built here instead.

use super::answers::{Answers, Choice};
use super::sizing::Whole;
use super::update::Recorded;
use super::{Draft, SetupArguments, Source, SourceChoice, fetch, image, mounts};
use crate::{CommandError, CommandResult};
use agent::catalog::Catalog;
use agent::{Error, manifest};
use std::path::{Path, PathBuf};

/// The agent, from where it comes from.
pub(super) async fn choose_draft(
    arguments: &SetupArguments,
    answers: &Answers,
    catalog: &Catalog,
    staging: &Path,
    source: SourceChoice,
) -> CommandResult<Draft> {
    match source {
        SourceChoice::Standard => standard_draft(arguments.archetype.as_deref(), answers, catalog, staging),
        SourceChoice::Team => team_source(arguments, answers, staging).await,
        SourceChoice::Existing => Err(CommandError::Message(
            "an existing agent is carried on with, not set up again".into(),
        )),
    }
}

/// A team's agent from `--from`, else from a path or GitHub address the person gives. Without a
/// flag, an address that cannot be read, fetched or chosen from is asked for again, so a typo does
/// not end setup.
pub(super) async fn team_source(arguments: &SetupArguments, answers: &Answers, staging: &Path) -> CommandResult<Draft> {
    loop {
        let from = match &arguments.from {
            Some(from) => from.clone(),
            None => answers.text(
                "--from",
                "Path to the agent manifest or a folder holding one, or its GitHub address:",
                None,
                check_team_source,
            )?,
        };
        match team_draft_from(arguments.variant.as_ref(), answers, staging, &from).await {
            Err(error) if arguments.from.is_none() && answers.interactive() && !answers.went_back() => {
                // What went wrong with the address, without the prefix a manifest's own errors carry.
                match error {
                    CommandError::Agent(Error::Invalid(reason)) | CommandError::Message(reason) => {
                        eprintln!("  {reason}");
                    }
                    other @ CommandError::Agent(_) => eprintln!("  {other}"),
                }
            }
            result => return result,
        }
    }
}

/// Whether `answer` is a source setup can read: a GitHub address in a form it knows, or a path
/// that exists.
pub(super) fn check_team_source(answer: &str) -> Result<(), String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Err("Type a (relative or absolute) path or a URL to a GitHub file/folder.".to_owned());
    }
    let address = ["https://", "http://", "github.com/", "www.github.com/"]
        .iter()
        .any(|prefix| answer.starts_with(prefix));
    if address {
        return fetch::GithubSource::parse(answer).map(|_| ()).ok_or_else(|| {
            "GitHub addresses look like https://github.com/OWNER/REPO//FOLDER[@REF], or are the address of a \
             folder or a manifest as GitHub shows it"
                .to_owned()
        });
    }
    let path = manifest::expand_host_path(Path::new(answer)).map_err(|error| error.to_string())?;
    if path.exists() {
        Ok(())
    } else {
        Err(format!("{} does not exist", path.display()))
    }
}

/// The team's agent at `from`, a GitHub address or a path, its manifest chosen when it holds
/// several, or the one of `variant`.
pub(super) async fn team_draft_from(
    variant: Option<&agent::AgentVariantName>,
    answers: &Answers,
    staging: &Path,
    from: &str,
) -> CommandResult<Draft> {
    if let Some(mut github) = fetch::GithubSource::parse(from) {
        let folder = github.fetch(staging).await?;
        let manifest = choose_manifest(answers, variant, &folder)?;
        let within = manifest.strip_prefix(staging.join("checkout")).unwrap_or(&manifest);
        let address = github.address_of(within);
        record_team_source(answers, &address, &manifest);
        let origin = format!(
            "github.com/{}/{}, {}",
            github.owner,
            github.repository,
            within.display()
        );
        let mut draft = team_draft(&manifest, &origin)?;
        draft.source.repository = Some(mounts::Repository::fetched(
            &staging.join("checkout"),
            &github.repository,
        )?);
        draft.source.recorded = Some(Recorded::Team {
            from: address,
            variant: manifest::variant_from_filename(&manifest).map(|variant| variant.as_str().to_owned()),
        });
        return Ok(draft);
    }
    let manifest = choose_manifest(answers, variant, &manifest::expand_host_path(Path::new(from))?)?;
    answers.record("--from", &[&manifest.display().to_string()]);
    answers.forget("--variant");
    team_draft(&manifest, &manifest.display().to_string())
}

/// The standard agent `id`, else the one the person picks from the catalog.
pub(super) fn standard_draft(
    id: Option<&str>,
    answers: &Answers,
    catalog: &Catalog,
    staging: &Path,
) -> CommandResult<Draft> {
    let archetype = if let Some(id) = id {
        catalog.get(id).ok_or_else(|| {
            Error::Invalid(format!(
                "no standard agent {id:?}; see `agentctl setup --list-archetypes`"
            ))
        })?
    } else {
        {
            let choices: Vec<_> = catalog
                .visible()
                .map(|archetype| {
                    Choice::new(archetype, format!("{}\n{}", archetype.title, archetype.description))
                        .flag("--archetype", &[&archetype.id])
                })
                .collect();
            if choices.is_empty() {
                return Err(CommandError::Message(
                    "this agentctl has no standard agents; start from your team's with --from".into(),
                ));
            }
            answers.choose_shown("--archetype", "Which standard agent?", choices, 0)?
        }
    };
    let manifest_path = catalog.extract(&archetype.id, staging)?;
    let mut agent = manifest::resolve(&manifest_path)?.agent;
    archetype.default_name.clone_into(&mut agent.metadata.name);
    let directory = parent(&manifest_path)?;
    let source = Source {
        origin: format!("the standard agent {}", archetype.id),
        recorded: Some(Recorded::Standard {
            id: archetype.id.clone(),
        }),
        repository: None,
        from_catalog: true,
        image_sources: Some(directory.clone()).filter(|directory| image::buildable(directory)),
        directory,
    };
    Ok(Draft::new(
        agent,
        source,
        Whole::of(archetype.minimum),
        Whole::of(archetype.recommended),
    ))
}

/// Records the flags that fetch `manifest` again from GitHub: the folder it is in, at `address`,
/// and its variant.
pub(super) fn record_team_source(answers: &Answers, address: &str, manifest: &Path) {
    answers.record("--from", &[address]);
    match manifest::variant_from_filename(manifest) {
        Some(variant) => answers.record("--variant", &[variant.as_str()]),
        None => answers.forget("--variant"),
    }
}

/// One manifest out of `path`: the file itself, or one of those a folder holds, chosen by name.
pub(super) fn choose_manifest(
    answers: &Answers,
    variant: Option<&agent::AgentVariantName>,
    path: &Path,
) -> CommandResult<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let mut found = mounts::found_manifests(path);
    if let Some(variant) = variant {
        found.retain(|manifest| manifest::variant_from_filename(manifest).as_ref() == Some(variant));
        if found.is_empty() {
            return Err(CommandError::Message(format!(
                "{} has no variant {variant}",
                path.display()
            )));
        }
    }
    if found.is_empty() {
        return Err(CommandError::Message(format!(
            "{} holds no agent.yaml, in it, its folders or agents/*/",
            path.display()
        )));
    }
    let choices = found
        .into_iter()
        .map(|manifest| {
            let label = manifest_label(path, &manifest);
            Choice::new(manifest, label)
        })
        .collect();
    Ok(answers.choose("--from", "Which agent?", choices, 0)?)
}

/// What a choice of `manifest`, found below `root`, shows: the name of the agent it defines and
/// the file, such as `altinn-minimal-nested (agents/minimal/agent.nested.yaml)`, so variants of
/// one folder tell apart. A manifest that does not resolve shows why.
pub(super) fn manifest_label(root: &Path, manifest: &Path) -> String {
    let file = manifest.strip_prefix(root).unwrap_or(manifest);
    let file: Vec<_> = file
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    let file = file.join("/");
    match manifest::resolve(manifest) {
        Ok(resolved) => format!("{} ({file})", resolved.agent.metadata.name),
        Err(error) => format!("{file}, which does not resolve: {error}"),
    }
}

pub(super) fn team_draft(manifest_path: &Path, origin: &str) -> CommandResult<Draft> {
    let agent = manifest::resolve(manifest_path)?.agent;
    let repository = mounts::Repository::containing(&parent(manifest_path)?);
    let resources = Whole::of_resources(&agent.spec.sandbox.resources);
    let source = Source {
        directory: parent(manifest_path)?,
        origin: origin.to_owned(),
        recorded: Some(Recorded::Team {
            from: std::path::absolute(manifest_path)
                .map_err(Error::from)?
                .display()
                .to_string(),
            variant: None,
        }),
        repository,
        from_catalog: false,
        image_sources: None,
    };
    let minimum = Whole {
        cpus: 1,
        memory_gib: 2,
        disk_gib: resources.disk_gib.min(16),
    };
    Ok(Draft::new(agent, source, minimum, resources))
}

pub(super) fn parent(path: &Path) -> Result<PathBuf, Error> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::Invalid(format!("{} has no parent folder", path.display())))
}

/// Builds a standard agent's image here from its Dockerfile when asked to, or when the published
/// image cannot be pulled and the person agrees. Creating the agent would fail on it otherwise.
pub(super) async fn choose_image(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
) -> CommandResult<()> {
    let (Some(_), sandbox::image::ImageSource::Reference { reference }) =
        (&draft.source.image_sources, &draft.agent.spec.sandbox.image)
    else {
        if arguments.build_image && draft.source.image_sources.is_none() {
            return Err(CommandError::Message(
                "--build-image builds a standard agent's image; this agent's definition says how its image is made"
                    .into(),
            ));
        }
        return Ok(());
    };
    let reference = reference.clone();
    if !arguments.build_image {
        match image::check(&reference).await {
            image::Pull::Available => return Ok(()),
            image::Pull::Unknown(reason) => {
                eprintln!("warning: cannot tell whether {reference} can be pulled: {reason}");
                return Ok(());
            }
            image::Pull::Missing => {
                let build = answers.choose(
                    "--build-image",
                    &format!(
                        "The agent's image cannot be pulled: it is not published, or not public.\n\
                         {reference}\n\
                         Build it on this machine from the standard agent's Dockerfile? That needs Docker and \
                         takes a while."
                    ),
                    vec![
                        Choice::new(true, "Yes, build it here").flag("--build-image", &[]),
                        Choice::new(false, "No, stop"),
                    ],
                    0,
                )?;
                if !build {
                    return Err(CommandError::Message(format!(
                        "the agent needs {reference}; nothing was created"
                    )));
                }
            }
        }
    }
    draft.built_here = Some(reference);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::manifest_label;

    #[test]
    fn a_manifest_choice_names_its_agent_and_its_file() {
        let root = tempfile::tempdir().expect("a folder");
        let folder = root.path().join("agents/minimal");
        std::fs::create_dir_all(folder.join("home")).expect("folders");
        std::fs::write(
            folder.join("agent.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: altinn-minimal}\nspec:\n  sandbox:\n    \
             image: {type: reference, reference: example.com/agent:latest}\n    platform: {os: linux}\n    \
             resources: {cpu: '2', memory: 4Gi, rootFilesystem: {capacity: 20Gi, mode: layered}}\n  \
             home: {source: home}\n  harnesses: [{type: pi}]\n  network: {mode: mediated, allow: all}\n",
        )
        .expect("a manifest");
        std::fs::write(folder.join("agent.broken.yaml"), "kind: Agent\n").expect("a broken manifest");
        assert_eq!(
            manifest_label(root.path(), &folder.join("agent.yaml")),
            "altinn-minimal (agents/minimal/agent.yaml)"
        );
        assert_eq!(
            manifest_label(&folder, &folder.join("agent.yaml")),
            "altinn-minimal (agent.yaml)"
        );
        assert!(
            manifest_label(&folder, &folder.join("agent.broken.yaml"))
                .starts_with("agent.broken.yaml, which does not resolve")
        );
    }
}
