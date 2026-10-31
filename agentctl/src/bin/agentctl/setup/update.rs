//! Updating an agent in `~/agents/<name>/` from where setup made it from, such as a team's
//! repository or a folder of definitions where the agent improved its own. Replacing an agent
//! deletes it with its disk, so an update instead writes the folder again from the source,
//! keeps what setup's questions decided, and applies it as `agentctl apply` does.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use agent::{AccessSpec, Agent, Error, Harness, MountSpec, control_plane::ApplyRequest, manifest};
use sandbox::{RootFilesystem, SandboxPath, image::ImageSource};
use serde::{Deserialize, Serialize};

use super::answers::Choice;
use super::create::{OnFailure, create, write_contents};
use super::existing::{Context, Existing, State, exchange, move_folder, swap};
use super::mounts::{check_mounts, create_missing_mounts, protected_folders, refuse_staged_mounts};
use super::source::{standard_draft, team_draft_from};
use super::{Draft, command_line, docker, edit, github, image, sign_in, values};
use crate::{CommandError, CommandResult};

/// The file in an agent's folder that records where setup made it from.
const SOURCE_FILE: &str = ".setup-source.yaml";

/// Where an agent's definition came from, so setup can fetch it again.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Recorded {
    /// A standard agent, from the catalog of the agentctl that updates it.
    Standard { id: String },
    /// A team's agent: a manifest on this machine, or the GitHub address of the folder holding it
    /// with its variant.
    Team {
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<String>,
    },
}

/// What setup left out of the agent its source defines, so an update leaves it out again while
/// taking what the source adds.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LeftOut {
    /// The harnesses the agent does not have.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) harnesses: Vec<Harness>,
    /// The folders it does not share, by where the agent would see them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) shared: Vec<String>,
}

impl LeftOut {
    const fn is_empty(&self) -> bool {
        self.harnesses.is_empty() && self.shared.is_empty()
    }
}

/// What setup records in an agent's folder: where the agent came from and what it left out.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Record {
    pub(super) source: Recorded,
    #[serde(default, skip_serializing_if = "LeftOut::is_empty")]
    pub(super) left_out: LeftOut,
}

/// Records in `folder` where its agent came from and what setup left out.
pub(super) fn record(folder: &Path, record: &Record) -> Result<(), Error> {
    let yaml = serde_yaml_ng::to_string(record).map_err(Error::Yaml)?;
    std::fs::write(
        folder.join(SOURCE_FILE),
        format!("# Where `agentctl setup` made this agent from, to update it from there.\n{yaml}"),
    )?;
    Ok(())
}

/// Where the agent in `folder` came from, and what setup left out, when setup recorded it.
pub(super) fn recorded(folder: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(folder.join(SOURCE_FILE)).ok()?;
    serde_yaml_ng::from_str(&text).ok()
}

/// Updates `existing` from where it came from, after showing what changes.
pub(super) async fn run(context: &Context<'_>, existing: &Existing) -> CommandResult<()> {
    let Some(Record {
        source: recorded,
        left_out,
    }) = &existing.recorded
    else {
        return Err(CommandError::Message(format!(
            "setup did not record where {} came from; replace it instead",
            existing.name
        )));
    };
    let answers = context.answers;
    let staging = context.staging.path();
    let current = manifest::resolve(&existing.folder.join(manifest::MANIFEST_FILE))?.agent;
    let mut draft = match recorded {
        Recorded::Standard { id } => standard_draft(Some(id), answers, context.catalog, staging)?,
        Recorded::Team { from, variant } => {
            let variant = variant
                .as_deref()
                .map(crate::parse_variant_name)
                .transpose()
                .map_err(CommandError::Message)?;
            team_draft_from(variant.as_ref(), answers, staging, from).await?
        }
    };
    keep_answers(&current, left_out, &existing.folder, &mut draft);
    refuse_staged_mounts(&draft, staging)?;
    let protected = protected_folders(existing.folder.parent().unwrap_or(&existing.folder), context.home);
    create_missing_mounts(answers, &draft, &protected, false)?;
    check_mounts(&draft, &protected, &[])?;
    draft.access.known_values =
        agent::environment::read_or_empty(&existing.folder.join(agent::control_plane::ENV_FILE)).await?;
    values::collect(answers, None, &mut draft).await?;

    let staged = staging.join("update");
    let agent = loop {
        if staged.exists() {
            std::fs::remove_dir_all(&staged).map_err(Error::from)?;
        }
        std::fs::create_dir_all(&staged).map_err(Error::from)?;
        let agent = write_contents(&draft, &staged)?;
        let changes = changes(&existing.folder, &staged)?;
        if changes.is_empty() {
            println!("{} is up to date with {}.", existing.name, draft.source.origin);
            return Ok(());
        }
        let recreate = fixed_fields_changed(context, existing, &agent).await?;
        match review(context, existing, &draft, &agent, &changes, &recreate)? {
            Review::Update => break (agent, !recreate.is_empty()),
            Review::Edit => {
                edit::manifest(answers, &mut draft)?;
                check_mounts(&draft, &protected, &[])?;
            }
            Review::Stop => {
                println!("Nothing was changed.");
                return Ok(());
            }
        }
    };
    let (agent, recreate) = agent;
    answers.no_way_back();
    if matches!(agent.spec.sandbox.image, ImageSource::Build { .. }) {
        docker::ensure().await?;
    }
    if recreate {
        return create_again(context, existing, &agent, &staged).await;
    }
    apply(context, existing, &agent, &staged).await
}

