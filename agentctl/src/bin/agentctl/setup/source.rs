//! Where an agent comes from: one the repository setup runs in defines, or a team's from a path or
//! a GitHub address, which setup clones, with the manifest chosen among those it holds. The person's variant of it may exist already, from an earlier
//! setup, in which case setup changes it.

use super::answers::{Answers, Choice};
use super::sizing::Whole;
use super::{Access, Draft, SetupArguments, Source, SourceChoice, fetch};
use crate::{CommandError, CommandResult};
use agent::{Agent, AgentVariantName, Error, manifest};
use std::path::{Path, PathBuf};

/// The agent, from where it comes from, with the person's choices to be saved as `save_as`.
pub(super) async fn choose_draft(
    arguments: &SetupArguments,
    answers: &Answers,
    source: SourceChoice,
    save_as: &AgentVariantName,
) -> CommandResult<Draft> {
    match source {
        SourceChoice::Here => {
            let directory = std::env::current_dir().map_err(Error::from)?;
            let root = crate::discovery::repository_root(&directory).unwrap_or(&directory);
            let found = crate::discovery::working_tree_manifests(&directory);
            let manifest = choose_among(answers, arguments.variant.as_ref(), root, found)?;
            answers.record("--from", &[&manifest.display().to_string()]);
            let save_as = choose_save_as(arguments, answers, &manifest, save_as)?;
            team_draft(&manifest, &manifest.display().to_string(), &save_as)
        }
        SourceChoice::Team => team_source(arguments, answers, save_as).await,
    }
}

