//! Where an agent comes from: one the repository setup runs in defines, or a team's from a path or
//! a GitHub address, with the manifest chosen among those it holds. The person's variant of it may
//! exist already, from an earlier setup, in which case setup changes it.
//!
//! A definition from GitHub is downloaded, and kept in the control-plane home once the agent is
//! made, as agentd reads its home, instructions and skills from it on every pass. One that mounts
//! its own repository, as a worktree variant does, works on a checkout the person keeps instead.

use super::answers::{Answers, Choice};
use super::sizing::Whole;
use super::{Access, Copy, Draft, SetupArguments, Source, SourceChoice, fetch};
use crate::{CommandError, CommandResult};
use agent::{Agent, AgentVariantName, Error, manifest};
use std::path::{Path, PathBuf};

/// Where setup puts what it downloads: this run's folder, until the agent is made, and the folder
/// that keeps the copies agents are made from.
pub(super) struct Downloads<'a> {
    pub(super) staging: &'a Path,
    pub(super) kept: &'a Path,
}

/// The agent, from where it comes from, with the person's choices to be saved as `save_as`.
pub(super) async fn choose_draft(
    arguments: &SetupArguments,
    answers: &Answers,
    downloads: &Downloads<'_>,
    source: SourceChoice,
    save_as: &AgentVariantName,
) -> CommandResult<Draft> {
    match source {
        SourceChoice::Here => {
            let directory = std::env::current_dir().map_err(Error::from)?;
            let root = crate::discovery::repository_root(&directory).unwrap_or(&directory);
            let found = crate::discovery::working_tree_manifests(&directory);
            let manifest = choose_among(answers, arguments.variant.as_ref(), root, found)?;
            // Named by its path, so the repeated command sets up this one and no other.
            answers.record("--source", &["team"]);
            answers.record("--from", &[&manifest.display().to_string()]);
            let save_as = choose_save_as(arguments, answers, &manifest, save_as, &git_owner)?;
            team_draft(&manifest, &manifest.display().to_string(), &save_as)
        }
        SourceChoice::Team => team_source(arguments, answers, downloads, save_as).await,
    }
}

