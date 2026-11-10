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
    /// Whether it's an agent's environment file: the file an agent records, or a `.env` beside an
    /// agent manifest, which agentctl reads values from by default. It holds the agent's own secrets,
    /// which belong in stored secrets instead, so consent never covers it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub environment_file: bool,
}

impl ExposedFile {
    /// The file's path within its folder, as the person knows it in their checkout.
    #[must_use]
    pub fn relative(&self) -> &Path {
        self.path.strip_prefix(&self.folder).unwrap_or(&self.path)
    }
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
/// agent sees it, if it does. `environment_files` are the canonical environment files agents
/// record, which are found under any name.
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
    environment_files: Vec<PathBuf>,
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
/// with the `environment_files` of agents inside it, telling which are environment files.
///
/// Reads the filesystem, so it blocks.
pub(crate) fn classify(
    folder: &Path,
    target: Option<&SandboxPath>,
    named: Vec<PathBuf>,
    environment_files: &[PathBuf],
) -> BTreeSet<ExposedFile> {
    let recorded = environment_files
        .iter()
        .filter(|file| file.starts_with(folder))
        .cloned();
    named
        .into_iter()
        .chain(recorded)
        .map(|path| ExposedFile {
            environment_file: environment_files.contains(&path) || beside_manifest(&path),
            folder: folder.to_path_buf(),
            target: target.cloned(),
            path,
        })
        .collect()
}

/// Whether `path` is in a folder with an agent manifest, whose values agentctl reads from it by default.
fn beside_manifest(path: &Path) -> bool {
    path.parent()
        .and_then(|parent| std::fs::read_dir(parent).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| crate::manifest::is_manifest_filename(&entry.path()))
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
        .any(|file| file.environment_file || !allowed.contains(&file.path))
    {
        return Err(Error::ExposedFiles(found));
    }
    Ok(found.into_iter().map(|file| file.path).collect())
}

/// Why agentd refuses `files`: an agent's environment file, which no consent covers, or else files
/// the request did not name, for a client that doesn't ask about them.
#[must_use]
pub fn refusal(files: &[ExposedFile]) -> String {
    let environment = files
        .iter()
        .filter(|file| file.environment_file)
        .map(|file| file.path.display().to_string())
        .collect::<Vec<_>>();
    if !environment.is_empty() {
        return format!(
            "{} holds an agent's own secrets, so no agent may read it. Move its values as \
             {ENVIRONMENT_FILE_GUIDE} says, then delete it",
            sentence_list(&environment)
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn found(folder: &Path, environment_files: Vec<PathBuf>) -> Vec<ExposedFile> {
        let folder = std::fs::canonicalize(folder).expect("folder");
        find(vec![(folder, None)], environment_files).await.expect("found")
    }

    fn relative(files: &[ExposedFile]) -> Vec<(&Path, bool)> {
        files
            .iter()
            .map(|file| (file.relative(), file.environment_file))
            .collect()
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

        let files = found(root, vec![canonical.join("tools").join("beta.env")]).await;

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
                environment_file: true,
                ..file
            })
            .collect::<Vec<_>>();
        assert!(consented(environment, &named).is_err());
    }
}
