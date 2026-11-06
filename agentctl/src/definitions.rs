//! The agent definitions on disk: the manifests and variants a person or client can apply, and the definition sources
//! that hold them.
//!
//! Every client that offers to create an agent looks for them the same way, so they offer the same ones. The files are
//! not resources in `agentd`, which knows only the agents applied from them.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    Agent, AgentVariantName,
    local::repository_root,
    manifest::{self, MANIFEST_FILE},
};

/// A definition source: a folder that holds agent definitions, as the glossary has it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionSource<'a> {
    /// A Git repository, as [`repository_root`] counts one. Its agents are found from anywhere in it.
    Repository(&'a Path),
    /// The folder holding `agents/<name>/` outside any repository, such as the home directory. Its agents are found
    /// only from the folder itself, as the folders below it are anyone's.
    Folder(&'a Path),
}

impl<'a> DefinitionSource<'a> {
    /// The source's folder.
    #[must_use]
    pub const fn path(self) -> &'a Path {
        match self {
            Self::Repository(path) | Self::Folder(path) => path,
        }
    }
}

/// Returns the definition source holding the agent definition in `definition`, the directory an agent was applied
/// from.
///
/// That is the repository holding it or, outside one, the folder holding its `agents/`. It is inferred from the
/// folders, as agentd does not keep definition sources of its own yet.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn source(definition: &Path) -> Option<DefinitionSource<'_>> {
    if let Some(root) = repository_root(definition) {
        return Some(DefinitionSource::Repository(root));
    }
    let agents = definition.parent()?;
    (agents.file_name()? == "agents")
        .then(|| agents.parent())
        .flatten()
        .map(DefinitionSource::Folder)
}

/// How deep below a repository's root manifests are looked for.
const DISCOVERY_DEPTH: usize = 8;

/// Lists the manifests and their variants for `directory`, each agent's `agent.yaml` before its variants.
///
/// Inside a git repository, as [`repository_root`] counts one, the whole repository is walked, honoring ignore
/// files. Outside one only `directory/agent.yaml` and `directory/agents/*/agent.yaml` are checked, so running from a
/// home directory never walks it. Each agent directory is then read directly, so that checkout-local, ignored
/// `agent.<variant>.yaml` siblings are still found.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn manifests(directory: &Path) -> Vec<PathBuf> {
    let agents: Vec<PathBuf> = repository_root(directory).map_or_else(
        || {
            std::iter::once(directory.join(MANIFEST_FILE))
                .chain(
                    std::fs::read_dir(directory.join("agents"))
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .map(|entry| entry.path().join(MANIFEST_FILE)),
                )
                .filter(|path| path.is_file())
                .collect()
        },
        |root| {
            WalkBuilder::new(root)
                .max_depth(Some(DISCOVERY_DEPTH))
                .require_git(false)
                .follow_links(false)
                .build()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_some_and(|kind| kind.is_file()) && entry.file_name() == MANIFEST_FILE
                })
                .map(ignore::DirEntry::into_path)
                .collect()
        },
    );
    let mut found: Vec<PathBuf> = agents
        .into_iter()
        .flat_map(|base| {
            let Some(parent) = base.parent() else {
                return vec![base];
            };
            let mut manifests = std::fs::read_dir(parent)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| manifest::is_manifest_filename(path))
                .collect::<Vec<_>>();
            manifests.sort_by_key(|path| (path.file_name().is_none_or(|name| name != MANIFEST_FILE), path.clone()));
            manifests
        })
        .collect();
    found.sort_by_key(|path| {
        (
            path.components().count(),
            path.parent().map(Path::to_path_buf),
            path.file_name().is_none_or(|name| name != MANIFEST_FILE),
            path.clone(),
        )
    });
    found
}

/// Largest file read to learn whether it is a volume manifest. A manifest is a few lines.
const VOLUME_MANIFEST_MAX_BYTES: u64 = 64 * 1024;

