//! The host folders a new agent mounts: the checkout its own repository mounts, a folder the
//! person chooses, what may not be mounted, the agent definitions a folder already holds, and
//! where a folder appears in the sandbox.

use super::answers::{Answers, Choice};
use super::source::manifest_label;
use super::{Draft, SetupArguments};
use crate::{CommandError, CommandResult};
use agent::local::home::ControlPlaneHome;

use std::path::{Component, Path, PathBuf};

use agent::{
    Agent, Error, MountSpec,
    manifest::{self, is_manifest_filename},
};
use sandbox::SandboxPath;

/// Directories that may not be mounted, nor any directory holding them: a mount of one would give
/// the sandbox the person's keys, the control plane's state or every agent's secrets.
pub(super) struct Protected {
    pub(super) user_home: Option<PathBuf>,
    pub(super) inside: Vec<PathBuf>,
}

impl Protected {
    /// Checks `folder` and returns it in canonical form, or why it may not be mounted.
    pub(super) fn check(&self, folder: &Path) -> Result<PathBuf, String> {
        let folder = folder
            .canonicalize()
            .map_err(|error| format!("{}: {error}", folder.display()))?;
        if !folder.is_dir() {
            return Err(format!("{} is not a folder", folder.display()));
        }
        if folder.parent().is_none() {
            return Err("the root of the file system cannot be mounted".into());
        }
        if let Some(home) = self.user_home.as_deref().and_then(|home| home.canonicalize().ok())
            && home.starts_with(&folder)
        {
            return Err(format!(
                "{} holds your whole home folder; choose a project folder inside it",
                folder.display()
            ));
        }
        for protected in &self.inside {
            let protected = protected.canonicalize().unwrap_or_else(|_| protected.clone());
            if folder.starts_with(&protected) || protected.starts_with(&folder) {
                return Err(format!(
                    "{} overlaps {}, which must stay out of agents",
                    folder.display(),
                    protected.display()
                ));
            }
        }
        Ok(folder)
    }
}

/// The agent manifests in `folder`: in it, in its direct subfolders, and in `agents/*/`, where
/// repositories keep their agents.
pub(super) fn found_manifests(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut directories = vec![folder.to_path_buf()];
    directories.extend(subdirectories(folder));
    directories.extend(subdirectories(&folder.join("agents")));
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_manifest_filename(&path) && !found.contains(&path) {
                found.push(path);
            }
        }
    }
    // Each folder's agent first, then its variants, so the default choice is the agent itself.
    found.sort_by_key(|path| {
        (
            path.parent().map(Path::to_path_buf),
            manifest::variant_from_filename(path).is_some(),
            path.clone(),
        )
    });
    found
}

fn subdirectories(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut directories: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    directories.sort();
    directories
}

/// The Git repository an agent's manifest is in. A relative bind mount source in the manifest
/// mounts a part of it, such as `../..` for the whole repository from `agents/minimal`, so it
/// needs a checkout of that repository on this machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Repository {
    /// Where setup reads it: the temporary checkout it fetched, or the local one the manifest is in.
    pub(super) root: PathBuf,
    /// Its name, such as `altinn-studio`.
    pub(super) name: String,
    /// Whether `root` is the temporary checkout, which is removed when setup ends.
    pub(super) fetched: bool,
}

impl Repository {
    /// The local Git checkout `folder` is in, by the `.git` at its root.
    pub(super) fn containing(folder: &Path) -> Option<Self> {
        let folder = folder.canonicalize().ok()?;
        let root = folder.ancestors().find(|folder| folder.join(".git").exists())?;
        Some(Self {
            name: root.file_name()?.to_string_lossy().into_owned(),
            root: root.to_path_buf(),
            fetched: false,
        })
    }

    /// The temporary checkout of the repository `name` at `root`.
    pub(super) fn fetched(root: &Path, name: &str) -> Result<Self, Error> {
        Ok(Self {
            root: root.canonicalize()?,
            name: name.to_owned(),
            fetched: true,
        })
    }
}

