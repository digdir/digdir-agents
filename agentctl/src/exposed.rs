//! Files that may hold real keys, such as a checkout's `.env`, in the host folders an agent would
//! see, and the person's consent to let it.
//!
//! An agent can reach the internet, so a key it can read can leave the machine. agentd finds such
//! files in every folder an agent mounts or a volume copies and refuses to go on until the request
//! names each of them, which a client does once the person has agreed. A file that isn't named, such
//! as one created since, means asking again rather than mounting it quietly.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ::sandbox::SandboxPath;
use common::blocking;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::Error;

/// The name of a file that may hold real keys, matched without regard to case: `.env`, which tools
/// such as dotenv read keys for running an app locally from.
const NAME: &str = crate::control_plane::ENV_FILE;

/// Where to read how to move values out of an agent's environment file.
pub const ENVIRONMENT_FILE_GUIDE: &str =
    "https://github.com/digdir/digdir-agents/blob/main/agentctl/docs/moving-off-the-environment-file.md";

/// A file that may hold real keys in a host folder an agent would see.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExposedFile {
    /// The file's absolute host path, which consent names.
    pub path: PathBuf,
    /// The host folder the agent would see it in: a bind mount's source or a volume's.
    pub folder: PathBuf,
    /// Where an agent sees `folder`, when it's a bind mount's source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SandboxPath>,
    /// What is left to do about it when it's an agent's environment file: the file an agent records,
    /// or a `.env` beside an agent manifest, which agentctl reads values from by default. It holds the
    /// agent's own secrets, which belong in stored secrets instead, so consent never covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_file: Option<EnvironmentFile>,
}

impl ExposedFile {
    /// The file's path within its folder, as the person knows it in their checkout.
    #[must_use]
    pub fn relative(&self) -> &Path {
        self.path.strip_prefix(&self.folder).unwrap_or(&self.path)
    }

    /// The file as a person working in the directory `from` names it: by its path from there, or
    /// else within its folder, since the whole path is mostly what they know already.
    #[must_use]
    pub fn shown(&self, from: Option<&Path>) -> String {
        from.and_then(|from| self.path.strip_prefix(from).ok()).map_or_else(
            || format!("{} in {}", self.relative().display(), self.folder.display()),
            |path| path.display().to_string(),
        )
    }
}

/// What stands between an agent's environment file and deleting it, which is what lets an agent be
/// given the folder it is in.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentFile {
    /// What still reads values from it. With none, the file is left from before they were moved, and
    /// deleting it is all that remains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readers: Vec<Reader>,
    /// What the manifests beside it take from agentctl instead of the file. Deleting the file loses
    /// the values of those the person has not stored yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stored: Vec<Stored>,
}

/// Something that still reads values from an environment file.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum Reader {
    /// A manifest beside the file, by its file name.
    Manifest { name: String, variables: Vec<Variable> },
    /// A manifest beside the file that agentctl cannot read, so it may read any value.
    Unreadable { name: String },
    /// An agent applied with the file, as `agent/NAME`. Its manifest may have stopped reading the
    /// file since, such as through a pull, but the agent reads it until it is applied again.
    Agent { name: String, variables: Vec<Variable> },
}

/// A value an agent reads from an environment file.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Variable {
    /// The variable the agent gets, which names its entry in the manifest.
    pub name: String,
    /// The variable in the file, when its entry names one with `source`. It is the agent's variable
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The hosts its value may reach when it is a secret, which may be none. A plain value, which
    /// the agent sees as it is, has no such list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_hosts: Option<Vec<String>>,
}

/// Something a manifest takes from what agentctl stores, in place of an environment file.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum Stored {
    /// The GitHub login of an entry in `spec.accounts`.
    GithubLogin { login: String },
    /// The stored secret an entry in `spec.secrets` names, with the hosts the entry narrows it to.
    Secret {
        name: String,
        allowed_hosts: Vec<String>,
        /// Whether the agent starts without it, so that a person who has no such key need not
        /// store one.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        optional: bool,
    },
}

/// An environment file an agent was applied with, and what the agent reads from it.
#[derive(Clone)]
pub(crate) struct Applied {
    /// The file, as a canonical path.
    pub(crate) file: PathBuf,
    /// The agent, as `agent/NAME`.
    pub(crate) agent: String,
    pub(crate) variables: Vec<Variable>,
}

