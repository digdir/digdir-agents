//! The agent definitions on disk: the manifests and variants a person or client can apply.
//!
//! Every client that offers to create an agent looks for them the same way, so they offer the same ones. The files are
//! not resources in `agentd`, which knows only the agents applied from them.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    Agent, AgentVariantName,
    manifest::{self, MANIFEST_FILE},
};

/// How deep below a repository's root manifests are looked for.
const DISCOVERY_DEPTH: usize = 8;

/// Returns the root of the git repository containing `directory`, if any.
///
/// A linked worktree keeps `.git` as a file, so only presence is checked.
#[must_use]
pub fn repository_root(directory: &Path) -> Option<&Path> {
    directory.ancestors().find(|ancestor| ancestor.join(".git").exists())
}

/// Lists the manifests and their variants for `directory`, each agent's `agent.yaml` before its variants.
///
/// Inside a git repository the whole repository is walked, honoring ignore files, so an agent is found from anywhere
/// in its repository. Outside one only `directory/agent.yaml` and `directory/agents/*/agent.yaml` are checked, so
/// running from a home directory never walks it. Each agent directory is then read directly, so that checkout-local,
/// ignored `agent.<variant>.yaml` siblings are still found.
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
    /// The root of the git repository it is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<PathBuf>,
    /// The names of the applied agents that came from this file. `apply --name` makes more than one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

/// Lists the definitions [`manifests`] finds for `directory`, with the agents in `applied` that came from each.
///
/// It reads the filesystem, so an async caller runs it off its event loop.
#[must_use]
pub fn list(directory: &Path, applied: &[Agent]) -> Vec<Definition> {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let sources: Vec<(PathBuf, &str)> = applied
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
            let mut agents: Vec<String> = sources
                .iter()
                .filter(|(source, _)| *source == file)
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