/// A team's agent from `--from`, else from a path or GitHub address the person gives. Without a
/// flag, an address that cannot be read, cloned or chosen from is asked for again, so a typo does
/// not end setup.
async fn team_source(
    arguments: &SetupArguments,
    answers: &Answers,
    downloads: &Downloads<'_>,
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
        match team_draft_from(arguments, answers, downloads, &from, save_as).await {
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

/// The team's agent at `from`: a path, its manifest chosen when it holds several, or a GitHub
/// address, which is downloaded first.
async fn team_draft_from(
    arguments: &SetupArguments,
    answers: &Answers,
    downloads: &Downloads<'_>,
    from: &str,
    save_as: &AgentVariantName,
) -> CommandResult<Draft> {
    let Some(mut github) = fetch::GithubSource::parse(from) else {
        let path = manifest::expand_host_path(Path::new(from))?;
        let manifest = choose_manifest(answers, arguments.variant.as_ref(), &path)?;
        answers.record("--from", &[&manifest.display().to_string()]);
        answers.forget("--variant");
        let save_as = choose_save_as(arguments, answers, &manifest, save_as, &git_owner)?;
        return team_draft(&manifest, &manifest.display().to_string(), &save_as);
    };
    answers.record("--from", &[from]);
    let copy = downloads.staging.join("download");
    if copy.exists() {
        // From an address tried before in this run.
        std::fs::remove_dir_all(&copy).map_err(Error::from)?;
    }
    github.download(&copy).await?;
    let found = github.found_in(&copy)?;
    let manifest = if found.is_file() {
        found
    } else {
        let manifests = crate::discovery::repository_manifests(&copy)
            .into_iter()
            .filter(|manifest| manifest.starts_with(&found))
            .collect();
        choose_among(answers, arguments.variant.as_ref(), &found, manifests)?
    };
    let relative = manifest
        .strip_prefix(&copy)
        .map_err(|_| Error::Invalid(format!("{} is not in the download", manifest.display())))?;
    if mounts_its_repository(&manifest, &copy)? {
        // Before asking where the checkout goes, which is of no use without git.
        fetch::ensure_git(&format!("{}/{}", github.owner, github.repository)).await?;
        let checkout = clone_into(arguments, answers, &github)?;
        if arguments.dry_run {
            // The download stands in for the checkout, so a dry run clones nothing.
            let save_as = arguments.save_as.clone().unwrap_or_else(|| save_as.clone());
            let mut draft = team_draft(&manifest, &github.describe(), &save_as)?;
            draft.source.copy = Some(Copy {
                root: copy,
                kept: checkout,
            });
            return Ok(draft);
        }
        let made = !checkout.join(".git").exists();
        if !fetch::adopt(&copy, &checkout) {
            github.checkout(&checkout).await?;
        }
        let manifest = checkout.join(relative);
        if !manifest.is_file() {
            return Err(Error::Invalid(format!("{} has no {}", checkout.display(), relative.display())).into());
        }
        let save_as = choose_save_as(arguments, answers, &manifest, save_as, &git_owner)?;
        let mut draft = team_draft(&manifest, &github.describe(), &save_as)?;
        draft.source.checkout_made = made.then_some(checkout);
        return Ok(draft);
    }
    if arguments.clone_into.is_some() {
        eprintln!("warning: --clone-into is left out: this agent needs no checkout of its repository");
    }
    // A copy, not a checkout to commit in.
    std::fs::remove_dir_all(copy.join(".git")).ok();
    let kept = downloads.kept.join(github.kept_folder());
    let carried = carry_over(&kept, &copy)?;
    let owner = |path: &Path| {
        if carried.iter().any(|carried| carried == path) {
            Some(true)
        } else {
            path.exists().then_some(false)
        }
    };
    let save_as = choose_save_as(arguments, answers, &manifest, save_as, &owner)?;
    let mut draft = team_draft(&manifest, &github.describe(), &save_as)?;
    draft.source.copy = Some(Copy { root: copy, kept });
    Ok(draft)
}

/// Where the checkout of the repository `github` names goes, which a definition that mounts its
/// repository works on: `--clone-into`, else a folder the person names, here unless they say
/// otherwise.
fn clone_into(arguments: &SetupArguments, answers: &Answers, github: &fetch::GithubSource) -> CommandResult<PathBuf> {
    if let Some(folder) = &arguments.clone_into {
        return Ok(manifest::expand_host_path(folder)?);
    }
    let default = std::env::current_dir().map_err(Error::from)?.join(&github.repository);
    let question = format!(
        "This agent works on a checkout of {}/{} on this machine.\nWhere should setup clone it?",
        github.owner, github.repository
    );
    let default = default.display().to_string();
    let answer = answers.text("--clone-into", &question, Some(&default), |_| Ok(()))?;
    Ok(manifest::expand_host_path(Path::new(answer.trim()))?)
}

/// Whether the agent `manifest` defines mounts the repository it is in at `root`, or a folder of
/// it, by a path relative to the manifest, as a worktree variant does. One that reaches outside
/// the repository that way is refused, since it would mount whatever is beside the copy.
fn mounts_its_repository(manifest: &Path, root: &Path) -> CommandResult<bool> {
    let agent = manifest::resolve(manifest)?.agent;
    let directory = parent(manifest)?;
    let root = root.canonicalize().map_err(Error::from)?;
    let mut mounts = false;
    for mount in &agent.spec.sandbox.mounts {
        let agent::MountSpec::Bind { source, .. } = mount else {
            continue;
        };
        // A path from a root, such as `/srv/data` on Windows, is not relative to the manifest
        // either, though Windows does not count it as absolute without a drive.
        if !source.is_relative() || source.has_root() || source.starts_with("~") {
            continue;
        }
        let resolved = directory.join(source);
        let inside = resolved
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with(&root));
        if !inside {
            return Err(CommandError::Message(format!(
                "{} mounts {}, relative to the repository it is in but outside it; clone the repository and \
                 pass its folder to --from",
                file_name(manifest)?,
                source.display()
            )));
        }
        mounts = true;
    }
    Ok(mounts)
}

/// The file in a kept copy that lists the variants setup wrote there for the person, one path
/// relative to the copy a line, as the team's own `agent.*.yaml` files look the same.
pub(super) const VARIANTS_FILE: &str = ".agentctl-variants";

/// The person's variants an earlier setup wrote in the kept copy at `kept`: those it lists, or, in
/// a copy made before it listed them, those a download at `copy` does not have.
fn persons_variants(kept: &Path, copy: &Path) -> Vec<PathBuf> {
    if let Ok(listed) = std::fs::read_to_string(kept.join(VARIANTS_FILE)) {
        return listed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|relative| {
                relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            })
            .collect();
    }
    ignore::WalkBuilder::new(kept)
        .standard_filters(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| manifest::variant_from_filename(entry.path()).is_some())
        .filter_map(|entry| entry.path().strip_prefix(kept).ok().map(Path::to_path_buf))
        .filter(|relative| !copy.join(relative).exists())
        .collect()
}