/// The values `spec` reads from an environment file: those of `spec.environment` without a `value`
/// and of `spec.secrets` without a `name`.
pub(crate) fn variables(spec: &crate::Spec) -> Vec<Variable> {
    let plain = spec.environment.iter().filter_map(|entry| {
        entry.source()?;
        Some(Variable {
            name: entry.name.clone(),
            source: entry.source.clone(),
            allowed_hosts: None,
        })
    });
    let secret = spec.secrets.iter().filter_map(|entry| {
        entry.source()?;
        Some(Variable {
            name: entry.environment.clone(),
            source: entry.source.clone(),
            allowed_hosts: Some(entry.allowed_hosts.clone()),
        })
    });
    plain.chain(secret).collect()
}

/// `items` as a sentence lists them: `a`, `a and b`, or `a, b and c`.
#[must_use]
pub fn sentence_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Whether `name` is the name of a file that may hold real keys.
pub(crate) fn is_exposed_name(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().eq_ignore_ascii_case(NAME.as_bytes())
}

/// The files that may hold real keys in each of `folders`, a canonical host folder and where an
/// agent sees it, if it does. `environment_files` are the environment files agents were applied
/// with, which are found under any name.
///
/// Every file in a folder counts, also one Git ignores, since that's where such files usually are.
/// Symbolic links are not followed, but a link with the name counts. A folder that no longer
/// exists, as an optional one may not, holds nothing.
///
/// # Errors
///
/// Returns an error when a folder cannot be read.
pub(crate) async fn find(
    folders: Vec<(PathBuf, Option<SandboxPath>)>,
    environment_files: Vec<Applied>,
) -> Result<Vec<ExposedFile>, Error> {
    blocking::run("looking for .env files", move || {
        let mut found = BTreeSet::new();
        for (folder, target) in folders {
            if matches!(folder.try_exists(), Ok(false)) {
                continue;
            }
            let mut named = Vec::new();
            for entry in WalkBuilder::new(&folder)
                .hidden(false)
                .ignore(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .parents(false)
                .follow_links(false)
                .build()
            {
                blocking::check()?;
                let entry = entry.map_err(|error| {
                    Error::Invalid(format!("cannot look for .env files in {}: {error}", folder.display()))
                })?;
                if !entry.file_type().is_some_and(|kind| kind.is_dir()) && is_exposed_name(entry.file_name()) {
                    named.push(entry.into_path());
                }
            }
            found.extend(classify(&folder, target.as_ref(), named, &environment_files));
        }
        Ok(found.into_iter().collect())
    })
    .await?
}

/// The files `named` in the canonical `folder`, which an agent sees at `target`, if it does, together
/// with the `environment_files` of agents inside it, telling which are environment files and what
/// is left to do about those.
///
/// Reads the filesystem, so it blocks.
pub(crate) fn classify(
    folder: &Path,
    target: Option<&SandboxPath>,
    named: Vec<PathBuf>,
    environment_files: &[Applied],
) -> BTreeSet<ExposedFile> {
    let recorded = environment_files
        .iter()
        .map(|applied| &applied.file)
        .filter(|file| file.starts_with(folder))
        .cloned();
    named
        .into_iter()
        .chain(recorded)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| ExposedFile {
            environment_file: environment_file(&path, environment_files),
            folder: folder.to_path_buf(),
            target: target.cloned(),
            path,
        })
        .collect()
}

