//! The agent definitions on disk: the manifests and variants a person or client can apply, and the definition sources
//! that hold them.
//!
//! Every client that offers to create an agent looks for them the same way, so they offer the same ones.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::{
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

/// A definition a client can apply or run, as agentd lists it: a manifest on disk, or a definition agentd carries.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DefinitionSummary {
    /// Its manifest, in agentd's copy of it for a definition agentd carries, which is never changed.
    pub path: PathBuf,
    /// The other paths that lead to the same manifest, such as through a symbolic link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<PathBuf>,
    /// For a definition agentd carries, the reference that runs it, in its full form, such as
    /// `bundled/agent-builder`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The name it gives its agent, `metadata.name`, when it can be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Why it cannot be read, in place of its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The targets of its bind mounts without a source, which running it needs a folder for each of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<::sandbox::SandboxPath>,
}

/// Lists the definitions on disk for `directory`, as [`manifests`] finds them, then the `recorded` manifests agents
/// were applied from, each file once however many paths lead to it.
///
/// A recorded manifest that cannot be read is still listed, with why, and so is one found that cannot. One found that
/// has gone since is left out.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn on_disk(directory: Option<&Path>, recorded: Vec<PathBuf>) -> Vec<DefinitionSummary> {
    let found = directory.map_or_else(Vec::new, manifests);
    merge(found, recorded, |path| {
        manifest::resolve(path).map(|resolved| resolved.agent)
    })
    .into_iter()
    .map(|listed| {
        let (name, error, slots) = match listed.read {
            Ok(agent) => (Some(agent.metadata.name.clone()), None, unsourced_binds(&agent)),
            Err(error) => (None, Some(error), Vec::new()),
        };
        DefinitionSummary {
            path: listed.path,
            aliases: listed.aliases,
            reference: None,
            name,
            error,
            slots,
        }
    })
    .collect()
}

/// The targets of `agent`'s bind mounts without a source, which running it needs a folder for each of.
#[must_use]
pub fn unsourced_binds(agent: &crate::Agent) -> Vec<::sandbox::SandboxPath> {
    agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            crate::MountSpec::Bind {
                source: None, target, ..
            } => Some(target.clone()),
            _ => None,
        })
        .collect()
}

/// A file [`merge`] lists, with what reading it gave.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listed<T> {
    /// The path it was first reached by.
    pub path: PathBuf,
    /// The other paths that lead to it.
    pub aliases: Vec<PathBuf>,
    /// What reading it gave, or why it cannot be read.
    pub read: Result<T, String>,
}