/// The fields agentd cannot change on the agent of this folder that `agent` changes, so updating
/// to it takes creating the agent again.
async fn fixed_fields_changed(
    context: &Context<'_>,
    existing: &Existing,
    agent: &Agent,
) -> CommandResult<Vec<&'static str>> {
    let Some(name) = existing.agent_here() else {
        return Ok(Vec::new());
    };
    let current = context.client.get(name).await?;
    // agentd holds bind mount sources resolved, as it resolves those it is given.
    let mut desired = agent.spec.clone();
    for mount in &mut desired.sandbox.mounts {
        if let MountSpec::Bind { source, .. } = mount
            && let Ok(resolved) = std::fs::canonicalize(&*source)
        {
            *source = resolved;
        }
    }
    Ok(current.spec.immutable_changes(&desired))
}

/// Carries over to `draft`, made from the source again, what setup's questions decided for the
/// `current` agent in `folder`: its name, its size, its harnesses and the default one, its mounts,
/// GitHub access and an image built here. What setup `left_out` stays out, while what the source
/// adds since comes in. Whatever else was changed by hand in `folder` gives way to the source, which
/// the review shows.
fn keep_answers(current: &Agent, left_out: &LeftOut, folder: &Path, draft: &mut Draft) {
    draft.agent.metadata.name.clone_from(&current.metadata.name);
    let spec = &mut draft.agent.spec;
    let resources = &mut spec.sandbox.resources;
    resources.cpu = current.spec.sandbox.resources.cpu;
    resources.memory = current.spec.sandbox.resources.memory;
    resources.root_filesystem = RootFilesystem::new(
        current.spec.sandbox.resources.root_filesystem.capacity(),
        resources.root_filesystem.mode(),
    );

    // A source that keeps only harnesses setup left out keeps them, as an agent needs one.
    if spec
        .harnesses
        .iter()
        .any(|harness| !left_out.harnesses.contains(&harness.kind))
    {
        spec.harnesses
            .retain(|harness| !left_out.harnesses.contains(&harness.kind));
    }
    draft.left_out = left_out.clone();
    if let Some(default) = current.spec.default_harness().map(|harness| harness.kind)
        && spec.harnesses.iter().any(|harness| harness.kind == default)
    {
        for harness in &mut spec.harnesses {
            harness.default = harness.kind == default;
            harness.optional &= harness.kind != default;
        }
    }

    // A mount of the current agent stands in for the source's at the same place, since setup made
    // its source absolute or chose the checkout behind it. The folder setup mounted is kept too.
    spec.sandbox
        .mounts
        .retain(|mount| !left_out.shared.iter().any(|shared| shared == target(mount).as_str()));
    // Mount targets are unique within an agent, so each place has at most one of each.
    let current_mounts = &current.spec.sandbox.mounts;
    for mount in &mut spec.sandbox.mounts {
        if let Some(current) = current_mounts.iter().find(|current| target(current) == target(mount)) {
            mount.clone_from(current);
        }
    }
    let added: Vec<MountSpec> = current_mounts
        .iter()
        .filter(|current| !spec.sandbox.mounts.iter().any(|mount| target(mount) == target(current)))
        .cloned()
        .collect();
    spec.sandbox.mounts.extend(added);

    if current
        .spec
        .secrets
        .iter()
        .any(|secret| secret.environment == github::TOKEN_VARIABLE)
    {
        github::declare(spec);
    }
    if current
        .spec
        .environment
        .iter()
        .any(|variable| variable.name == github::GIT_USER_NAME)
    {
        github::declare_identity(spec);
    }
    if !spec.access.contains(&AccessSpec::Ssh {}) {
        spec.access.push(AccessSpec::Ssh {});
    }

    let built_here = folder.join(image::SOURCES).is_dir();
    if let (true, Some(_), ImageSource::Reference { reference }) =
        (built_here, &draft.source.image_sources, &spec.sandbox.image)
    {
        draft.built_here = Some(reference.clone());
    }
}

