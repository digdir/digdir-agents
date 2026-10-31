//! Fetching a team's agent definition from GitHub into a temporary checkout. Only the files the
//! manifest reads are kept, in the agent's own folder; the checkout goes when setup ends.

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
    /// The address of the folder holding `manifest`, a path within the repository, at the same
    /// reference, so fetching it again finds that manifest.
    pub(super) fn address_of(&self, manifest: &std::path::Path) -> String {
        let folder = manifest
            .parent()
            .map(|folder| {
                folder
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default();
        let mut address = format!("https://github.com/{}/{}", self.owner, self.repository);
        if !folder.is_empty() {
            address.push_str("//");
            address.push_str(&folder);
        }
        if let Some(reference) = &self.reference {
            address.push('@');
            address.push_str(reference);
        }
        address
    }

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

    /// Fetches the repository into `staging/checkout` and returns the folder, or the manifest,
    /// the agent definition is in. GitHub's API reads every branch, tag and commit of a public
    /// repository, or of a private one with `GH_TOKEN` set. A repository it cannot read is cloned
    /// with the person's own `git`, which reads branches and tags.
    pub(super) async fn fetch(&mut self, staging: &Path) -> Result<PathBuf, Error> {
        let checkout = staging.join("checkout");
        if checkout.exists() {
            std::fs::remove_dir_all(&checkout)?;
        }
        eprintln!(
            "Fetching the agent definition from {}; this copy is removed when setup ends.",
            self.describe()
        );
        if self.readable_through_api().await? {
            self.download(&checkout).await?;
        } else if git_available().await {
            self.clone_with_git(&checkout).await?;
        } else {
            return Err(Error::Invalid(format!(
                "GitHub does not show {} to this machine; a private repository needs GH_TOKEN set, or git that can \
                 read it",
                self.describe()
            )));
        }
        let found = self
            .subdirectory
            .as_ref()
            .map_or_else(|| checkout.clone(), |subdirectory| checkout.join(subdirectory));
        if !found.starts_with(&checkout) || !found.exists() {
            return Err(Error::Invalid(format!(
                "{} has no such folder or file",
                self.describe()
            )));
        }
        Ok(found)
    }

    /// Downloads the repository as an archive from GitHub's API, after splitting a `/tree/` or
    /// `/blob/` address at the longest ref GitHub knows. Without a ref, GitHub gives the default
    /// branch.
    async fn download(&mut self, checkout: &Path) -> Result<(), Error> {
        if let Some(ref_and_path) = self.ref_and_path.clone() {
            let mut known = None;
            for (reference, path) in splits(&ref_and_path).into_iter().rev() {
                if self.knows(&reference).await? {
                    known = Some((reference, path));
                    break;
                }
            }
            let (reference, path) = known.ok_or_else(|| {
                Error::Invalid(format!(
                    "{}/{} has no branch, tag or commit that {ref_and_path} starts with",
                    self.owner, self.repository
                ))
            })?;
            self.ref_and_path = None;
            self.reference = Some(reference);
            self.subdirectory = Some(path).filter(|path| !path.is_empty());
        }
        let mut address = format!(
            "https://api.github.com/repos/{}/{}/tarball",
            self.owner, self.repository
        );
        if let Some(reference) = &self.reference {
            address.push('/');
            address.push_str(reference);
        }
        let response = github_request(&address)
            .send()
            .await
            .map_err(|error| Error::Invalid(format!("could not download {}: {error}", self.describe())))?;
        if !response.status().is_success() {
            return Err(Error::Invalid(format!(
                "GitHub answered {} for {}",
                response.status(),
                self.describe()
            )));
        }
        let archive = response
            .bytes()
            .await
            .map_err(|error| Error::Invalid(format!("the download of {} failed: {error}", self.describe())))?;
        unpack(&archive, checkout)
    }

    /// Whether GitHub's API shows the repository to this machine, as it does a public one, or a
    /// private one with `GH_TOKEN` set. It answers a private repository it does not show as one that
    /// does not exist.
    async fn readable_through_api(&self) -> Result<bool, Error> {
        let address = format!("https://api.github.com/repos/{}/{}", self.owner, self.repository);
        let response = github_request(&address)
            .send()
            .await
            .map_err(|error| Error::Invalid(format!("could not reach GitHub: {error}")))?;
        Ok(response.status().is_success())
    }

    /// Whether GitHub knows `reference` as a branch, a tag or a commit of the repository. Its
    /// refusing to say, as for a private repository without a token, is an error.
    async fn knows(&self, reference: &str) -> Result<bool, Error> {
        let address = format!(
            "https://api.github.com/repos/{}/{}/commits/{reference}",
            self.owner, self.repository
        );
        let response = github_request(&address)
            .send()
            .await
            .map_err(|error| Error::Invalid(format!("could not reach GitHub: {error}")))?;
        match response.status() {
            status if status.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::UNPROCESSABLE_ENTITY => Ok(false),
            status => Err(Error::Invalid(format!(
                "GitHub answered {status} for {}",
                self.describe()
            ))),
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
            command.args(["clone", "--quiet", "--depth", "1"]);
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

/// A request to GitHub's API, with the token in this machine's environment when there is one,
/// which a private repository needs.
fn github_request(address: &str) -> reqwest::RequestBuilder {
    let request = reqwest::Client::new()
        .get(address)
        .header(
            reqwest::header::USER_AGENT,
            concat!("agentctl/", env!("CARGO_PKG_VERSION")),
        )
        .header(reqwest::header::ACCEPT, "application/vnd.github+json");
    let token = ["GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    match token {
        Some(token) => request.bearer_auth(token),
        None => request,
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

/// Unpacks a GitHub tarball, whose entries all sit in one top folder, into `checkout`. Entries
/// that would land outside it are refused.
fn unpack(archive: &[u8], checkout: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(checkout)?;
    let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in entries.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let relative: PathBuf = path.components().skip(1).collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        let safe = relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !safe {
            return Err(Error::Invalid(format!(
                "the archive holds an unsafe path {}",
                path.display()
            )));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() || kind.is_file() {
            let target = checkout.join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{GithubSource, splits, unpack};

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
    fn a_chosen_manifest_is_fetched_again_from_its_folder_at_the_same_reference() {
        let altinn = source("Altinn", "altinn-studio", Some("agents"), Some("main"));
        assert_eq!(
            altinn.address_of(std::path::Path::new("agents/minimal/agent.nested.yaml")),
            "https://github.com/Altinn/altinn-studio//agents/minimal@main"
        );
        assert_eq!(
            source("digdir", "agents", None, None).address_of(std::path::Path::new("agent.yaml")),
            "https://github.com/digdir/agents"
        );
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

    #[test]
    fn an_archive_unpacks_below_its_top_folder_only() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let contents = b"kind: Agent\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "repo-abc123/agents/minimal/agent.yaml", &contents[..])
            .expect("appended");
        let archive = builder.into_inner().expect("finished").finish().expect("compressed");

        let checkout = tempfile::tempdir().expect("a checkout");
        unpack(&archive, checkout.path()).expect("unpacked");
        assert!(checkout.path().join("agents/minimal/agent.yaml").is_file());
    }
}
