//! A team's agent definition on GitHub, in a checkout on this machine: one there already, or a
//! clone setup makes with the person's own `git`. The person's variant is written in it, beside
//! the team's manifest, as for any checkout.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use agent::Error;

/// A repository on GitHub, with the folder or manifest and the ref an agent definition is in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GithubSource {
    pub(super) owner: String,
    pub(super) repository: String,
    /// The folder, or the manifest file, within the repository.
    pub(super) subdirectory: Option<String>,
    /// A branch, a tag or a commit.
    pub(super) reference: Option<String>,
    /// What a `/tree/` or `/blob/` address has after it: a ref, then a path. A branch may hold
    /// slashes itself, so the two are told apart once the repository's refs are known.
    pub(super) ref_and_path: Option<String>,
}

impl GithubSource {
    /// Reads `https://github.com/OWNER/REPO[//SUBDIR][@REF]`, with or without the scheme, and the
    /// `/tree/REF/FOLDER` and `/blob/REF/FILE` addresses a browser shows, at any branch, tag or
    /// commit.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let rest = text
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.")
            .strip_prefix("github.com/")?;
        let (rest, reference) = match rest.rsplit_once('@') {
            Some((rest, reference)) if !reference.is_empty() && !reference.contains('/') => {
                (rest, Some(reference.to_owned()))
            }
            _ => (rest, None),
        };
        let (repository_path, subdirectory) = match rest.split_once("//") {
            Some((repository_path, subdirectory)) => (repository_path, Some(subdirectory.trim_matches('/').to_owned())),
            None => (rest, None),
        };
        let mut parts = repository_path.trim_matches('/').splitn(3, '/');
        let owner = parts.next().filter(|part| !part.is_empty())?.to_owned();
        let repository = parts
            .next()
            .filter(|part| !part.is_empty())?
            .trim_end_matches(".git")
            .to_owned();
        let ref_and_path = match (parts.next(), &subdirectory, &reference) {
            (Some(browsed), None, None) => {
                let ref_and_path = browsed
                    .strip_prefix("tree/")
                    .or_else(|| browsed.strip_prefix("blob/"))?;
                Some(ref_and_path.trim_matches('/').to_owned()).filter(|ref_and_path| !ref_and_path.is_empty())
            }
            (Some(_), ..) => return None,
            (None, ..) => None,
        };
        Some(Self {
            owner,
            repository,
            subdirectory: subdirectory.filter(|subdirectory| !subdirectory.is_empty()),
            reference,
            ref_and_path,
        })
    }

    pub(super) fn describe(&self) -> String {
        let mut text = format!("github.com/{}/{}", self.owner, self.repository);
        if let Some(ref_and_path) = &self.ref_and_path {
            text.push_str("/tree/");
            text.push_str(ref_and_path);
        }
        if let Some(subdirectory) = &self.subdirectory {
            text.push_str("//");
            text.push_str(subdirectory);
        }
        if let Some(reference) = &self.reference {
            text.push('@');
            text.push_str(reference);
        }
        text
    }

    /// The folder, or the manifest, the agent definition is in, in the checkout of the repository
    /// at `folder`. A checkout there already is used as it is, on whatever it has checked out.
    /// Otherwise the repository is cloned there, at the address's ref when it has one.
    pub(super) async fn checkout(&mut self, folder: &Path) -> Result<PathBuf, Error> {
        if folder.join(".git").exists() {
            self.check_origin(folder).await?;
            // The checkout stays on its own branch, so a `/tree/` or `/blob/` address's path is the
            // longest one it holds.
            if let Some(ref_and_path) = self.ref_and_path.take() {
                self.subdirectory = splits(&ref_and_path)
                    .into_iter()
                    .map(|(_, path)| path)
                    .find(|path| !path.is_empty() && folder.join(path).exists());
            }
            eprintln!("Using the checkout in {} as it is.", folder.display());
        } else if std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(Error::Invalid(format!(
                "{} holds files but no checkout of {}; choose another folder",
                folder.display(),
                self.describe()
            )));
        } else {
            if !git_available().await {
                return Err(Error::Invalid(format!(
                    "cloning {} needs git; clone it yourself and pass its folder to --from",
                    self.describe()
                )));
            }
            eprintln!("Cloning {} into {}.", self.describe(), folder.display());
            self.clone_with_git(folder).await?;
        }
        let found = self
            .subdirectory
            .as_ref()
            .map_or_else(|| folder.to_path_buf(), |subdirectory| folder.join(subdirectory));
        // Canonical paths, so that `..` in the address cannot lead out of the checkout.
        let inside = match (found.canonicalize(), folder.canonicalize()) {
            (Ok(found), Ok(folder)) => found.starts_with(folder),
            _ => false,
        };
        if !inside {
            return Err(Error::Invalid(format!(
                "{} has no such folder or file",
                self.describe()
            )));
        }
        Ok(found)
    }

    /// Refuses a checkout at `folder` of another repository than this one.
    async fn check_origin(&self, folder: &Path) -> Result<(), Error> {
        let output = tokio::process::Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(["remote", "get-url", "origin"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .map_err(|error| Error::Invalid(format!("could not run git: {error}")))?;
        let origin = String::from_utf8_lossy(&output.stdout).to_lowercase();
        let wanted = format!("{}/{}", self.owner, self.repository).to_lowercase();
        if origin.trim_end().trim_end_matches(".git").ends_with(&wanted) {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "{} is a checkout of another repository than {}; choose another folder",
                folder.display(),
                self.describe()
            )))
        }
    }

    /// Clones the repository with `git`, at its ref when it has one. A `/tree/` or `/blob/`
    /// address is split at the longest branch or tag it starts with that the clone finds.
    async fn clone_with_git(&mut self, checkout: &Path) -> Result<(), Error> {
        let candidates = match self.ref_and_path.take() {
            Some(ref_and_path) => splits(&ref_and_path)
                .into_iter()
                .rev()
                .map(|(reference, path)| (Some(reference), Some(path).filter(|path| !path.is_empty())))
                .collect(),
            None => vec![(self.reference.clone(), self.subdirectory.clone())],
        };
        for (reference, subdirectory) in candidates {
            let mut command = tokio::process::Command::new("git");
            command.args(["clone", "--quiet"]);
            if let Some(reference) = &reference {
                command.args(["--branch", reference]);
            }
            let status = command
                .arg(format!("https://github.com/{}/{}.git", self.owner, self.repository))
                .arg(checkout)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map_err(|error| Error::Invalid(format!("could not run git: {error}")))?;
            if status.success() {
                self.reference = reference;
                self.subdirectory = subdirectory;
                return Ok(());
            }
        }
        Err(Error::Invalid(format!(
            "git could not clone {}; check the address, the branch or tag, and that your git can read it",
            self.describe()
        )))
    }
}