/// A bind mount of the agent's own repository.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct RepositoryMount {
    /// Its place among the agent's mounts.
    pub(super) index: usize,
    /// The folder it mounts, relative to the repository's root; empty for the whole repository.
    pub(super) within: PathBuf,
}

/// The bind mounts of `agent`, whose manifest is in `source_directory`, that mount a part of
/// `repository` by a relative source. One that climbs out of the repository is refused: it would
/// mount whatever lies beside the checkout, which for a fetched one is setup's own state.
pub(super) fn repository_mounts(
    agent: &Agent,
    source_directory: &Path,
    repository: &Repository,
) -> Result<Vec<RepositoryMount>, Error> {
    let source_directory = source_directory.canonicalize()?;
    let mut found = Vec::new();
    for (index, mount) in agent.spec.sandbox.mounts.iter().enumerate() {
        let MountSpec::Bind { source, .. } = mount else {
            continue;
        };
        if !is_relative_source(source) {
            continue;
        }
        let resolved = normalized(&source_directory.join(source));
        let within = resolved.strip_prefix(&repository.root).map_err(|_| {
            Error::Invalid(format!(
                "spec.sandbox.mounts[{index}].source {} leaves the repository {} the manifest is in; mount a folder \
                 of the repository, or give an absolute path",
                source.display(),
                repository.name
            ))
        })?;
        found.push(RepositoryMount {
            index,
            within: within.to_path_buf(),
        });
    }
    Ok(found)
}

/// Whether a manifest's bind mount `source` is resolved against the manifest's folder, rather
/// than being absolute or below the home directory. A path from the root without a drive, such as
/// `/srv/data` on Windows, is not absolute there, but it is not resolved against the folder either.
pub(super) fn is_relative_source(source: &Path) -> bool {
    source.is_relative() && !source.has_root() && !source.starts_with("~")
}

/// `path` with `.` and `..` worked out from the path alone, as it reads, not as links lead.
fn normalized(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// The read-write bind mount that puts `folder` in the agent's working directory under its own
/// name, as a checkout cloned there would be.
pub(super) fn bind_into_working_directory(os: &str, folder: &Path) -> Result<MountSpec, Error> {
    Ok(MountSpec::Bind {
        source: folder.to_path_buf(),
        target: working_directory_target(os, folder)?,
        read_only: false,
    })
}

/// Where [`bind_into_working_directory`] puts `folder` in the sandbox.
pub(super) fn working_directory_target(os: &str, folder: &Path) -> Result<SandboxPath, Error> {
    let working = agent::sandbox::platform::working_directory(os)
        .ok_or_else(|| Error::Invalid(format!("cannot mount folders into a {os} agent")))?;
    let name = folder
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .replace(|character: char| character == '/' || character.is_control(), "-")
        })
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .ok_or_else(|| Error::Invalid(format!("{} has no folder name to mount it under", folder.display())))?;
    Ok(SandboxPath::new(format!("{working}/{name}")))
}

/// The folders no agent may mount, nor any folder holding them.
pub(super) fn protected_folders(agents_root: &Path, home: &ControlPlaneHome) -> Protected {
    Protected {
        user_home: agent::local::home::user_home_directory(),
        inside: vec![
            agents_root.to_path_buf(),
            home.path().to_path_buf(),
            agent::local::home::user_home_directory()
                .map(|home| home.join(".ssh"))
                .unwrap_or_default(),
        ],
    }
}