const fn target(mount: &MountSpec) -> &SandboxPath {
    match mount {
        MountSpec::Bind { target, .. } | MountSpec::Tmpfs { target, .. } => target,
    }
}

/// The files that differ between the folder `before` and the one `after`, each marked as added
/// (`+`), removed (`-`) or changed (`~`). The `.env` is left out, since an update keeps its values.
fn changes(before: &Path, after: &Path) -> Result<Vec<String>, Error> {
    let old = files(before)?;
    let new = files(after)?;
    let mut changes = Vec::new();
    for file in old.union(&new) {
        let change = match (old.contains(file), new.contains(file)) {
            (false, _) => '+',
            (_, false) => '-',
            _ if std::fs::read(before.join(file))? != std::fs::read(after.join(file))? => '~',
            _ => continue,
        };
        let file: Vec<_> = file
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        changes.push(format!("{change} {}", file.join("/")));
    }
    Ok(changes)
}

/// The files below `root`, relative to it, but not its `.env`.
fn files(root: &Path) -> Result<BTreeSet<PathBuf>, Error> {
    let mut found = BTreeSet::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if let Ok(relative) = path.strip_prefix(root)
                && relative != Path::new(agent::control_plane::ENV_FILE)
            {
                found.insert(relative.to_path_buf());
            }
        }
    }
    Ok(found)
}

enum Review {
    Update,
    /// Edit its manifest first, then review the changes again.
    Edit,
    Stop,
}

fn review(
    context: &Context<'_>,
    existing: &Existing,
    draft: &Draft,
    agent: &Agent,
    changes: &[String],
    recreate: &[&str],
) -> CommandResult<Review> {
    let mut lines = vec![format!(
        "Update {} from {}:",
        existing.folder.display(),
        draft.source.origin
    )];
    lines.extend(changes.iter().map(|change| format!("  {change}")));
    if changes.iter().any(|change| change.ends_with(manifest::MANIFEST_FILE)) {
        lines.push(
            "Changes made to agent.yaml by hand, other than its name, size, harnesses and shared folders, give \
             way to the source."
                .to_owned(),
        );
    }
    let name = &agent.metadata.name;
    let (question, yes, default) = if recreate.is_empty() {
        let question = if existing.agent_here().is_some() {
            format!("Update agent/{name}? It keeps its disk and sessions.")
        } else {
            format!("Update the folder and create agent/{name}?")
        };
        (question, "Yes, update it", 0)
    } else {
        // Like replacing, this deletes the agent's disk, so neither `--yes` nor a script does it
        // without `--replace`.
        if !context.answers.interactive() && !context.arguments.replace {
            return Err(CommandError::Message(format!(
                "the update changes {}, which agent/{name} cannot change without being created again, its disk \
                 and sessions deleted; pass --replace to do so",
                recreate.join(", ")
            )));
        }
        let question = format!(
            "An existing agent cannot change {}, so the update creates agent/{name} again, and its disk and \
             sessions are deleted. Do it?",
            recreate.join(", ")
        );
        let default = if context.arguments.replace { 0 } else { 2 };
        (question, "Yes, create it again, deleting its disk", default)
    };
    lines.push(question);
    Ok(context.answers.choose(
        "--yes",
        &lines.join("\n"),
        vec![
            Choice::new(Review::Update, yes),
            Choice::new(Review::Edit, "Edit its agent.yaml first, then decide"),
            Choice::new(Review::Stop, "No, change nothing"),
        ],
        default,
    )?)
}

/// Deletes the agent of this folder, puts the folder written at `staged` in its place and creates
/// the agent from it, as replacing it does.
async fn create_again(context: &Context<'_>, existing: &Existing, agent: &Agent, staged: &Path) -> CommandResult<()> {
    let backup = context.staging.path().join("updated");
    let timeout = context.arguments.timeout;
    swap(context.client, existing, staged, &backup, timeout).await?;
    let folder = &existing.folder;
    let cli = command_line(context.home);
    let name = create(
        context.answers,
        context.client,
        &cli,
        agent,
        folder,
        &folder.join(manifest::MANIFEST_FILE),
        timeout,
        OnFailure::Restore(backup),
    )
    .await?;
    println!("agent/{name} is created again from the update and ready.");
    Ok(())
}