/// Copies the person's variants from an earlier setup, which `kept` holds, into the download at
/// `copy`, lists them there for the next one, and returns where they are in it.
fn carry_over(kept: &Path, copy: &Path) -> CommandResult<Vec<PathBuf>> {
    let mut carried = Vec::new();
    if !kept.is_dir() {
        return Ok(carried);
    }
    let mut listed = Vec::new();
    for relative in persons_variants(kept, copy) {
        let source = kept.join(&relative);
        let target = copy.join(&relative);
        if !source.is_file() {
            continue;
        }
        if target.exists() {
            eprintln!(
                "warning: the team's {} now has your variant's name, so your copy of it is not kept",
                relative.display()
            );
            continue;
        }
        // Kept where the team removed its folder too, for the agents made from it to say so.
        let parent = target.parent().unwrap_or(copy);
        if !parent.is_dir() {
            eprintln!(
                "warning: the team removed {}, which your variant {} extends",
                parent.strip_prefix(copy).unwrap_or(parent).display(),
                relative.display()
            );
            std::fs::create_dir_all(parent).map_err(Error::from)?;
        }
        std::fs::copy(&source, &target).map_err(Error::from)?;
        listed.push(relative);
        carried.push(target);
    }
    list_variants(copy, &listed)?;
    Ok(carried)
}

/// Lists `variants`, relative to the copy at `copy`, as the person's, with those it lists already.
pub(super) fn list_variants(copy: &Path, variants: &[PathBuf]) -> Result<(), Error> {
    let file = copy.join(VARIANTS_FILE);
    let mut lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    for variant in variants {
        let line = variant
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(file, text)?;
    Ok(())
}

/// The variant beside `manifest` the person's choices are saved as: `manifest` itself when it is
/// a variant of theirs, which an earlier setup wrote, else `--save-as` or the person's answer,
/// `default` unless they name another. A variant the team's files hold is not theirs to write.
/// `owner` says whether a file is the person's, the team's, or cannot tell.
fn choose_save_as(
    arguments: &SetupArguments,
    answers: &Answers,
    manifest: &Path,
    default: &AgentVariantName,
    owner: &dyn Fn(&Path) -> Option<bool>,
) -> CommandResult<AgentVariantName> {
    if let Some(variant) = manifest::variant_from_filename(manifest)
        && owner(manifest) == Some(true)
        && is_variant(manifest)
    {
        return Ok(variant);
    }
    let directory = parent(manifest)?;
    let check = |answer: &str| {
        let variant: AgentVariantName = answer.trim().parse().map_err(|error: Error| error.to_string())?;
        let path = directory.join(variant.filename());
        if !path.is_file() {
            Ok(())
        } else if owner(&path) == Some(false) {
            Err(format!("{} is the team's; choose another name", variant.filename()))
        } else if !is_variant(&path) {
            Err(format!(
                "{} is a manifest of its own, not a variant; choose another name",
                variant.filename()
            ))
        } else {
            Ok(())
        }
    };
    if let Some(save_as) = &arguments.save_as {
        check(save_as.as_ref()).map_err(|reason| Error::Invalid(format!("--save-as {save_as}: {reason}")))?;
        return Ok(save_as.clone());
    }
    let question = format!(
        "Setup saves your choices as a variant, agent.<variant>.yaml beside {}, which Git should ignore.\nWhich variant?",
        file_name(manifest)?
    );
    let answer = answers.text("--save-as", &question, Some(default.as_ref()), check)?;
    Ok(answer.trim().parse()?)
}

/// Whether the file at `path` is an agent variant, which extends another manifest, rather than a
/// manifest of its own.
fn is_variant(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| serde_yaml_ng::from_str::<manifest::AgentVariant>(&text).is_ok())
}