/// What is left to do about `path` when it is an environment file: one of the `environment_files`
/// agents were applied with, or a file beside an agent manifest.
fn environment_file(path: &Path, environment_files: &[Applied]) -> Option<EnvironmentFile> {
    let reads = |manifest: &Path| crate::manifest::resolve(manifest).map(|resolved| resolved.agent.spec);
    // An agent records the file a link leads to, so its readers are those of that file.
    let resolved = common::fs::canonicalize(path).ok();
    let mut readers = environment_files
        .iter()
        .filter(|applied| applied.file == path || Some(&applied.file) == resolved.as_ref())
        .map(|applied| Reader::Agent {
            name: applied.agent.clone(),
            variables: applied.variables.clone(),
        })
        .collect::<Vec<_>>();
    let manifests = manifests_beside(path);
    if readers.is_empty() && manifests.is_empty() {
        return None;
    }
    let mut stored = BTreeSet::new();
    // Only the `.env` beside a manifest is read without being named.
    if path.file_name().is_some_and(is_exposed_name) {
        for manifest in &manifests {
            let name = manifest
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match reads(manifest) {
                Ok(spec) => {
                    let variables = variables(&spec);
                    if !variables.is_empty() {
                        readers.push(Reader::Manifest { name, variables });
                    }
                    stored.extend(spec.accounts.iter().map(|account| Stored::GithubLogin {
                        login: account.login().to_owned(),
                    }));
                    stored.extend(spec.secrets.iter().filter_map(|secret| {
                        Some(Stored::Secret {
                            name: secret.name.clone()?,
                            allowed_hosts: secret.allowed_hosts.clone(),
                            optional: secret.optional,
                        })
                    }));
                }
                Err(_) => readers.push(Reader::Unreadable { name }),
            }
        }
    }
    Some(EnvironmentFile {
        readers,
        stored: stored.into_iter().collect(),
    })
}

/// The agent manifests in the folder of `path`, by name.
fn manifests_beside(path: &Path) -> Vec<PathBuf> {
    let mut manifests = path
        .parent()
        .and_then(|parent| std::fs::read_dir(parent).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| crate::manifest::is_manifest_filename(path))
        .collect::<Vec<_>>();
    manifests.sort();
    manifests
}

/// The paths of the `found` files, once the person has agreed to every one of them: each is in
/// `allowed`, which the request names or an earlier one did, and none is an environment file.
///
/// # Errors
///
/// Returns [`Error::ExposedFiles`] with every file `found` when any is not allowed.
pub(crate) fn consented(found: Vec<ExposedFile>, allowed: &BTreeSet<PathBuf>) -> Result<BTreeSet<PathBuf>, Error> {
    if found
        .iter()
        .any(|file| file.environment_file.is_some() || !allowed.contains(&file.path))
    {
        return Err(Error::ExposedFiles(found));
    }
    Ok(found.into_iter().map(|file| file.path).collect())
}

/// Why agentd refuses `files`: an agent's environment file, which no consent covers, with what to do
/// about it, or else files the request did not name, for a client that doesn't ask about them.
#[must_use]
pub fn refusal(files: &[ExposedFile]) -> String {
    refusal_from(files, None)
}