/// Puts the folder written at `staged` in place of the agent's own and applies it. Should applying
/// fail, the folder from before comes back.
async fn apply(context: &Context<'_>, existing: &Existing, agent: &Agent, staged: &Path) -> CommandResult<()> {
    let folder = &existing.folder;
    let backup = context.staging.path().join("updated");
    exchange(folder, staged, &backup)?;
    let applied = context
        .client
        .apply(ApplyRequest {
            source_directory: folder.clone(),
            manifest_path: Some(folder.join(manifest::MANIFEST_FILE)),
            env_file: None,
            create_only: false,
            agent: agent.clone(),
        })
        .await;
    if let Err(error) = applied {
        std::fs::remove_dir_all(folder).ok();
        if move_folder(&backup, folder).is_ok() {
            eprintln!("The agent was not updated; {} is as it was.", folder.display());
        }
        return Err(error.into());
    }
    println!("Updated {}", folder.display());
    let name = &agent.metadata.name;
    let cli = command_line(context.home);
    if existing.state == State::Stopped {
        println!("agent/{name} stays stopped; `{cli} start agent/{name}` starts it as updated.");
    } else {
        println!("agent/{name} is updating");
        if let Err(error) = crate::wait_until_converged(context.client, name, context.arguments.timeout).await {
            eprintln!("\nagent/{name} did not become ready: {error}");
            return Err(CommandError::Message(format!(
                "agent/{name} is not ready; `{cli} describe agent/{name}` shows why"
            )));
        }
        println!("agent/{name} is updated and ready.");
    }
    let missing = sign_in::logins(context.client, agent).await?.missing;
    if !missing.is_empty() {
        let names: Vec<_> = missing.iter().map(|harness| harness.display_name()).collect();
        println!(
            "It has no login for {} yet; `{cli} setup --existing {name} --action sign-in` signs in.",
            names.join(" or ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use agent::{Harness, MountSpec, manifest};

    use super::{LeftOut, Record, Recorded, changes, keep_answers, record, recorded};
    use crate::setup::source::team_draft;

    const MANIFEST: &str = "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: worker}\nspec:\n  \
                            sandbox:\n    image: {type: reference, reference: example.com/agent:latest}\n    \
                            platform: {os: linux}\n    resources: {cpu: '2', memory: 4Gi, rootFilesystem: \
                            {capacity: 20Gi, mode: layered}}\n    mounts:\n      - {type: bind, source: ../repo, \
                            target: /mnt/repo, readOnly: false}\n  home: {source: home}\n  harnesses:\n    - {type: \
                            claudeCode, auth: mediated, default: true}\n    - {type: codex, auth: mediated, \
                            optional: true}\n  network: {mode: mediated, allow: all}\n";

    #[test]
    fn an_update_keeps_what_setup_asked_and_takes_the_rest_from_the_source() {
        let root = tempfile::tempdir().expect("a root");
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("home")).expect("a home");
        std::fs::write(source.join("agent.yaml"), MANIFEST).expect("a manifest");
        let current = MANIFEST
            .replace("name: worker", "name: mine")
            .replace("cpu: '2', memory: 4Gi", "cpu: '6', memory: 12Gi")
            .replace("capacity: 20Gi", "capacity: 90Gi")
            .replace("source: ../repo", "source: /work/repo")
            .replace(
                "target: /mnt/repo, readOnly: false}\n",
                "target: /mnt/repo, readOnly: false}\n      - {type: bind, source: /work/notes, target: \
                 /mnt/notes, readOnly: false}\n",
            )
            .replace(
                "claudeCode, auth: mediated, default: true",
                "claudeCode, auth: mediated",
            )
            .replace(
                "codex, auth: mediated, optional: true",
                "codex, auth: mediated, default: true",
            )
            .replace(
                "  network:",
                "  secrets:\n    - {environment: GITHUB_TOKEN, allowedHosts: [github.com]}\n  network:",
            );
        let current = manifest::decode(current.as_bytes()).expect("the current agent");
        let mut draft = team_draft(&source.join("agent.yaml"), "a test").expect("a draft");

        keep_answers(&current, &LeftOut::default(), root.path(), &mut draft);

        let spec = &draft.agent.spec;
        assert_eq!(draft.agent.metadata.name, "mine");
        assert_eq!(spec.sandbox.resources.cpu, current.spec.sandbox.resources.cpu);
        assert_eq!(spec.sandbox.resources.memory, current.spec.sandbox.resources.memory);
        assert_eq!(
            spec.sandbox.resources.root_filesystem.capacity(),
            current.spec.sandbox.resources.root_filesystem.capacity()
        );
        assert_eq!(spec.default_harness().map(|harness| harness.kind), Some(Harness::Codex));
        let sources: Vec<_> = spec
            .sandbox
            .mounts
            .iter()
            .filter_map(|mount| match mount {
                MountSpec::Bind { source, .. } => Some(source.display().to_string()),
                MountSpec::Tmpfs { .. } => None,
            })
            .collect();
        assert_eq!(sources, ["/work/repo", "/work/notes"]);
        assert!(spec.secrets.iter().any(|secret| secret.environment == "GITHUB_TOKEN"));
        assert!(spec.environment.iter().any(|variable| variable.name == "GIT_USER_NAME"));
        assert_eq!(draft.built_here, None);
    }

    #[test]
    fn what_setup_left_out_stays_out_while_what_the_source_adds_comes_in() {
        let root = tempfile::tempdir().expect("a root");
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("home")).expect("a home");
        let added = MANIFEST.replace("optional: true}\n", "optional: true}\n    - {type: pi}\n");
        std::fs::write(source.join("agent.yaml"), added).expect("a manifest");
        let current = MANIFEST
            .replace(
                "    mounts:\n      - {type: bind, source: ../repo, target: /mnt/repo, readOnly: false}\n",
                "",
            )
            .replace("\n    - {type: codex, auth: mediated, optional: true}", "");
        let current = manifest::decode(current.as_bytes()).expect("the current agent");
        let left_out = LeftOut {
            harnesses: vec![Harness::Codex],
            shared: vec!["/mnt/repo".into()],
        };
        let mut draft = team_draft(&source.join("agent.yaml"), "a test").expect("a draft");

        keep_answers(&current, &left_out, root.path(), &mut draft);

        let spec = &draft.agent.spec;
        let harnesses: Vec<_> = spec.harnesses.iter().map(|harness| harness.kind).collect();
        assert_eq!(harnesses, [Harness::ClaudeCode, Harness::Pi]);
        assert_eq!(
            spec.default_harness().map(|harness| harness.kind),
            Some(Harness::ClaudeCode)
        );
        assert!(spec.sandbox.mounts.is_empty(), "{:?}", spec.sandbox.mounts);
        assert_eq!(draft.left_out, left_out, "an update records what stays out");
    }

    #[test]
    fn where_an_agent_came_from_reads_back_as_recorded() {
        let folder = tempfile::tempdir().expect("a folder");
        for source in [
            Recorded::Standard {
                id: "dotnet-backend-developer".into(),
            },
            Recorded::Team {
                from: "https://github.com/Altinn/altinn-studio//agents/minimal@main".into(),
                variant: Some("nested".into()),
            },
        ] {
            let record_of = |left_out| Record {
                source: source.clone(),
                left_out,
            };
            for left_out in [
                LeftOut::default(),
                LeftOut {
                    harnesses: vec![Harness::Codex],
                    shared: vec!["/mnt/repo".into()],
                },
            ] {
                record(folder.path(), &record_of(left_out.clone())).expect("recorded");
                assert_eq!(recorded(folder.path()), Some(record_of(left_out)));
            }
        }
    }

    #[test]
    fn the_changes_name_each_file_added_removed_or_changed_but_not_the_values() {
        let root = tempfile::tempdir().expect("a root");
        let (before, after) = (root.path().join("before"), root.path().join("after"));
        for folder in [&before, &after] {
            std::fs::create_dir_all(folder.join("skills/a")).expect("folders");
            std::fs::write(folder.join("agent.yaml"), "same").expect("a manifest");
        }
        std::fs::write(before.join(".env"), "A=1").expect("values");
        std::fs::write(after.join(".env"), "A=2").expect("values");
        std::fs::write(before.join("instructions.md"), "old").expect("instructions");
        std::fs::write(after.join("instructions.md"), "new").expect("instructions");
        std::fs::write(before.join("skills/a/SKILL.md"), "gone").expect("a skill");
        std::fs::write(after.join("skills/a/notes.md"), "added").expect("a note");

        assert_eq!(
            changes(&before, &after).expect("compared"),
            ["~ instructions.md", "- skills/a/SKILL.md", "+ skills/a/notes.md"]
        );
        assert_eq!(changes(&before, &before).expect("compared"), Vec::<String>::new());
    }
}