/// A team's agent from `--from`, else from a path or GitHub address the person gives. Without a
/// flag, an address that cannot be read, cloned or chosen from is asked for again, so a typo does
/// not end setup.
async fn team_source(
    arguments: &SetupArguments,
    answers: &Answers,
    save_as: &AgentVariantName,
) -> CommandResult<Draft> {
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
        match team_draft_from(arguments, answers, &from, save_as).await {
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

/// The team's agent at `from`, a GitHub address, which is cloned first, or a path, its manifest
/// chosen when it holds several.
async fn team_draft_from(
    arguments: &SetupArguments,
    answers: &Answers,
    from: &str,
    save_as: &AgentVariantName,
) -> CommandResult<Draft> {
    let github = fetch::GithubSource::parse(from);
    let path = if let Some(mut github) = github.clone() {
        let checkout = if let Some(folder) = &arguments.clone_into {
            manifest::expand_host_path(folder)?
        } else {
            let default = std::env::current_dir().map_err(Error::from)?.join(&github.repository);
            let question = format!("Where should setup clone {}/{}?", github.owner, github.repository);
            let default = default.display().to_string();
            let answer = answers.text("--clone-into", &question, Some(&default), |_| Ok(()))?;
            manifest::expand_host_path(Path::new(answer.trim()))?
        };
        answers.record("--from", &[from]);
        github.checkout(&checkout).await?
    } else {
        manifest::expand_host_path(Path::new(from))?
    };
    let manifest = choose_manifest(answers, arguments.variant.as_ref(), &path)?;
    if github.is_none() {
        answers.record("--from", &[&manifest.display().to_string()]);
        answers.forget("--variant");
    }
    let save_as = choose_save_as(arguments, answers, &manifest, save_as)?;
    team_draft(&manifest, &manifest.display().to_string(), &save_as)
}

/// The variant beside `manifest` the person's choices are saved as: `manifest` itself when it is
/// a variant Git ignores, which an earlier setup wrote, else `--save-as` or the person's answer,
/// `default` unless they name another. A variant the team's files hold is not theirs to write.
fn choose_save_as(
    arguments: &SetupArguments,
    answers: &Answers,
    manifest: &Path,
    default: &AgentVariantName,
) -> CommandResult<AgentVariantName> {
    if let Some(variant) = manifest::variant_from_filename(manifest)
        && super::create::git_ignores(manifest) == Some(true)
    {
        return Ok(variant);
    }
    if let Some(save_as) = &arguments.save_as {
        return Ok(save_as.clone());
    }
    let directory = parent(manifest)?;
    let check = |answer: &str| {
        let variant: AgentVariantName = answer.trim().parse().map_err(|error: Error| error.to_string())?;
        let path = directory.join(variant.filename());
        if path.is_file() && super::create::git_ignores(&path) == Some(false) {
            Err(format!("{} is the team's; choose another name", variant.filename()))
        } else {
            Ok(())
        }
    };
    let question = format!(
        "Setup saves your choices as a variant, agent.<variant>.yaml beside {}, which Git should ignore.\nWhich variant?",
        file_name(manifest)?
    );
    let answer = answers.text("--save-as", &question, Some(default.as_ref()), check)?;
    Ok(answer.trim().parse()?)
}

/// One manifest out of `path`: the file itself, or one of those a folder holds, chosen by name.
pub(super) fn choose_manifest(
    answers: &Answers,
    variant: Option<&AgentVariantName>,
    path: &Path,
) -> CommandResult<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    choose_among(answers, variant, path, crate::discovery::manifests_in(path))
}

/// One of the manifests `found` below `root`, of `variant` when one is named, chosen by name.
fn choose_among(
    answers: &Answers,
    variant: Option<&AgentVariantName>,
    root: &Path,
    mut found: Vec<PathBuf>,
) -> CommandResult<PathBuf> {
    if let Some(variant) = variant {
        found.retain(|manifest| manifest::variant_from_filename(manifest).as_ref() == Some(variant));
        if found.is_empty() {
            return Err(CommandError::Message(format!(
                "{} has no variant {variant}",
                root.display()
            )));
        }
    }
    if found.is_empty() {
        return Err(CommandError::Message(format!(
            "{} holds no agent.yaml, in it or in agents/*/",
            root.display()
        )));
    }
    let choices = found
        .into_iter()
        .map(|manifest| {
            let label = manifest_label(root, &manifest);
            Choice::new(manifest, label)
        })
        .collect();
    Ok(answers.choose("--from", "Which agent?", choices, 0)?)
}

/// What a choice of `manifest`, found below `root`, shows: the name of the agent it defines and
/// the file, such as `altinn-minimal-nested (agents/minimal/agent.nested.yaml)`, so variants of
/// one folder tell apart. A manifest that does not resolve shows why.
pub(super) fn manifest_label(root: &Path, manifest: &Path) -> String {
    // Discovery finds manifests by their canonical paths, while the person may name the folder
    // by a relative one, or by one through a symlink.
    let below = manifest
        .strip_prefix(root)
        .ok()
        .or_else(|| manifest.strip_prefix(root.canonicalize().ok()?).ok());
    let file = below.map_or_else(
        || manifest.display().to_string(),
        |below| {
            let parts: Vec<_> = below
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect();
            parts.join("/")
        },
    );
    match manifest::resolve(manifest) {
        Ok(resolved) => format!("{} ({file})", resolved.agent.metadata.name),
        Err(error) => format!("{file}, which does not resolve: {error}"),
    }
}

/// The team's agent `manifest_path` defines, to be made the person's with their variant
/// `save_as` beside it. When that variant exists already, or is the manifest chosen, setup changes
/// it instead.
pub(super) fn team_draft(manifest_path: &Path, origin: &str, save_as: &AgentVariantName) -> CommandResult<Draft> {
    let directory = parent(manifest_path)?;
    let variant = directory.join(save_as.filename());
    if variant.is_file() {
        return variant_draft(&variant);
    }
    let base = manifest::resolve(manifest_path)?.agent;
    let mut agent = base.clone();
    agent.metadata.name = format!("{}-{save_as}", base.metadata.name);
    let source = Source {
        variant,
        extends: file_name(manifest_path)?,
        directory,
        origin: origin.to_owned(),
    };
    let (minimum, recommended) = team_sizes(&base);
    Ok(draft(agent, base, source, minimum, recommended))
}

/// The person's variant `variant`, written before, to change: the agent it resolves to, and the
/// one the manifest it extends resolves to.
fn variant_draft(variant: &Path) -> CommandResult<Draft> {
    let text = std::fs::read_to_string(variant).map_err(Error::from)?;
    let extends = serde_yaml_ng::from_str::<manifest::AgentVariant>(&text)
        .map_err(|error| Error::Invalid(format!("{} is not an agent variant: {error}", variant.display())))?
        .extends;
    let directory = parent(variant)?;
    let base = manifest::resolve(&directory.join(&extends))?.agent;
    let agent = manifest::resolve(variant)?.agent;
    let source = Source {
        variant: variant.to_path_buf(),
        directory,
        extends,
        origin: variant.display().to_string(),
    };
    let (minimum, recommended) = team_sizes(&base);
    let mut draft = draft(agent, base, source, minimum, recommended);
    draft.changing = true;
    Ok(draft)
}

/// The smallest machine a team's agent is offered, and the one it recommends: its own.
fn team_sizes(base: &Agent) -> (Whole, Whole) {
    let resources = Whole::of_resources(&base.spec.sandbox.resources);
    let minimum = Whole {
        cpus: 1,
        memory_gib: 2,
        disk_gib: resources.disk_gib.min(16),
    };
    (minimum, resources)
}

fn draft(agent: Agent, base: Agent, source: Source, minimum: Whole, recommended: Whole) -> Draft {
    Draft {
        agent,
        base,
        source,
        minimum,
        recommended,
        access: Access::default(),
        changing: false,
        values_file: None,
    }
}

pub(super) fn parent(path: &Path) -> Result<PathBuf, Error> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::Invalid(format!("{} has no parent folder", path.display())))
}