/// Every way to split `ref_and_path` into a ref and a path, the shortest ref first.
fn splits(ref_and_path: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = ref_and_path.split('/').collect();
    (1..=parts.len())
        .map(|count| (parts[..count].join("/"), parts[count..].join("/")))
        .collect()
}

async fn git_available() -> bool {
    tokio::process::Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{GithubSource, splits};

    fn source(owner: &str, repository: &str, subdirectory: Option<&str>, reference: Option<&str>) -> GithubSource {
        GithubSource {
            owner: owner.into(),
            repository: repository.into(),
            subdirectory: subdirectory.map(Into::into),
            reference: reference.map(Into::into),
            ref_and_path: None,
        }
    }

    fn browsed(owner: &str, repository: &str, ref_and_path: &str) -> GithubSource {
        GithubSource {
            ref_and_path: Some(ref_and_path.into()),
            ..source(owner, repository, None, None)
        }
    }

    #[test]
    fn github_addresses_name_the_repository_folder_and_ref() {
        assert_eq!(
            GithubSource::parse("https://github.com/Altinn/altinn-studio//agents/minimal"),
            Some(source("Altinn", "altinn-studio", Some("agents/minimal"), None))
        );
        assert_eq!(
            GithubSource::parse("github.com/digdir/digdir-agents.git@v1"),
            Some(source("digdir", "digdir-agents", None, Some("v1")))
        );
        assert_eq!(
            GithubSource::parse("https://github.com/Altinn/altinn-studio/tree/main/agents/desktop"),
            Some(browsed("Altinn", "altinn-studio", "main/agents/desktop"))
        );
        assert_eq!(
            GithubSource::parse(
                "https://github.com/Altinn/altinn-studio/blob/feature/x/agents/minimal/agent.worktree.yaml"
            ),
            Some(browsed(
                "Altinn",
                "altinn-studio",
                "feature/x/agents/minimal/agent.worktree.yaml"
            ))
        );
        assert_eq!(
            GithubSource::parse("https://github.com/Altinn/altinn-studio/blob/main/agents/minimal/"),
            Some(browsed("Altinn", "altinn-studio", "main/agents/minimal"))
        );
        assert_eq!(
            GithubSource::parse("https://github.com/Altinn/altinn-studio/agents/minimal"),
            None
        );
        assert_eq!(GithubSource::parse("./agents/minimal"), None);
        assert_eq!(GithubSource::parse("https://gitlab.com/a/b"), None);
        assert_eq!(GithubSource::parse("https://github.com/only-owner"), None);
    }

    #[test]
    fn a_browsed_address_splits_into_every_ref_and_path_it_may_be() {
        assert_eq!(
            splits("feature/x/agents"),
            [
                ("feature".to_owned(), "x/agents".to_owned()),
                ("feature/x".to_owned(), "agents".to_owned()),
                ("feature/x/agents".to_owned(), String::new()),
            ]
        );
    }
}
