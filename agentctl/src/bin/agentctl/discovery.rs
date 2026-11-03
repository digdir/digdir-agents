//! Finding agent manifests in a working tree, as the TUI's create form and `agentctl setup` offer
//! them.

use std::path::{Path, PathBuf};

use agent::manifest::{self, MANIFEST_FILE};
use ignore::WalkBuilder;

/// Deepest directory level below the working directory searched for manifests.
const DISCOVERY_DEPTH: usize = 8;

/// Returns the root of the git repository containing `directory`, if any.
///
/// A linked worktree keeps `.git` as a file, so only presence is checked.
pub(crate) fn repository_root(directory: &Path) -> Option<&Path> {
    directory.ancestors().find(|ancestor| ancestor.join(".git").exists())
}

/// Lists agents and their variant manifests for `directory`.
///
/// Inside a git repository the whole repository is walked, as [`repository_manifests`] does;
/// outside one only `directory/agent.yaml` and `directory/agents/*/agent.yaml` are checked, so
/// running from a home directory never walks it.
pub(crate) fn working_tree_manifests(directory: &Path) -> Vec<PathBuf> {
    repository_root(directory).map_or_else(
        || {
            with_variants(
                std::iter::once(directory.join(MANIFEST_FILE))
                    .chain(
                        std::fs::read_dir(directory.join("agents"))
                            .into_iter()
                            .flatten()
                            .filter_map(Result::ok)
                            .map(|entry| entry.path().join(MANIFEST_FILE)),
                    )
                    .filter(|path| path.is_file())
                    .collect(),
            )
        },
        repository_manifests,
    )
}

/// Lists agents and their variant manifests in the repository at `root`, such as a copy of one
/// that has no `.git`, walking it whole and honoring its ignore files.
pub(crate) fn repository_manifests(root: &Path) -> Vec<PathBuf> {
    with_variants(
        WalkBuilder::new(root)
            .max_depth(Some(DISCOVERY_DEPTH))
            .require_git(false)
            .follow_links(false)
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()) && entry.file_name() == MANIFEST_FILE)
            .map(ignore::DirEntry::into_path)
            .collect(),
    )
}

/// The `agents` manifests with the variants beside each, enumerated directly so checkout-local,
/// ignored `agent.<variant>.yaml` siblings remain discoverable.
fn with_variants(agents: Vec<PathBuf>) -> Vec<PathBuf> {
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

/// The manifests [`working_tree_manifests`] finds that are in `folder`, such as a team's agent
/// folder or a repository a person names, rather than anywhere in its repository.
pub(crate) fn manifests_in(folder: &Path) -> Vec<PathBuf> {
    let folder = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    working_tree_manifests(&folder)
        .into_iter()
        .filter(|path| path.canonicalize().is_ok_and(|path| path.starts_with(&folder)))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    use super::manifests_in;

    fn below(root: &Path, found: &[std::path::PathBuf]) -> Vec<String> {
        let root = root.canonicalize().expect("a canonical root");
        found
            .iter()
            .map(|path| {
                let path = path.canonicalize().expect("a canonical path");
                let parts: Vec<_> = path
                    .strip_prefix(&root)
                    .expect("below the root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                parts.join("/")
            })
            .collect()
    }

    #[test]
    fn a_folder_offers_the_agents_and_variants_in_it_but_not_the_rest_of_its_repository() {
        let repository = tempfile::tempdir().expect("a repository");
        let root = repository.path();
        std::fs::create_dir_all(root.join(".git")).expect("a checkout");
        for path in [
            "agent.yaml",
            "agents/minimal/agent.yaml",
            "agents/minimal/agent.mine.yaml",
            "deep/er/agent.yaml",
            ".hidden/agent.yaml",
            "agents/minimal/notes.yaml",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
            std::fs::write(path, "").expect("a file");
        }

        assert_eq!(
            below(root, &manifests_in(root)),
            [
                "agent.yaml",
                "agents/minimal/agent.yaml",
                "agents/minimal/agent.mine.yaml",
                "deep/er/agent.yaml"
            ]
        );
        assert_eq!(
            below(root, &manifests_in(&root.join("agents"))),
            ["agents/minimal/agent.yaml", "agents/minimal/agent.mine.yaml"]
        );
    }
}