/// Asks which checkout of the agent's repository the manifest's mounts of that repository mount.
/// A fetched repository is removed when setup ends, so it needs one on this machine, and the
/// mounts are left out without one. A local manifest's mounts mount the checkout it is in unless
/// the person names another, such as a worktree of it. The mounts keep the targets the manifest
/// gives them.
pub(super) fn choose_checkout(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    protected: &Protected,
) -> CommandResult<()> {
    let Some(repository) = draft.source.repository.clone() else {
        return Ok(());
    };
    let found = repository_mounts(&draft.agent, &draft.source.directory, &repository)?;
    if found.is_empty() {
        return Ok(());
    }
    let targets: Vec<String> = found
        .iter()
        .filter_map(|mount| match &draft.agent.spec.sandbox.mounts[mount.index] {
            agent::MountSpec::Bind { target, .. } => Some(target.as_str().to_owned()),
            agent::MountSpec::Tmpfs { .. } => None,
        })
        .collect();
    let name = &repository.name;
    let at = targets.join(" and ");
    let local = repository.root.display().to_string();
    let default = if repository.fetched { "" } else { local.as_str() };
    let check = |answer: &str| {
        if answer.trim().is_empty() {
            return Ok(());
        }
        let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
        let checkout = protected.check(&path)?;
        found
            .iter()
            .map(|mount| checkout.join(&mount.within))
            .find(|folder| !folder.is_dir())
            .map_or(Ok(()), |missing| {
                Err(format!("{} has no folder {}", checkout.display(), missing.display()))
            })
    };
    let answer = match &arguments.checkout {
        Some(path) => path.display().to_string(),
        None if answers.interactive() && !arguments.yes => {
            let question = if repository.fetched {
                format!(
                    "The agent mounts its repository, {name}, at {at}.\nWhich checkout of {name} on this machine \
                     should it mount? (Enter to leave the mount out)"
                )
            } else {
                format!(
                    "The agent mounts its repository, {name}, at {at}.\nWhich checkout of {name} should it mount, \
                     such as another worktree of it?"
                )
            };
            answers.text("--checkout", &question, Some(default), check)?
        }
        None => default.to_owned(),
    };
    if answer.trim().is_empty() {
        eprintln!(
            "warning: left out the mount at {at}: it mounts {name}, which setup fetched only for the agent \
             definition. Pass --checkout with a checkout of {name} to mount it."
        );
        for mount in found.iter().rev() {
            draft.agent.spec.sandbox.mounts.remove(mount.index);
        }
        return Ok(());
    }
    check(&answer).map_err(Error::Invalid)?;
    let checkout = protected
        .check(&manifest::expand_host_path(Path::new(answer.trim()))?)
        .map_err(Error::Invalid)?;
    for mount in &found {
        if let agent::MountSpec::Bind { source, .. } = &mut draft.agent.spec.sandbox.mounts[mount.index] {
            // Joining an empty path would add a trailing separator.
            *source = if mount.within.as_os_str().is_empty() {
                checkout.clone()
            } else {
                checkout.join(&mount.within)
            };
        }
    }
    Ok(())
}

/// The host folders `draft` mounts already, resolved, with where each appears in the sandbox.
pub(super) fn bind_mounts(draft: &Draft) -> Result<Vec<(PathBuf, String)>, Error> {
    draft
        .agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            agent::MountSpec::Bind { source, target, .. } => Some((source, target)),
            agent::MountSpec::Tmpfs { .. } => None,
        })
        .map(|(source, target)| {
            let source = manifest::resolve_host_path(&draft.source.directory, source, "spec.sandbox.mounts")?;
            Ok((source.canonicalize().unwrap_or(source), target.as_str().to_owned()))
        })
        .collect()
}