fn file_name(path: &Path) -> Result<String, Error> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Invalid(format!("{} names no file", path.display())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{choose_save_as, manifest_label};
    use crate::setup::{SetupArguments, answers::Answers};
    use clap::{Args, FromArgMatches};

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
        let canonical = folder.canonicalize().expect("a canonical folder");
        assert_eq!(
            manifest_label(&root.path().join("agents/.."), &canonical.join("agent.yaml")),
            "altinn-minimal (agents/minimal/agent.yaml)"
        );
        assert!(
            manifest_label(&folder, &folder.join("agent.broken.yaml"))
                .starts_with("agent.broken.yaml, which does not resolve")
        );
    }

    #[test]
    fn the_persons_choices_go_to_a_variant_git_ignores_and_the_teams_are_not_theirs() {
        let repository = tempfile::tempdir().expect("a repository");
        let initialized = std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(repository.path())
            .status();
        if !initialized.is_ok_and(|status| status.success()) {
            return;
        }
        std::fs::write(repository.path().join(".gitignore"), "agent.mine.yaml\n").expect("an ignore file");
        for file in ["agent.yaml", "agent.nested.yaml", "agent.mine.yaml"] {
            std::fs::write(repository.path().join(file), "kind: Agent\n").expect("a manifest");
        }
        let command = SetupArguments::augment_args(clap::Command::new("setup"));
        let arguments = SetupArguments::from_arg_matches(&command.get_matches_from(["setup"])).expect("no arguments");
        let mine = "mine".parse().expect("a variant name");
        let nested = "nested".parse().expect("a variant name");
        let chosen = |manifest: &str, default, answers: &Answers| {
            choose_save_as(&arguments, answers, &repository.path().join(manifest), default)
                .map(|variant| variant.to_string())
        };

        let defaults = Answers::new(false, true);
        assert_eq!(chosen("agent.yaml", &mine, &defaults).expect("the default"), "mine");
        assert!(
            chosen("agent.yaml", &nested, &defaults).is_err(),
            "agent.nested.yaml is the team's"
        );
        let unasked = Answers::new(false, false);
        assert_eq!(
            chosen("agent.mine.yaml", &nested, &unasked).expect("the person's own"),
            "mine"
        );
    }
}