/// Whose a file in a checkout is: the person's when Git ignores it, the team's when it does not.
fn git_owner(path: &Path) -> Option<bool> {
    super::create::git_ignores(path)
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
        copy: None,
        checkout_made: None,
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
        copy: None,
        checkout_made: None,
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
        edited: None,
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

    use super::{VARIANTS_FILE, carry_over, choose_save_as, git_owner, manifest_label, mounts_its_repository};
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
        std::fs::write(
            repository.path().join(".gitignore"),
            "agent.mine.yaml\nagent.local.yaml\n",
        )
        .expect("an ignore file");
        for file in ["agent.yaml", "agent.nested.yaml", "agent.local.yaml"] {
            std::fs::write(repository.path().join(file), "kind: Agent\n").expect("a manifest");
        }
        std::fs::write(
            repository.path().join("agent.mine.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata: {name: mine}\n",
        )
        .expect("the person's variant");
        let parse = |flags: &[&str]| {
            let command = SetupArguments::augment_args(clap::Command::new("setup"));
            SetupArguments::from_arg_matches(
                &command.get_matches_from(std::iter::once("setup").chain(flags.iter().copied())),
            )
            .expect("arguments")
        };
        let arguments = parse(&[]);
        let mine = "mine".parse().expect("a variant name");
        let nested = "nested".parse().expect("a variant name");
        let chosen = |manifest: &str, default, answers: &Answers| {
            choose_save_as(
                &arguments,
                answers,
                &repository.path().join(manifest),
                default,
                &git_owner,
            )
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
        assert!(
            chosen("agent.local.yaml", &mine, &unasked).is_err(),
            "a manifest of the person's own is not a variant to change, so setup asks"
        );
        let local = "local".parse().expect("a variant name");
        assert!(
            chosen("agent.yaml", &local, &defaults).is_err(),
            "agent.local.yaml is no variant"
        );
        let flagged = parse(&["--save-as", "nested"]);
        assert!(
            choose_save_as(
                &flagged,
                &defaults,
                &repository.path().join("agent.yaml"),
                &mine,
                &git_owner
            )
            .is_err(),
            "--save-as is checked as the question is"
        );
    }

    const BASE: &str = "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: worker}\nspec:\n  sandbox:\n    \
                        image: {type: reference, reference: example.com/agent:latest}\n    platform: {os: linux}\n    \
                        resources: {cpu: '2', memory: 4Gi, rootFilesystem: {capacity: 20Gi, mode: layered}}\n  \
                        home: {source: home}\n  harnesses: [{type: pi}]\n  network: {mode: mediated, allow: all}\n";

    fn variant_mounting(source: &str) -> String {
        format!(
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata: {{name: \
             worker-x}}\nspec:\n  sandbox:\n    mounts:\n      - {{type: bind, source: '{source}', target: \
             /workspace/repo, readOnly: false}}\n"
        )
    }

    #[test]
    fn a_definition_that_mounts_its_own_repository_needs_a_checkout() {
        let repository = tempfile::tempdir().expect("a repository");
        let folder = repository.path().join("agents/worker");
        std::fs::create_dir_all(folder.join("home")).expect("folders");
        std::fs::write(folder.join("agent.yaml"), BASE).expect("a manifest");
        std::fs::write(folder.join("agent.worktree.yaml"), variant_mounting("../..")).expect("a variant");
        std::fs::write(folder.join("agent.outside.yaml"), variant_mounting("../../..")).expect("a variant");
        // An absolute path on this system, which `/srv/data` is not on Windows.
        let elsewhere = tempfile::tempdir().expect("a folder elsewhere");
        let absolute = variant_mounting(&elsewhere.path().display().to_string());
        std::fs::write(folder.join("agent.absolute.yaml"), absolute).expect("a variant");

        assert!(!mounts_its_repository(&folder.join("agent.yaml"), repository.path()).expect("resolves"));
        assert!(mounts_its_repository(&folder.join("agent.worktree.yaml"), repository.path()).expect("resolves"));
        assert!(!mounts_its_repository(&folder.join("agent.absolute.yaml"), repository.path()).expect("resolves"));
        assert!(
            mounts_its_repository(&folder.join("agent.outside.yaml"), repository.path()).is_err(),
            "a relative mount outside the repository would mount whatever is beside the copy"
        );
    }

    #[test]
    fn the_persons_variants_carry_over_to_a_new_download() {
        let kept = tempfile::tempdir().expect("a kept copy");
        let copy = tempfile::tempdir().expect("a download");
        for root in [kept.path(), copy.path()] {
            std::fs::create_dir_all(root.join("agents/worker")).expect("folders");
            std::fs::write(root.join("agents/worker/agent.yaml"), "team").expect("the team's manifest");
            std::fs::write(root.join("agents/worker/agent.nested.yaml"), "team").expect("the team's variant");
        }
        std::fs::write(kept.path().join("agents/worker/agent.mine.yaml"), "mine").expect("the person's variant");
        std::fs::write(kept.path().join("agents/worker/notes.md"), "old").expect("another file");
        std::fs::create_dir_all(kept.path().join("agents/gone")).expect("a folder the team removed");
        std::fs::write(kept.path().join("agents/gone/agent.mine.yaml"), "mine").expect("a variant of it");

        let carried = carry_over(kept.path(), copy.path()).expect("carried over");
        assert_eq!(
            carried,
            [
                copy.path().join("agents/gone/agent.mine.yaml"),
                copy.path().join("agents/worker/agent.mine.yaml")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(copy.path().join("agents/worker/agent.mine.yaml")).expect("read"),
            "mine"
        );
        assert!(
            !copy.path().join("agents/worker/notes.md").exists(),
            "only variants carry over"
        );
    }

    #[test]
    fn the_variants_setup_listed_carry_over_and_the_teams_do_not() {
        let kept = tempfile::tempdir().expect("a kept copy");
        let copy = tempfile::tempdir().expect("a download");
        for root in [kept.path(), copy.path()] {
            std::fs::create_dir_all(root.join("agents/worker")).expect("folders");
            std::fs::write(root.join("agents/worker/agent.yaml"), "team").expect("the team's manifest");
            std::fs::write(root.join("agents/worker/agent.build.yaml"), "team").expect("the team's variant");
        }
        std::fs::write(kept.path().join("agents/worker/agent.old.yaml"), "team, since removed").expect("a variant");
        std::fs::write(kept.path().join("agents/worker/agent.mine.yaml"), "mine").expect("the person's variant");
        std::fs::write(kept.path().join(VARIANTS_FILE), "agents/worker/agent.mine.yaml\n").expect("the list");

        let carried = carry_over(kept.path(), copy.path()).expect("carried over");
        assert_eq!(carried, [copy.path().join("agents/worker/agent.mine.yaml")]);
        assert!(
            !copy.path().join("agents/worker/agent.old.yaml").exists(),
            "a variant the team removed is not the person's"
        );
        assert_eq!(
            std::fs::read_to_string(copy.path().join(VARIANTS_FILE)).expect("the new list"),
            "agents/worker/agent.mine.yaml\n"
        );
    }
}