/// Asks for a folder to mount and adds the mount. The question names what the manifest mounts
/// already, and a folder mounted already, or one that would land where a mount is, is refused.
/// Returns a manifest to start from instead, with the folder, when a standard agent was chosen
/// and the folder holds agent definitions the person chooses one of.
pub(super) fn choose_mount(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    protected: &Protected,
) -> CommandResult<Option<(PathBuf, PathBuf)>> {
    if arguments.no_mount {
        return Ok(None);
    }
    let mounted = bind_mounts(draft)?;
    let os = draft.agent.spec.sandbox.platform.os.clone();
    let check = |answer: &str| {
        if answer.trim().is_empty() {
            return Ok(());
        }
        let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
        let folder = protected.check(&path)?;
        if let Some((_, target)) = mounted.iter().find(|(source, _)| *source == folder) {
            return Err(format!("{} is mounted already, at {target}", folder.display()));
        }
        let target = working_directory_target(&os, &folder).map_err(|error| error.to_string())?;
        if mounted.iter().any(|(_, mounted)| mounted == target.as_str()) {
            return Err(format!(
                "the agent mounts another folder at {} already; choose a folder of another name",
                target.as_str()
            ));
        }
        Ok(())
    };
    let answer = match &arguments.mount {
        Some(path) => path.display().to_string(),
        None if answers.interactive() && !arguments.yes => {
            let mut question: Vec<String> = mounted
                .iter()
                .map(|(source, target)| format!("The agent mounts {} at {target} already.", source.display()))
                .collect();
            question.push(if mounted.is_empty() {
                "Folder on this machine to work on in the agent (Enter to skip)".to_owned()
            } else {
                "Another folder to work on in the agent (Enter to skip)".to_owned()
            });
            answers.text("--mount", &question.join("\n"), Some(""), check)?
        }
        None => return Ok(None),
    };
    if answer.trim().is_empty() {
        answers.record("--no-mount", &[]);
        return Ok(None);
    }
    check(&answer).map_err(Error::Invalid)?;
    let folder = protected
        .check(&manifest::expand_host_path(Path::new(answer.trim()))?)
        .map_err(Error::Invalid)?;
    if draft.source.from_catalog
        && let Some(manifest) = offer_found_manifests(answers, &folder)?
    {
        return Ok(Some((manifest, folder)));
    }
    mount_unless_mounted(draft, &folder)?;
    Ok(None)
}

/// Mounts `folder` in the agent's working directory, unless the agent mounts it already, as the
/// repository mount of an agent chosen from it does.
pub(super) fn mount_unless_mounted(draft: &mut Draft, folder: &Path) -> CommandResult<()> {
    if bind_mounts(draft)?.iter().any(|(source, _)| source == folder) {
        return Ok(());
    }
    let os = draft.agent.spec.sandbox.platform.os.clone();
    draft
        .agent
        .spec
        .sandbox
        .mounts
        .push(bind_into_working_directory(&os, folder)?);
    Ok(())
}

/// When the folder holds agent definitions, offers to start from one of them.
pub(super) fn offer_found_manifests(answers: &Answers, folder: &Path) -> CommandResult<Option<PathBuf>> {
    let found = found_manifests(folder);
    if found.is_empty() {
        return Ok(None);
    }
    let mut choices = vec![Choice::new(None, "No, keep the agent chosen above")];
    choices.extend(found.into_iter().map(|manifest| {
        let label = format!("Use {}", manifest_label(folder, &manifest));
        let from = manifest.display().to_string();
        Choice::new(Some(manifest), label)
            .flag("--source", &["team"])
            .flag("--from", &[&from])
    }));
    let question = format!(
        "{} defines agents of its own. Start from one of them instead?",
        folder.display()
    );
    let chosen = answers.choose("--from", &question, choices, 0)?;
    if chosen.is_some() {
        answers.forget("--archetype");
    }
    Ok(chosen)
}

/// Checks every folder the agent mounts before anything is written: each must exist and hold no
/// `.env`, whose values the agent would see. agentd refuses either when it creates the agent, after
/// the folder is written.
pub(super) fn check_mounts(draft: &Draft) -> CommandResult<()> {
    for (source, target) in bind_mounts(draft)? {
        if !source.is_dir() {
            return Err(CommandError::Message(format!(
                "the agent mounts {} at {target}, but it is not a folder here",
                source.display()
            )));
        }
        if let Some(found) = agent::control_plane::find_dot_env(&source)? {
            return Err(CommandError::Message(format!(
                "the agent mounts {} at {target}, which holds {}; the agent would see its values. Move it out of \
                 the folder, or mount another",
                source.display(),
                found.display()
            )));
        }
    }
    Ok(draft.agent.validate()?)
}

