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
    /// A manifest beside the file that this version cannot read, so it may read any value.
    Unreadable { name: String },
    /// An agent applied with the file, as `agent/NAME`.
    Agent {
        name: String,
        variables: Vec<Variable>,
        /// Whether its manifest has stopped reading the file since, so applying it again is all it
        /// takes.
        reapply: bool,
    },
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
    Secret { name: String, allowed_hosts: Vec<String> },
}

/// An environment file an agent was applied with, and what the agent reads from it.
#[derive(Clone)]
pub(crate) struct Applied {
    /// The file, as a canonical path.
    pub(crate) file: PathBuf,
    /// The agent, as `agent/NAME`.
    pub(crate) agent: String,
    pub(crate) variables: Vec<Variable>,
    /// The manifest it was applied from, when agentd knows it.
    pub(crate) manifest: Option<PathBuf>,
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
    let resolved = std::fs::canonicalize(path).ok();
    let mut readers = environment_files
        .iter()
        .filter(|applied| applied.file == path || Some(&applied.file) == resolved.as_ref())
        .map(|applied| Reader::Agent {
            name: applied.agent.clone(),
            variables: applied.variables.clone(),
            reapply: applied
                .manifest
                .as_deref()
                .and_then(|manifest| reads(manifest).ok())
                .is_some_and(|spec| variables(&spec).is_empty()),
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
#[must_use]
pub fn refusal_from(files: &[ExposedFile], from: Option<&Path>) -> String {
    // A file in several of the folders is one file to the person.
    let mut seen = BTreeSet::new();
    let environment = files
        .iter()
        .filter(|file| seen.insert(&file.path))
        .filter_map(|file| {
            Some(environment_file_refusal(
                &file.shown(from),
                file.environment_file.as_ref()?,
            ))
        })
        .collect::<Vec<_>>();
    if !environment.is_empty() {
        return environment.join("\n\n");
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

/// What agentctl knows of an environment file that nothing it can see reads any more. A manifest
/// elsewhere may still name the file, which is why deleting it is the person's to agree to.
pub const UNREAD: &str = "No manifest beside this environment file and no agent on this machine reads it any more, \
                          but it still holds their values, so no agent may be given a folder it is in";

/// Why no agent may be given a folder with the environment file `path`, and every step that gets
/// rid of the file: what reads it, where each value goes instead, and what to run again.
fn environment_file_refusal(path: &str, file: &EnvironmentFile) -> String {
    let mut lines = Vec::new();
    if file.readers.is_empty() {
        lines.extend([format!("{}:", UNREAD.to_ascii_lowercase()), format!("  {path}")]);
        let stored = file.stored_lines();
        if !stored.is_empty() {
            lines.push(String::new());
            lines.extend(stored);
        }
        lines.extend([String::new(), "Delete the file, then run this again.".into()]);
        return lines.join("\n");
    }
    lines.extend([
        "an environment file still gives agents their values, so no agent may be given a folder it is in:".into(),
        format!("  {path}"),
        String::new(),
        "It is read by:".into(),
    ]);
    // The manifests of a folder mostly read the same, a variant what its base does.
    let mut manifests = Vec::<(&[Variable], Vec<String>)>::new();
    let mut others = Vec::new();
    let mut agents = Vec::new();
    let mut moving = Vec::<Variable>::new();
    for reader in &file.readers {
        let variables = match reader {
            Reader::Manifest { name, variables } => {
                match manifests.iter_mut().find(|(read, _)| read == variables) {
                    Some((_, names)) => names.push(name.clone()),
                    None => manifests.push((variables, vec![name.clone()])),
                }
                variables.as_slice()
            }
            Reader::Unreadable { name } => {
                others.push(format!(
                    "  {name}: this version cannot read it, so it may read any value"
                ));
                &[]
            }
            Reader::Agent {
                name,
                variables,
                reapply,
            } => {
                agents.push(name.clone());
                let read = names(variables);
                if *reapply {
                    others.push(format!(
                        "  {name}: {read}. Its manifest no longer reads the file, so apply it again"
                    ));
                    &[]
                } else {
                    others.push(format!("  {name}: {read}"));
                    variables.as_slice()
                }
            }
        };
        for variable in variables {
            if !moving.contains(variable) {
                moving.push(variable.clone());
            }
        }
    }
    lines.extend(
        manifests
            .into_iter()
            .map(|(variables, files)| format!("  {}: {}", sentence_list(&files), names(variables))),
    );
    lines.extend(others);
    lines.extend([String::new(), moving_out(&moving, &agents)]);
    lines.join("\n")
}

/// The steps that take `variables` out of an environment file, so that it can be deleted: where each
/// goes instead, then applying the `agents` that read it again.
pub(crate) fn moving_out(variables: &[Variable], agents: &[String]) -> String {
    let mut lines = Vec::new();
    if !variables.is_empty() {
        lines.push("Move each value out of the file:".into());
        lines.extend(moves(variables));
        lines.push(String::new());
    }
    let apply = if agents.is_empty() {
        String::new()
    } else {
        format!("apply {} again, ", sentence_list(agents))
    };
    lines.push(format!("Then {apply}delete the file and run this again."));
    lines.join("\n")
}

impl EnvironmentFile {
    /// What the manifests beside the file take from agentctl instead, with the command that stores
    /// each, for a person about to delete the file. Nothing when they take nothing.
    #[must_use]
    pub fn stored_lines(&self) -> Vec<String> {
        if self.stored.is_empty() {
            return Vec::new();
        }
        let stored = self
            .stored
            .iter()
            .map(|stored| match stored {
                Stored::GithubLogin { login } => (
                    format!("the GitHub login {login}"),
                    format!("agentctl github login {login}"),
                ),
                Stored::Secret { name, allowed_hosts } => {
                    (format!("the secret {name}"), secret_set(name, allowed_hosts))
                }
            })
            .collect::<Vec<_>>();
        let width = stored.iter().map(|(what, _)| what.len()).max().unwrap_or_default();
        let mut lines = vec!["The manifests beside it now take these from agentctl. Store those you have not:".into()];
        lines.extend(
            stored
                .iter()
                .map(|(what, command)| format!("  {what:width$}  {command}")),
        );
        lines
    }
}

/// The `variables` as a person reads them in a list: each by the name the agent gets, with the
/// name the file has for it when that differs.
fn names(variables: &[Variable]) -> String {
    variables
        .iter()
        .map(|variable| match &variable.source {
            Some(source) if *source != variable.name => format!("{} ({source} in the file)", variable.name),
            _ => variable.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The command that stores the secret `name` for `allowed_hosts`, which the person fills in when the
/// manifest names none.
fn secret_set(name: &str, allowed_hosts: &[String]) -> String {
    let hosts = if allowed_hosts.is_empty() {
        "HOST".to_owned()
    } else {
        allowed_hosts.join(",")
    };
    format!("agentctl secret set {name} --hosts {hosts}")
}

/// Where each of `variables` goes instead of the environment file: the command that stores it, and
/// the change to its entry in the manifest.
///
/// A GitHub token becomes a GitHub login, whose account is also Git's name and email. Another
/// secret is stored under a name made from its variable's. A plain value goes in the manifest.
fn moves(variables: &[Variable]) -> Vec<String> {
    const GIT_IDENTITY: [&str; 2] = ["GIT_USER_NAME", "GIT_USER_EMAIL"];
    let is_token =
        |variable: &Variable| variable.allowed_hosts.is_some() && variable.name == crate::github::ACCESS_ENVIRONMENT;
    let (github, others): (Vec<_>, Vec<_>) = if variables.iter().any(is_token) {
        variables.iter().partition(|variable| {
            is_token(variable) || (variable.allowed_hosts.is_none() && GIT_IDENTITY.contains(&variable.name.as_str()))
        })
    } else {
        (Vec::new(), variables.iter().collect())
    };
    let mut lines = Vec::new();
    if !github.is_empty() {
        let entries = if github.len() == 1 {
            "its entry"
        } else {
            "their entries"
        };
        lines.extend([
            format!("  {}", names(&github.iter().copied().cloned().collect::<Vec<_>>())),
            "    Sign in:   agentctl github login default".into(),
            format!("    Manifest:  replace {entries} with this, under spec:"),
            "                 accounts:".into(),
            "                   - type: github".into(),
            "                     login: default".into(),
        ]);
    }
    let mut named = Vec::<(String, String)>::new();
    for variable in others {
        lines.push(format!("  {}", names(std::slice::from_ref(variable))));
        // An entry takes its value from the file's variable or from elsewhere, never both.
        let change = |field: &str| {
            variable.source.as_ref().map_or_else(
                || format!("add {field} to"),
                |source| format!("replace `source: {source}` with {field} in"),
            )
        };
        if let Some(allowed_hosts) = &variable.allowed_hosts {
            // Named after the file's variable, which is what tells one credential from another.
            let source = variable.source.as_ref().unwrap_or(&variable.name);
            let name = source.to_ascii_lowercase().replace('_', "-");
            let name = if crate::names::valid(&name) {
                name
            } else {
                "NAME".into()
            };
            // Two of the file's variables may differ only in what a name leaves out, such as case.
            let taken = |name: &str| named.iter().any(|(other, taken)| taken == name && other != source);
            let name = std::iter::once(name.clone())
                .chain((2..named.len() + 2).map(|number| format!("{name}-{number}")))
                .find(|name| !taken(name))
                .unwrap_or(name);
            named.push((source.clone(), name.clone()));
            lines.extend([
                format!("    Store it:  {}", secret_set(&name, allowed_hosts)),
                format!(
                    "    Manifest:  {} its entry in spec.secrets",
                    change(&format!("`name: {name}`"))
                ),
            ]);
        } else {
            lines.extend([
                format!(
                    "    Manifest:  {} its entry in spec.environment.",
                    change("`value:` with its value")
                ),
                "               A value of your own goes in a variant Git ignores, such as agent.mine.yaml".into(),
            ]);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn found(folder: &Path, environment_files: Vec<Applied>) -> Vec<ExposedFile> {
        let folder = std::fs::canonicalize(folder).expect("folder");
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
        let canonical = std::fs::canonicalize(root).expect("root");

        let reviewer = Applied {
            file: canonical.join("tools").join("beta.env"),
            agent: "agent/reviewer".into(),
            variables: vec![variable("KEY", None)],
            manifest: None,
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
        let root = std::fs::canonicalize(directory.path()).expect("root");
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
            manifest: Some(root.join("applied/agent.yaml")),
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
                            reapply: true
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
                                allowed_hosts: vec!["api.aivar.no".into()]
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
                            name: "agent.yaml".into()
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
        let root = std::fs::canonicalize(directory.path()).expect("root");
        std::fs::create_dir_all(root.join("checkout")).expect("folder");
        std::fs::write(root.join("checkout/agent.yaml"), manifest("")).expect("manifest");
        std::fs::write(root.join("secrets.env"), "KEY=1").expect("file");
        std::os::unix::fs::symlink(root.join("secrets.env"), root.join("checkout/.env")).expect("link");
        let applied = Applied {
            file: root.join("secrets.env"),
            agent: "agent/dev".into(),
            variables: vec![variable("KEY", None)],
            manifest: None,
        };

        let files = found(&root.join("checkout"), vec![applied]).await;

        let readers = &files[0].environment_file.as_ref().expect("beside a manifest").readers;
        assert!(
            matches!(readers.as_slice(), [Reader::Agent { name, .. }] if name == "agent/dev"),
            "{readers:?}"
        );
    }

    #[test]
    fn the_refusal_of_an_environment_file_says_where_each_value_goes_instead() {
        let file = |readers, stored| ExposedFile {
            path: PathBuf::from("/src/app/agents/dev/.env"),
            folder: PathBuf::from("/src/app"),
            target: None,
            environment_file: Some(EnvironmentFile { readers, stored }),
        };
        let variables = vec![
            variable("GIT_USER_NAME", None),
            variable("GIT_USER_EMAIL", None),
            variable("EDITOR", None),
            variable("GITHUB_TOKEN", Some(&["api.github.com"])),
            variable("AIVAR_API_KEY", Some(&["api.aivar.no"])),
        ];
        let manifest = |name: &str| Reader::Manifest {
            name: name.into(),
            variables: variables.clone(),
        };
        let agent = Reader::Agent {
            name: "agent/dev".into(),
            variables: variables[3..4].to_vec(),
            reapply: true,
        };
        let read = file(
            vec![manifest("agent.nested.yaml"), manifest("agent.yaml"), agent],
            Vec::new(),
        );

        let refused = refusal_from(std::slice::from_ref(&read), Some(Path::new("/src/app")));
        let lines = refused.lines().map(str::trim).collect::<Vec<_>>();

        assert_eq!(
            lines[1], "agents/dev/.env",
            "named from the directory, on a line of its own"
        );
        for expected in [
            "agent.nested.yaml and agent.yaml: GIT_USER_NAME, GIT_USER_EMAIL, EDITOR, GITHUB_TOKEN, AIVAR_API_KEY",
            // Git's name and email come with the GitHub login, so they go with the token.
            "GIT_USER_NAME, GIT_USER_EMAIL, GITHUB_TOKEN",
            "Sign in:   agentctl github login default",
            "Store it:  agentctl secret set aivar-api-key --hosts api.aivar.no",
            "Manifest:  add `name: aivar-api-key` to its entry in spec.secrets",
            "Then apply agent/dev again, delete the file and run this again.",
        ] {
            assert!(lines.contains(&expected), "{expected:?} is missing from:\n{refused}");
        }
        assert!(!refused.contains("https://"), "it says what to do itself: {refused}");
        assert_eq!(
            refusal_from(&[read], None).lines().nth(1).map(str::trim),
            Some("agents/dev/.env in /src/app"),
            "within its folder for a reader elsewhere"
        );

        // The same variable from several of the file's, as manifests may have it, is as many
        // credentials, and an entry that names the file's variable stops naming it.
        let renamed = |manifest: &str, source: &str| Reader::Manifest {
            name: manifest.into(),
            variables: vec![Variable {
                source: Some(source.into()),
                ..variable("API_KEY", Some(&["api.example.com"]))
            }],
        };
        let read = file(
            vec![
                renamed("agent.dev.yaml", "DEV_KEY"),
                renamed("agent.other.yaml", "Dev_KEY"),
                renamed("agent.yaml", "API_KEY"),
            ],
            Vec::new(),
        );
        let refused = refusal(&[read]);
        for expected in [
            "Store it:  agentctl secret set dev-key --hosts api.example.com",
            "Store it:  agentctl secret set dev-key-2 --hosts api.example.com",
            "Manifest:  replace `source: Dev_KEY` with `name: dev-key-2` in its entry in spec.secrets",
            "Manifest:  replace `source: API_KEY` with `name: api-key` in its entry in spec.secrets",
        ] {
            assert!(
                refused.lines().any(|line| line.trim() == expected),
                "{expected:?} is missing from:\n{refused}"
            );
        }

        let left = file(Vec::new(), vec![Stored::GithubLogin { login: "work".into() }]);
        let refused = refusal(&[left]);
        assert!(refused.contains("agentctl github login work"), "{refused}");
        assert!(!refused.contains("Move each value"), "{refused}");
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
