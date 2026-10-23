//! `repositories.yaml`: the GitHub repositories an agent folder's agents clone into their working
//! directory.
//!
//! The file sits beside the folder's manifests. Agent setup hands the list to the base image's
//! workspace unit on every pass, which clones each repository that is not there yet, so a
//! repository added later reaches a running agent without recreating it. Nothing is ever pulled,
//! reset or deleted: a checkout belongs to the agent once it exists.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// The file beside an agent's manifests that lists its repositories.
pub const REPOSITORIES_FILE: &str = "repositories.yaml";

/// One repository an agent clones.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// The GitHub repository, as `owner/name`.
    pub repo: String,
    /// Where it is cloned, relative to the working directory; its name when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// What the agent uses it for, in a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

impl Repository {
    /// Where the repository is cloned, relative to the working directory.
    #[must_use]
    pub fn destination(&self) -> &str {
        self.path
            .as_deref()
            .unwrap_or_else(|| self.repo.split_once('/').map_or(self.repo.as_str(), |(_, name)| name))
    }

    fn validate(&self) -> Result<(), Error> {
        let valid_name = |name: &str| {
            !name.is_empty()
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        let Some((owner, name)) = self.repo.split_once('/') else {
            return Err(Error::Invalid(format!(
                "{REPOSITORIES_FILE}: `{}` is not a GitHub repository; write it as owner/name",
                self.repo
            )));
        };
        if !crate::github::valid_owner(owner) || !valid_name(name) {
            return Err(Error::Invalid(format!(
                "{REPOSITORIES_FILE}: `{}` is not a GitHub repository; write it as owner/name",
                self.repo
            )));
        }
        if let Some(path) = &self.path
            && !path.split('/').all(valid_name)
        {
            return Err(Error::Invalid(format!(
                "{REPOSITORIES_FILE}: `{path}` must be a relative path below the working directory, such as forks/{name}"
            )));
        }
        Ok(())
    }
}

/// Reads `directory`'s `repositories.yaml`: none when it has no such file or the file lists none.
///
/// # Errors
///
/// Returns an error when the file cannot be read, is not valid, or two repositories share a path.
pub fn read(directory: &Path) -> Result<Vec<Repository>, Error> {
    let path = directory.join(REPOSITORIES_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    parse(&text).map_err(|error| match error {
        Error::Invalid(message) => Error::Invalid(format!("{}: {message}", path.display())),
        other => other,
    })
}

/// Reads the repositories `text` lists, as [`read`] does a file's.
///
/// # Errors
///
/// Returns an error when the text is not valid or two repositories share a path.
pub fn parse(text: &str) -> Result<Vec<Repository>, Error> {
    let repositories: Option<Vec<Repository>> =
        serde_yaml_ng::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    let repositories = repositories.unwrap_or_default();
    let mut destinations = std::collections::BTreeSet::new();
    for repository in &repositories {
        repository.validate()?;
        if !destinations.insert(repository.destination()) {
            return Err(Error::Invalid(format!(
                "two repositories are cloned to {}; give one a different path",
                repository.destination()
            )));
        }
    }
    Ok(repositories)
}

/// `repositories` as `repositories.yaml`, with a header that explains the file.
#[must_use]
pub fn render(repositories: &[Repository]) -> String {
    let list = if repositories.is_empty() {
        "[]\n".to_owned()
    } else {
        serde_yaml_ng::to_string(repositories).unwrap_or_default()
    };
    format!(
        "# The GitHub repositories this agent clones into its working directory, one entry each:
#   repo: owner/name
#   path: where it goes below the working directory, when not its name (optional)
#   about: what the agent uses it for, in a line (optional)
# A repository added here is cloned on the agent's next setup pass, without recreating it. One
# removed here stays in the agent until you delete it there; nothing is ever pulled or reset.
{list}"
    )
}

/// The base image workspace unit's list format: `owner/name destination` per line.
#[must_use]
pub fn workspace_list(repositories: &[Repository]) -> String {
    use std::fmt::Write as _;

    repositories.iter().fold(String::new(), |mut list, repository| {
        let _ = writeln!(list, "{} {}", repository.repo, repository.destination());
        list
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn repository(repo: &str, path: Option<&str>) -> Repository {
        Repository {
            repo: repo.into(),
            path: path.map(Into::into),
            about: None,
        }
    }

    #[test]
    fn the_written_file_reads_back_as_the_same_repositories() {
        let repositories = vec![
            repository("digdir/digdir-agents", None),
            Repository {
                about: Some("Our fork: the VM runtime agentd drives.".into()),
                ..repository("digdir/microsandbox", Some("forks/microsandbox"))
            },
        ];
        assert_eq!(parse(&render(&repositories)).expect("repositories"), repositories);
        assert_eq!(parse(&render(&[])).expect("repositories"), Vec::new());
        assert_eq!(parse("# nothing yet\n").expect("repositories"), Vec::new());
    }

    #[test]
    fn the_workspace_list_names_each_destination() {
        let list = workspace_list(&[
            repository("digdir/app", None),
            repository("digdir/microsandbox", Some("forks/microsandbox")),
        ]);
        assert_eq!(list, "digdir/app app\ndigdir/microsandbox forks/microsandbox\n");
    }

    #[test]
    fn repositories_and_paths_must_stay_inside_the_working_directory() {
        for (repo, path) in [
            ("digdir", None),
            ("https://github.com/digdir/app", None),
            ("digdir/app extra", None),
            ("digdir/..", None),
            ("digdir/app", Some("../elsewhere")),
            ("digdir/app", Some("/etc/app")),
            ("digdir/app", Some("forks//app")),
            ("digdir/app", Some("app #comment")),
        ] {
            let text = render(&[repository(repo, path)]);
            assert!(parse(&text).is_err(), "{repo} at {path:?} was accepted");
        }
    }

    #[test]
    fn two_repositories_cannot_share_a_destination() {
        let text = render(&[repository("digdir/app", None), repository("someone/app", None)]);
        let error = parse(&text).expect_err("shared destination");
        assert!(error.to_string().contains("cloned to app"), "{error}");
    }
}
