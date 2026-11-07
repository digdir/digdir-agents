//! The agent definitions on disk: the manifests and variants a person or client can apply.
//!
//! Every client that offers to create an agent looks for them the same way, so they offer the same ones.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::{
    local::repository_root,
    manifest::{self, MANIFEST_FILE},
};

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