/// Refuses a relative bind mount that resolves into setup's staging folder outside a repository
/// it knows, as of a standard agent extracted there: the folder is removed when setup ends.
pub(super) fn refuse_staged_mounts(draft: &Draft, staging: &Path) -> CommandResult<()> {
    if draft.source.repository.is_some() {
        return Ok(());
    }
    let staging = staging.canonicalize().unwrap_or_else(|_| staging.to_path_buf());
    for (source, target) in bind_mounts(draft)? {
        if source.starts_with(&staging) {
            return Err(CommandError::Message(format!(
                "the agent mounts {} at {target}, a folder setup removes when it ends",
                source.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fmt::Write as _;

    use super::{Protected, Repository, RepositoryMount, found_manifests, repository_mounts};

    #[test]
    fn manifests_are_found_beside_one_level_down_and_in_agents() {
        let folder = tempfile::tempdir().expect("a folder");
        let root = folder.path();
        for path in [
            "agent.yaml",
            "tool/agent.nested.yaml",
            "agents/minimal/agent.yaml",
            "agents/minimal/agent.worktree.yaml",
            "deep/er/agent.yaml",
            ".hidden/agent.yaml",
            "tool/notes.yaml",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
            std::fs::write(path, "").expect("a file");
        }
        let found: Vec<_> = found_manifests(root)
            .into_iter()
            .map(|path| {
                let below = path.strip_prefix(root).expect("below");
                let parts: Vec<_> = below
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect();
                parts.join("/")
            })
            .collect();
        assert_eq!(
            found,
            [
                "agent.yaml",
                "agents/minimal/agent.yaml",
                "agents/minimal/agent.worktree.yaml",
                "tool/agent.nested.yaml"
            ]
        );
    }

    #[test]
    fn home_and_protected_folders_cannot_be_mounted() {
        let home = tempfile::tempdir().expect("a home");
        let project = home.path().join("code/project");
        let agents = home.path().join("agents");
        std::fs::create_dir_all(&project).expect("a project");
        std::fs::create_dir_all(agents.join("worker")).expect("an agent folder");
        let protected = Protected {
            user_home: Some(home.path().to_path_buf()),
            inside: vec![agents.clone()],
        };
        assert!(protected.check(&project).is_ok());
        assert!(protected.check(home.path()).is_err());
        assert!(protected.check(&agents.join("worker")).is_err());
        assert!(protected.check(&home.path().join("missing")).is_err());
    }

    fn agent_mounting(sources: &[&str]) -> agent::Agent {
        let mut mounts = String::new();
        for (index, source) in sources.iter().enumerate() {
            let _ = writeln!(
                mounts,
                "      - {{type: bind, source: {source:?}, target: /srv/m{index}, readOnly: false}}"
            );
        }
        let yaml = format!(
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {{name: worker}}\nspec:\n  sandbox:\n    \
             image: {{type: reference, reference: example.com/agent}}\n    platform: {{os: linux}}\n    \
             resources: {{cpu: '2', memory: 4Gi, rootFilesystem: {{capacity: 20Gi, mode: layered}}}}\n    \
             mounts:\n{mounts}  home: {{source: home}}\n  harnesses: [{{type: pi}}]\n  \
             network: {{mode: mediated, allow: all}}\n"
        );
        serde_yaml_ng::from_str(&yaml).expect("an agent")
    }

    #[test]
    fn relative_mounts_are_parts_of_the_repository_and_one_leaving_it_is_refused() {
        let checkout = tempfile::tempdir().expect("a checkout");
        let manifest_folder = checkout.path().join("agents/desktop");
        std::fs::create_dir_all(&manifest_folder).expect("folders");
        std::fs::create_dir(checkout.path().join(".git")).expect("a .git");
        let repository = Repository::containing(&manifest_folder).expect("a repository");
        assert_eq!(repository.root, checkout.path().canonicalize().expect("canonical"));
        assert!(!repository.fetched);

        let agent = agent_mounting(&["../..", "../../src", "/srv/data", "~/notes"]);
        let found = repository_mounts(&agent, &manifest_folder, &repository).expect("mounts");
        assert_eq!(
            found,
            [
                RepositoryMount {
                    index: 0,
                    within: "".into()
                },
                RepositoryMount {
                    index: 1,
                    within: "src".into()
                },
            ]
        );
        let leaving = agent_mounting(&["../../.."]);
        let error = repository_mounts(&leaving, &manifest_folder, &repository).expect_err("leaves the repository");
        assert!(error.to_string().contains("leaves the repository"), "{error}");
    }
}