/// Lists the volume manifests for `directory`: the YAML files declaring `kind: Volume` anywhere in its definition
/// source's `agents/`, and beside every agent manifest [`manifests`] finds.
///
/// They are found by their kind rather than their filename, so a volume several agents share can live in a folder of
/// its own, such as `agents/_volumes/`, and one that belongs with an agent can sit in its definition.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn volume_manifests(directory: &Path) -> Vec<PathBuf> {
    let agents = repository_root(directory).unwrap_or(directory).join("agents");
    let walked = agents
        .is_dir()
        .then(|| files(&agents, DISCOVERY_DEPTH))
        .into_iter()
        .flatten();
    let beside_agents = manifests(directory)
        .into_iter()
        .filter_map(|manifest| manifest.parent().map(Path::to_path_buf))
        .flat_map(|definition| files(&definition, 1));
    let mut found: Vec<PathBuf> = walked
        .chain(beside_agents)
        .filter(|path| declares_volume(path))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The files below `directory`, at most `depth` levels down, skipping hidden and ignored ones.
fn files(directory: &Path, depth: usize) -> impl Iterator<Item = PathBuf> + use<> {
    WalkBuilder::new(directory)
        .max_depth(Some(depth))
        .require_git(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
}

/// Whether `path` is a YAML file whose `kind` is a volume's.
fn declares_volume(path: &Path) -> bool {
    let yaml = path
        .extension()
        .is_some_and(|extension| extension == "yaml" || extension == "yml");
    let small_file = || {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() <= VOLUME_MANIFEST_MAX_BYTES)
    };
    yaml && small_file()
        && std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_yaml_ng::from_slice::<serde_yaml_ng::Value>(&bytes).ok())
            .is_some_and(|value| value.get("kind").and_then(serde_yaml_ng::Value::as_str) == Some(crate::volumes::KIND))
}

/// One agent definition on disk.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Definition {
    /// The manifest file.
    pub path: PathBuf,
    /// The variant the file is, absent for an agent's `agent.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<AgentVariantName>,
    /// The `metadata.name` it declares, once its variants are resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Why it cannot be applied as it is, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The root of the Git repository it is in, as [`repository_root`] counts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<PathBuf>,
    /// The names of the applied agents that came from this file. `apply --name` makes more than one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

/// Lists the definitions [`manifests`] finds for `directory`, with the agents in `applied` that came from each.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn list(directory: &Path, applied: &[Agent]) -> Vec<Definition> {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let applied_from: Vec<(PathBuf, &str)> = applied
        .iter()
        .filter(|agent| agent.metadata.deletion_timestamp.is_none())
        .filter_map(|agent| {
            let provenance = agent.status.provenance.as_ref()?;
            Some((
                canonical(&provenance.manifest_or_default()),
                agent.metadata.name.as_str(),
            ))
        })
        .collect();
    manifests(directory)
        .into_iter()
        .map(|path| {
            let file = canonical(&path);
            let (name, error) = match manifest::resolve(&path) {
                Ok(resolved) => (Some(resolved.agent.metadata.name), None),
                Err(error) => (None, Some(error.to_string())),
            };
            let mut agents: Vec<String> = applied_from
                .iter()
                .filter(|(from, _)| *from == file)
                .map(|(_, name)| (*name).to_owned())
                .collect();
            agents.sort();
            Definition {
                variant: manifest::variant_from_filename(&path),
                repository: repository_root(&path).map(Path::to_path_buf),
                path,
                name,
                error,
                agents,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DefinitionSource, source};

    #[test]
    fn a_definition_source_is_the_repository_or_the_folder_holding_agents() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let repository = temporary.path().join("repository");
        std::fs::create_dir_all(repository.join(".git")).expect("repository");
        std::fs::create_dir_all(repository.join("agents/worker")).expect("agent in the repository");
        let folder = temporary.path().join("folder");
        std::fs::create_dir_all(folder.join("agents/notes")).expect("agent outside a repository");
        std::fs::create_dir_all(temporary.path().join("loose/helper")).expect("agent outside agents/");

        assert_eq!(
            source(&repository.join("agents/worker")),
            Some(DefinitionSource::Repository(&repository))
        );
        assert_eq!(
            source(&folder.join("agents/notes")),
            Some(DefinitionSource::Folder(&folder))
        );
        assert_eq!(source(&temporary.path().join("loose/helper")), None);
    }
}