/// Merges the manifests `found` for a folder with the `recorded` ones resources were applied from into one entry per
/// file, however many paths lead to it, each read with `read`.
///
/// A recorded manifest that cannot be read is listed so its error is visible, as is one found that exists. One found
/// that has gone since it was found is left out.
///
/// Reads the filesystem, so it blocks.
pub fn merge<T>(
    found: Vec<PathBuf>,
    mut recorded: Vec<PathBuf>,
    read: impl Fn(&Path) -> Result<T, crate::Error>,
) -> Vec<Listed<T>> {
    recorded.sort();
    recorded.dedup();
    let paths = found
        .into_iter()
        .map(|path| (path, false))
        .chain(recorded.into_iter().map(|path| (path, true)));
    let mut seen = std::collections::HashMap::<PathBuf, usize>::new();
    let mut listed = Vec::<Listed<T>>::new();
    for (path, recorded) in paths {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(index) = seen.get(&canonical).copied() {
            let entry = &mut listed[index];
            if entry.path != path && !entry.aliases.contains(&path) {
                entry.aliases.push(path);
            }
            continue;
        }
        let read = match read(&path) {
            Ok(value) => Ok(value),
            Err(error) if recorded || path.exists() => Err(error.to_string()),
            Err(_) => continue,
        };
        seen.insert(canonical, listed.len());
        listed.push(Listed {
            path,
            aliases: Vec::new(),
            read,
        });
    }
    listed
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{DefinitionSource, DefinitionSummary, on_disk, source};
    use crate::manifest::MANIFEST_FILE;

    fn manifest_yaml(name: &str) -> String {
        manifest_with(name, "")
    }

    /// A manifest named `name`, with `mounts` as its `spec.sandbox.mounts` lines.
    fn manifest_with(name: &str, mounts: &str) -> String {
        format!(
            "apiVersion: agents.platform/v1alpha1\n\
             kind: Agent\n\
             metadata:\n\
             \x20 name: {name}\n\
             spec:\n\
             \x20 sandbox:\n\
             \x20   image:\n\
             \x20     type: build\n\
             \x20     context: .\n\
             \x20     dockerfile: Dockerfile\n\
             \x20   platform:\n\
             \x20     os: linux\n\
             \x20   resources:\n\
             \x20     cpu: \"1\"\n\
             \x20     memory: \"1Gi\"\n\
             \x20     rootFilesystem:\n\
             \x20       capacity: \"8Gi\"\n\
             \x20       mode: layered\n\
             {mounts}\
             \x20 harnesses:\n\
             \x20   - type: pi\n\
             \x20 secrets: []\n\
             \x20 network:\n\
             \x20   mode: mediated\n\
             \x20   allow: all\n"
        )
    }

    fn manifest_directory(root: &Path, name: &str, content: &str) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).expect("manifest directory should be created");
        std::fs::write(directory.join(MANIFEST_FILE), content).expect("manifest should be written");
        directory
    }

    fn empty_directory(root: &Path, name: &str) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).expect("directory should be created");
        directory
    }

    fn paths(listed: &[DefinitionSummary]) -> Vec<&Path> {
        listed.iter().map(|definition| definition.path.as_path()).collect()
    }

    #[test]
    fn the_working_directorys_definitions_come_before_the_recorded_ones_and_each_once() {
        let root = tempfile::tempdir().expect("temporary directory");
        let cwd = manifest_directory(root.path(), "a-cwd", &manifest_yaml("local"));
        let broken = manifest_directory(root.path(), "broken", "not a manifest");
        let missing = empty_directory(root.path(), "missing");
        let recorded = manifest_directory(root.path(), "recorded", &manifest_yaml("recorded"));

        let listed = on_disk(
            Some(&cwd),
            [&recorded, &cwd, &missing, &broken]
                .map(|directory| directory.join(MANIFEST_FILE))
                .to_vec(),
        );

        assert_eq!(
            paths(&listed),
            [
                cwd.join(MANIFEST_FILE),
                broken.join(MANIFEST_FILE),
                missing.join(MANIFEST_FILE),
                recorded.join(MANIFEST_FILE)
            ]
        );
        assert_eq!(listed[0].name.as_deref(), Some("local"));
        assert!(listed[1].error.is_some() && listed[1].name.is_none());
        assert!(
            listed[2].error.is_some(),
            "a recorded manifest that has gone is listed with why"
        );
        assert_eq!(listed[3].name.as_deref(), Some("recorded"));
    }

    #[test]
    fn a_definition_lists_the_targets_of_its_bind_mounts_without_a_source() {
        let root = tempfile::tempdir().expect("temporary directory");
        let mounts = "\x20   mounts:\n\
                      \x20     - type: bind\n\
                      \x20       target: /work/checkout\n\
                      \x20       readOnly: true\n\
                      \x20     - type: bind\n\
                      \x20       source: .\n\
                      \x20       target: /work/own\n\
                      \x20       readOnly: true\n";
        let cwd = manifest_directory(root.path(), "cwd", &manifest_with("builder", mounts));

        let listed = on_disk(Some(&cwd), Vec::new());

        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].slots, [::sandbox::SandboxPath::new("/work/checkout")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_manifest_reached_by_another_path_keeps_it_as_an_alias() {
        let root = tempfile::tempdir().expect("temporary directory");
        let source = manifest_directory(root.path(), "source", &manifest_yaml("worker"));
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&source, &alias).expect("manifest directory symlink");

        let listed = on_disk(Some(&source), vec![alias.join(MANIFEST_FILE)]);

        assert_eq!(paths(&listed), [source.join(MANIFEST_FILE)]);
        assert_eq!(listed[0].aliases, [alias.join(MANIFEST_FILE)]);
    }

    #[test]
    fn a_repository_is_walked_but_not_its_hidden_or_ignored_folders() {
        let root = tempfile::tempdir().expect("temporary directory");
        let cwd = manifest_directory(root.path(), "cwd", &manifest_yaml("top"));
        std::fs::write(cwd.join(".git"), "gitdir: elsewhere\n").expect("worktree marker should be written");
        let nested = manifest_directory(&cwd, "examples/deeper", &manifest_yaml("nested"));
        let sibling = manifest_directory(&cwd, "examples/other", &manifest_yaml("other"));
        manifest_directory(&cwd, ".hidden", &manifest_yaml("hidden"));
        manifest_directory(&cwd, "target/ignored", &manifest_yaml("ignored"));
        std::fs::write(cwd.join(".gitignore"), "target/\n").expect("ignore file should be written");

        let listed = on_disk(Some(&cwd), vec![nested.join(MANIFEST_FILE)]);

        assert_eq!(
            paths(&listed),
            [
                cwd.join(MANIFEST_FILE),
                nested.join(MANIFEST_FILE),
                sibling.join(MANIFEST_FILE)
            ]
        );
        assert_eq!(listed[1].name.as_deref(), Some("nested"));
    }

    #[test]
    fn outside_a_repository_only_the_conventional_layout_is_checked() {
        let root = tempfile::tempdir().expect("temporary directory");
        let cwd = manifest_directory(root.path(), "cwd", &manifest_yaml("top"));
        let dev = manifest_directory(&cwd, "agents/dev", &manifest_yaml("dev"));
        std::fs::write(
            dev.join("agent.mine.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata:\n  name: mine\n",
        )
        .expect("local variant");
        empty_directory(&cwd, "agents/empty");
        manifest_directory(&cwd, "projects/checkout", &manifest_yaml("unrelated"));
        manifest_directory(&cwd, "agents/dev/nested", &manifest_yaml("nested"));

        let listed = on_disk(Some(&cwd), Vec::new());

        assert_eq!(
            paths(&listed),
            [
                cwd.join(MANIFEST_FILE),
                dev.join(MANIFEST_FILE),
                dev.join("agent.mine.yaml")
            ]
        );
        assert_eq!(listed[2].name.as_deref(), Some("mine"));
    }

    #[test]
    fn ignored_sibling_variants_are_listed_with_their_errors() {
        let root = tempfile::tempdir().expect("temporary directory");
        let agent = manifest_directory(root.path(), "configured-agent", &manifest_yaml("default"));
        std::fs::write(agent.join(".gitignore"), "agent.*.yaml\n").expect("Agent ignore file");
        std::fs::write(
            agent.join("agent.mine.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata:\n  name: mine\n",
        )
        .expect("local variant");
        std::fs::write(
            agent.join("agent.broken.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: missing.yaml\nmetadata:\n  name: broken\n",
        )
        .expect("broken local variant");

        let listed = on_disk(Some(&agent), Vec::new());

        assert_eq!(
            paths(&listed),
            [
                agent.join(MANIFEST_FILE),
                agent.join("agent.broken.yaml"),
                agent.join("agent.mine.yaml")
            ]
        );
        assert_eq!(listed[0].name.as_deref(), Some("default"));
        assert!(
            listed[1]
                .error
                .as_ref()
                .is_some_and(|error| error.contains("extends must name"))
        );
        assert_eq!(listed[2].name.as_deref(), Some("mine"));
    }

    #[test]
    fn the_whole_repository_is_walked_from_a_folder_inside_it() {
        let root = tempfile::tempdir().expect("temporary directory");
        let repository = manifest_directory(root.path(), "repository", &manifest_yaml("root"));
        std::fs::write(repository.join(".git"), "gitdir: elsewhere\n").expect("worktree marker should be written");
        let cwd = empty_directory(&repository, "src/deep/inside");
        let sibling = manifest_directory(&repository, "agents/full", &manifest_yaml("full"));
        manifest_directory(root.path(), "outside", &manifest_yaml("outside"));

        let listed = on_disk(Some(&cwd), Vec::new());

        assert_eq!(
            paths(&listed),
            [repository.join(MANIFEST_FILE), sibling.join(MANIFEST_FILE)]
        );
    }

    #[test]
    fn a_recorded_manifest_keeps_its_filename() {
        let root = tempfile::tempdir().expect("temporary directory");
        let source = empty_directory(root.path(), "custom");
        let manifest = source.join("worker.yml");
        std::fs::write(&manifest, manifest_yaml("custom")).expect("manifest should be written");

        let listed = on_disk(None, vec![manifest.clone()]);

        assert_eq!(paths(&listed), [manifest.as_path()]);
        assert_eq!(listed[0].name.as_deref(), Some("custom"));
    }

    #[test]
    fn a_folder_without_a_manifest_lists_none_of_its_own_but_never_hides_a_recorded_one() {
        let root = tempfile::tempdir().expect("temporary directory");
        let cwd = empty_directory(root.path(), "cwd");

        assert_eq!(on_disk(Some(&cwd), Vec::new()), []);
        assert_eq!(on_disk(None, Vec::new()), []);
        let listed = on_disk(Some(&cwd), vec![cwd.join(MANIFEST_FILE)]);
        assert_eq!(paths(&listed), [cwd.join(MANIFEST_FILE)]);
        assert!(listed[0].error.is_some());
    }

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