/// [`refusal`] for a person working in the directory `from`, who knows the files by their paths from
/// there.
///
/// An environment file that something reads is moved off by `agentctl update`, which stores its
/// values and has the agent builder change the manifests, so the refusal sends the person there
/// rather than through the steps. One that nothing reads any more only needs deleting, once what
/// its manifests take from agentctl is stored.
#[must_use]
pub fn refusal_from(files: &[ExposedFile], from: Option<&Path>) -> String {
    // A file in several of the folders is one file to the person.
    let mut seen = BTreeSet::new();
    let (read, unread): (Vec<_>, Vec<_>) = files
        .iter()
        .filter(|file| seen.insert(&file.path))
        .filter_map(|file| Some((file.shown(from), file.environment_file.as_ref()?)))
        .partition(|(_, file)| !file.readers.is_empty());
    // Each file with what reads it, which is mostly the manifests beside it, but may be an agent
    // applied with it from elsewhere.
    let list = |files: &[(String, &EnvironmentFile)]| {
        files
            .iter()
            .map(|(path, file)| {
                let readers = file
                    .readers
                    .iter()
                    .map(|reader| match reader {
                        Reader::Manifest { name, .. }
                        | Reader::Unreadable { name, .. }
                        | Reader::Agent { name, .. } => name.clone(),
                    })
                    .collect::<Vec<_>>();
                if readers.is_empty() {
                    format!("  {path}")
                } else {
                    format!("  {path}, read by {}", sentence_list(&readers))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut blocks = Vec::new();
    if !read.is_empty() {
        blocks.push(format!(
            "agents read their keys from these environment files, so no agent may be given a folder that holds \
             one:\n{}\n{MOVING_OFF}",
            list(&read)
        ));
    }
    if !unread.is_empty() {
        let mut lines = vec![format!("{}:", UNREAD.to_ascii_lowercase()), list(&unread)];
        lines.extend(EnvironmentFile::stored_lines_of(unread.iter().map(|(_, file)| *file)));
        lines.push(
            "Delete them once you have, then run this again. `agentctl update` in their checkout stores what the \
             manifests take from the files for you and deletes them."
                .into(),
        );
        blocks.push(lines.join("\n"));
    }
    if !blocks.is_empty() {
        return blocks.join("\n\n");
    }
    let paths = files
        .iter()
        .map(|file| file.path.display().to_string())
        .collect::<Vec<_>>();
    format!(
        "the agent would be able to read {}, which may hold real keys; agree by naming them in allowExposedFiles",
        sentence_list(&paths)
    )
}

/// What gets an environment file that agents read out of the way: `agentctl update` stores its
/// values and has the agent builder change the manifests that read it.
pub const MOVING_OFF: &str = "`agentctl update`, run in the checkout or the folder of agents the manifests are in, \
                              moves the keys into agentctl's store and the manifests off the file.";

/// What agentctl knows of environment files that nothing it can see reads any more. A manifest
/// elsewhere may still name one, which is why deleting them is the person's to agree to.
pub const UNREAD: &str = "No manifest beside these environment files and no agent on this machine reads them any \
                          more, but they still hold keys, so no agent may be given a folder that holds one";

impl EnvironmentFile {
    /// What the manifests beside the `files` take from agentctl instead, with the command that
    /// stores each, for a person about to delete them. Nothing when they take nothing.
    #[must_use]
    pub fn stored_lines_of<'a>(files: impl IntoIterator<Item = &'a Self>) -> Vec<String> {
        let mut logins = BTreeSet::new();
        // Storing a secret sets its hosts, so one command names those of every entry that takes it.
        let mut hosts = std::collections::BTreeMap::<&str, Vec<String>>::new();
        for stored in files.into_iter().flat_map(|file| &file.stored) {
            match stored {
                Stored::GithubLogin { login } => {
                    logins.insert(login);
                }
                Stored::Secret {
                    name, allowed_hosts, ..
                } => {
                    let hosts = hosts.entry(name).or_default();
                    for host in allowed_hosts {
                        if !hosts.contains(host) {
                            hosts.push(host.clone());
                        }
                    }
                }
            }
        }
        let logins = logins.into_iter().map(|login| {
            (
                format!("the GitHub login {login}"),
                format!("agentctl github login {login}"),
            )
        });
        let secrets = hosts
            .iter()
            .map(|(name, hosts)| (format!("the secret {name}"), secret_set(name, hosts)));
        let stored = logins.chain(secrets).collect::<Vec<_>>();
        if stored.is_empty() {
            return Vec::new();
        }
        let width = stored.iter().map(|(what, _)| what.len()).max().unwrap_or_default();
        let mut lines = vec!["The manifests there now take these from agentctl. Store those you have not:".into()];
        lines.extend(
            stored
                .iter()
                .map(|(what, command)| format!("  {what:width$}  {command}")),
        );
        lines
    }
}

/// The command that stores the secret `name` for `allowed_hosts`, which the person fills in when the
/// manifest names none.
#[must_use]
pub fn secret_set(name: &str, allowed_hosts: &[String]) -> String {
    let hosts = if allowed_hosts.is_empty() {
        "HOST".to_owned()
    } else {
        allowed_hosts.join(",")
    };
    format!("agentctl secret set {name} --hosts {hosts}")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn found(folder: &Path, environment_files: Vec<Applied>) -> Vec<ExposedFile> {
        let folder = common::fs::canonicalize(folder).expect("folder");
        find(vec![(folder, None)], environment_files).await.expect("found")
    }

    fn relative(files: &[ExposedFile]) -> Vec<(&Path, bool)> {
        files
            .iter()
            .map(|file| (file.relative(), file.environment_file.is_some()))
            .collect()
    }

    fn variable(name: &str, allowed_hosts: Option<&[&str]>) -> Variable {
        Variable {
            name: name.into(),
            source: None,
            allowed_hosts: allowed_hosts.map(|hosts| hosts.iter().map(|&host| host.to_owned()).collect()),
        }
    }

    /// A manifest for an agent named `dev`, with `spec` as further lines of its `spec`.
    fn manifest(spec: &str) -> String {
        format!(
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata:\n  name: dev\nspec:\n  sandbox:\n    \
             image:\n      type: reference\n      reference: example.com/dev:1\n    platform:\n      os: linux\n    \
             resources:\n      cpu: \"1\"\n      memory: 1Gi\n      rootFilesystem:\n        capacity: 8Gi\n        \
             mode: layered\n{spec}"
        )
    }

    #[tokio::test]
    async fn every_env_file_is_found_and_those_agents_read_are_environment_files() {
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path();
        for folder in ["src/web", "tools", "agents/reviewer", "ignored", ".env"] {
            std::fs::create_dir_all(root.join(folder)).expect("folder");
        }
        std::fs::write(root.join(".gitignore"), "ignored/\n").expect("ignore");
        std::fs::write(root.join("src/web/.env"), "KEY=1").expect("file");
        std::fs::write(root.join("tools/.ENV"), "KEY=1").expect("file");
        std::fs::write(root.join("ignored/.env"), "KEY=1").expect("file");
        std::fs::write(root.join("agents/reviewer/agent.yaml"), "").expect("manifest");
        std::fs::write(root.join("agents/reviewer/.env"), "KEY=1").expect("file");
        std::fs::write(root.join("tools/beta.env"), "KEY=1").expect("file");
        let canonical = common::fs::canonicalize(root).expect("root");

        let reviewer = Applied {
            file: canonical.join("tools").join("beta.env"),
            agent: "agent/reviewer".into(),
            variables: vec![variable("KEY", None)],
        };
        let files = found(root, vec![reviewer]).await;

        assert_eq!(
            relative(&files),
            [
                (Path::new("agents/reviewer/.env"), true),
                (Path::new("ignored/.env"), false),
                (Path::new("src/web/.env"), false),
                (Path::new("tools/.ENV"), false),
                (Path::new("tools/beta.env"), true),
            ]
        );
    }

    #[tokio::test]
    async fn an_environment_file_names_what_still_reads_it_until_its_values_are_moved() {
        let directory = tempfile::tempdir().expect("directory");
        let root = common::fs::canonicalize(directory.path()).expect("root");
        let moved = "  environment:\n    - name: EDITOR\n      value: vim\n  accounts:\n    - type: github\n      \
                     login: work\n  secrets:\n    - name: aivar\n      environment: AIVAR_API_KEY\n      \
                     allowedHosts: [api.aivar.no]\n";
        let definitions = [
            ("reads", manifest("  environment:\n    - name: EDITOR\n")),
            ("moved", manifest(moved)),
            ("applied", manifest("")),
            ("unreadable", "kind: Agent\nspec:\n  mounts: []\n".to_owned()),
        ];
        for (folder, manifest) in definitions {
            std::fs::create_dir_all(root.join(folder)).expect("folder");
            std::fs::write(root.join(folder).join("agent.yaml"), manifest).expect("manifest");
            std::fs::write(root.join(folder).join(".env"), "EDITOR=vim").expect("file");
        }
        std::fs::write(
            root.join("reads/agent.nested.yaml"),
            "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata:\n  name: nested\n",
        )
        .expect("variant");
        let editor = vec![variable("EDITOR", None)];
        let token = vec![variable("GITHUB_TOKEN", Some(&["api.github.com"]))];
        // Applied before its manifest stopped reading the file, and not again since.
        let applied = Applied {
            file: root.join("applied/.env"),
            agent: "agent/dev".into(),
            variables: token.clone(),
        };

        let files = found(&root, vec![applied]).await;

        let left = files
            .iter()
            .map(|file| {
                (
                    file.relative(),
                    file.environment_file.clone().expect("beside a manifest"),
                )
            })
            .collect::<Vec<_>>();
        let manifest = |name: &str| Reader::Manifest {
            name: name.into(),
            variables: editor.clone(),
        };
        assert_eq!(
            left,
            [
                (
                    Path::new("applied/.env"),
                    EnvironmentFile {
                        readers: vec![Reader::Agent {
                            name: "agent/dev".into(),
                            variables: token,
                        }],
                        stored: Vec::new(),
                    }
                ),
                (
                    Path::new("moved/.env"),
                    EnvironmentFile {
                        readers: Vec::new(),
                        stored: vec![
                            Stored::GithubLogin { login: "work".into() },
                            Stored::Secret {
                                name: "aivar".into(),
                                allowed_hosts: vec!["api.aivar.no".into()],
                                optional: false,
                            },
                        ],
                    }
                ),
                (
                    Path::new("reads/.env"),
                    EnvironmentFile {
                        readers: vec![manifest("agent.nested.yaml"), manifest("agent.yaml")],
                        stored: Vec::new(),
                    }
                ),
                (
                    Path::new("unreadable/.env"),
                    EnvironmentFile {
                        readers: vec![Reader::Unreadable {
                            name: "agent.yaml".into(),
                        }],
                        stored: Vec::new(),
                    }
                ),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_agent_reads_the_environment_file_a_link_leads_to() {
        let directory = tempfile::tempdir().expect("directory");
        let root = common::fs::canonicalize(directory.path()).expect("root");
        std::fs::create_dir_all(root.join("checkout")).expect("folder");
        std::fs::write(root.join("checkout/agent.yaml"), manifest("")).expect("manifest");
        std::fs::write(root.join("secrets.env"), "KEY=1").expect("file");
        std::os::unix::fs::symlink(root.join("secrets.env"), root.join("checkout/.env")).expect("link");
        let applied = Applied {
            file: root.join("secrets.env"),
            agent: "agent/dev".into(),
            variables: vec![variable("KEY", None)],
        };

        let files = found(&root.join("checkout"), vec![applied]).await;

        let readers = &files[0].environment_file.as_ref().expect("beside a manifest").readers;
        assert!(
            matches!(readers.as_slice(), [Reader::Agent { name, .. }] if name == "agent/dev"),
            "{readers:?}"
        );
    }

    /// An environment file in a checkout, with what agentd found about it.
    #[test]
    fn the_refusal_names_each_environment_file_once_and_what_gets_it_out_of_the_way() {
        let at = |folder: &str, relative: &str, readers: Vec<Reader>, stored: Vec<Stored>| ExposedFile {
            path: Path::new(folder).join(relative),
            folder: PathBuf::from(folder),
            target: None,
            environment_file: Some(EnvironmentFile { readers, stored }),
        };
        let read = vec![Reader::Manifest {
            name: "agent.yaml".into(),
            variables: vec![variable("API_KEY", Some(&["api.example.com"]))],
        }];
        let login = vec![Stored::GithubLogin { login: "work".into() }];
        let files = [
            at("/src/app", "agents/dev/.env", read.clone(), Vec::new()),
            // The builder is given the checkout and the folder of personal agents, which may hold it.
            at("/src/app/agents", "dev/.env", read, Vec::new()),
            at("/src/app", "agents/old/.env", Vec::new(), login),
        ];

        let refused = refusal_from(&files, Some(Path::new("/src/app")));

        assert_eq!(refused.matches("agents/dev/.env").count(), 1, "{refused}");
        assert!(refused.contains(MOVING_OFF), "{refused}");
        assert!(
            refused.contains("`agentctl update` in their checkout stores what the manifests take"),
            "a file nothing reads goes there too: {refused}"
        );
        for expected in ["agents/old/.env", "the GitHub login work  agentctl github login work"] {
            assert!(
                refused.lines().any(|line| line.trim() == expected),
                "{expected:?} is missing from:\n{refused}"
            );
        }
        assert_eq!(
            refusal_from(&files[..1], None).lines().nth(1).map(str::trim),
            Some("agents/dev/.env in /src/app, read by agent.yaml"),
            "within its folder for a reader elsewhere"
        );
    }

    #[tokio::test]
    async fn consent_covers_the_files_it_names_and_never_an_environment_file() {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::create_dir_all(directory.path().join("web")).expect("folder");
        std::fs::write(directory.path().join("web/.env"), "KEY=1").expect("file");
        let files = found(directory.path(), Vec::new()).await;
        let named = files.iter().map(|file| file.path.clone()).collect::<BTreeSet<_>>();

        assert!(matches!(
            consented(files.clone(), &BTreeSet::new()),
            Err(Error::ExposedFiles(refused)) if refused == files
        ));
        assert_eq!(consented(files.clone(), &named).expect("consented"), named);

        let environment = files
            .into_iter()
            .map(|file| ExposedFile {
                environment_file: Some(EnvironmentFile::default()),
                ..file
            })
            .collect::<Vec<_>>();
        assert!(consented(environment, &named).is_err());
    }
}
