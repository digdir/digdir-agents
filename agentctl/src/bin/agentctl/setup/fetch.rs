//! A team's agent definition on GitHub, on this machine. Setup downloads it, with the person's own
//! `git` when there is one, which reads what their Git can, else as GitHub's archive of a public
//! repository. A definition that mounts its own repository, as a worktree variant does, needs a
//! checkout the person keeps instead: one there already, or a clone setup makes.

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
        let owner = parts.next().filter(|part| github_name(part))?.to_owned();
        let repository = parts.next()?.trim_end_matches(".git").to_owned();
        if !github_name(&repository) {
            return None;
        }
        // A ref names a folder of setup's copy too, so it may not climb out of it.
        if reference
            .as_deref()
            .is_some_and(|reference| reference.contains('\\') || reference.split('/').any(|part| part == ".."))
        {
            return None;
        }
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
            ensure_git(&self.describe()).await?;
            let message = format!("Cloning {} into {}", self.describe(), folder.display());
            crate::prompt::spinning(&message, self.clone_with_git(folder, false)).await?;
        }
        self.found_in(folder)
    }

    /// The folder, or the manifest, the agent definition is in, in a copy of the repository at
    /// `root`.
    pub(super) fn found_in(&self, root: &Path) -> Result<PathBuf, Error> {
        let found = self
            .subdirectory
            .as_ref()
            .map_or_else(|| root.to_path_buf(), |subdirectory| root.join(subdirectory));
        // Canonical paths, so that `..` in the address cannot lead out of the copy.
        let inside = match (found.canonicalize(), root.canonicalize()) {
            (Ok(found), Ok(root)) => found.starts_with(root),
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

    /// Where the copy of the repository setup keeps is, below the folder that holds them: one for
    /// each repository and ref, as agents made from it share it.
    pub(super) fn kept_folder(&self) -> PathBuf {
        let mut repository = self.repository.clone();
        if let Some(reference) = &self.reference {
            repository.push('@');
            repository.push_str(&reference.replace(['/', '\\'], "-"));
        }
        PathBuf::from("github.com").join(&self.owner).join(repository)
    }

    /// Downloads the repository into `copy`, which must not exist. The person's `git` clones its
    /// last commit when there is one, reading what their Git can, and the copy keeps its `.git`,
    /// so it can become the person's checkout. Without one, or should it fail, GitHub's archive of
    /// it is downloaded, which only a public repository, or one `GH_TOKEN` can read, gives.
    pub(super) async fn download(&mut self, copy: &Path) -> Result<(), Error> {
        let message = format!("Downloading the agent definition from {}", self.describe());
        crate::prompt::spinning(&message, self.download_into(copy)).await
    }

    async fn download_into(&mut self, copy: &Path) -> Result<(), Error> {
        let mut cloned = None;
        if git_available().await {
            let mut attempt = self.clone();
            match attempt.clone_with_git(copy, true).await {
                Ok(()) => {
                    *self = attempt;
                    return Ok(());
                }
                Err(error) => {
                    std::fs::remove_dir_all(copy).ok();
                    cloned = Some(match error {
                        Error::Invalid(reason) => reason,
                        other => other.to_string(),
                    });
                }
            }
        }
        if self.readable_through_api().await? {
            return self.download_archive(copy).await;
        }
        Err(Error::Invalid(cloned.map_or_else(
            || {
                format!(
                    "GitHub gives no archive of {} without signing in, as for a private repository. Install \
                     git, make sure it can read the repository, and run setup again",
                    self.describe()
                )
            },
            |error| format!("{error}, and GitHub gives no archive of it without signing in"),
        )))
    }

    /// Downloads the repository as an archive from GitHub's API, after splitting a `/tree/` or
    /// `/blob/` address at the longest ref GitHub knows. Without a ref, GitHub gives the default
    /// branch.
    async fn download_archive(&mut self, copy: &Path) -> Result<(), Error> {
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
        let copy = copy.to_path_buf();
        tokio::task::spawn_blocking(move || unpack(&archive, &copy))
            .await
            .map_err(|error| Error::Invalid(format!("unpacking the download failed: {error}")))?
    }

    /// Whether GitHub's API shows the repository to this machine, as it does a public one, or a
    /// private one `GH_TOKEN` can read. It answers a private repository it does not show as one
    /// that does not exist.
    async fn readable_through_api(&self) -> Result<bool, Error> {
        let address = format!("https://api.github.com/repos/{}/{}", self.owner, self.repository);
        let response = github_request(&address)
            .send()
            .await
            .map_err(|error| Error::Invalid(format!("could not reach GitHub: {error}")))?;
        Ok(response.status().is_success())
    }

    /// Whether GitHub knows `reference` as a branch, a tag or a commit of the repository.
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
        // The whole owner, after the `/` of an address or the `:` of an SSH remote.
        let origin = origin.trim_end().trim_end_matches(".git");
        let same = origin
            .strip_suffix(&wanted)
            .is_some_and(|before| before.ends_with('/') || before.ends_with(':'));
        if same {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "{} is a checkout of another repository than {}; choose another folder",
                folder.display(),
                self.describe()
            )))
        }
    }

    /// Clones the repository with `git`, at its ref when it has one, and only its last commit when
    /// `shallow`. A `/tree/` or `/blob/` address is split at the longest branch or tag it starts
    /// with that the clone finds.
    async fn clone_with_git(&mut self, checkout: &Path, shallow: bool) -> Result<(), Error> {
        let candidates = match self.ref_and_path.take() {
            Some(ref_and_path) => splits(&ref_and_path)
                .into_iter()
                .rev()
                .map(|(reference, path)| (Some(reference), Some(path).filter(|path| !path.is_empty())))
                .collect(),
            None => vec![(self.reference.clone(), self.subdirectory.clone())],
        };
        // What git said the last time it failed, such as that it could not sign in.
        let mut said = String::new();
        for (reference, subdirectory) in candidates {
            let mut command = tokio::process::Command::new("git");
            command.args(["clone", "--quiet"]);
            if shallow {
                command.args(["--depth", "1"]);
            }
            if let Some(reference) = &reference {
                command.args(["--branch", reference]);
            }
            let output = command
                .arg(format!("https://github.com/{}/{}.git", self.owner, self.repository))
                .arg(checkout)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .output()
                .await
                .map_err(|error| Error::Invalid(format!("could not run git: {error}")))?;
            if output.status.success() {
                self.reference = reference;
                self.subdirectory = subdirectory;
                return Ok(());
            }
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .rfind(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .clone_into(&mut said);
        }
        let said = if said.is_empty() {
            String::new()
        } else {
            format!(" ({said})")
        };
        Err(Error::Invalid(format!(
            "git could not clone {}{said}; check the address, the branch or tag, and that your git can read it",
            self.describe()
        )))
    }
}

/// Makes the download at `copy`, a clone of the last commit, the person's checkout at `folder`,
/// so a large repository is not cloned twice. Its remote is set to fetch every branch, as a
/// clone's does. Returns whether it did: not when `folder` holds files, or `copy` is an archive,
/// or cannot be moved there, as across disks.
pub(super) fn adopt(copy: &Path, folder: &Path) -> bool {
    let holds_files = std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some());
    if holds_files || !copy.join(".git").is_dir() {
        return false;
    }
    let parent = folder.parent().unwrap_or(folder);
    std::fs::remove_dir(folder).ok();
    if std::fs::create_dir_all(parent).is_err() || std::fs::rename(copy, folder).is_err() {
        return false;
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["remote", "set-branches", "origin", "*"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
    eprintln!(
        "Moved the download to {}. It holds the latest commit; `git fetch --unshallow` there brings its history.",
        folder.display()
    );
    true
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

/// Unpacks a GitHub archive, whose entries all sit in one top folder, into `copy`. Entries that
/// would land outside it are refused, and only files and folders are made.
fn unpack(archive: &[u8], copy: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(copy)?;
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
            let target = copy.join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&target)?;
        }
    }
    Ok(())
}

/// Whether `name` can be a GitHub owner or repository, which also name folders of setup's copy:
/// letters, digits, `.`, `_` and `-`, and not `.` or `..`.
fn github_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

/// Every way to split `ref_and_path` into a ref and a path, the shortest ref first.
fn splits(ref_and_path: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = ref_and_path.split('/').collect();
    (1..=parts.len())
        .map(|count| (parts[..count].join("/"), parts[count..].join("/")))
        .collect()
}

/// What to install, for this system, when there is no `git`.
const GIT_ADVICE: &str = if cfg!(target_os = "macos") {
    "Install it with `xcode-select --install`, or `brew install git`, and run setup again."
} else if cfg!(windows) {
    "Install it with `winget install Git.Git`, and run setup again."
} else {
    "Install it from your distribution (for example `sudo apt install git`, or `sudo dnf install git`), and run \
     setup again."
};

/// Checks that `git` runs, which an agent that works on a checkout of `repository` needs, saying
/// what to install otherwise.
pub(super) async fn ensure_git(repository: &str) -> Result<(), Error> {
    if git_available().await {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "this agent works on a checkout of {repository}, which needs git. {GIT_ADVICE}"
        )))
    }
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

    use super::{GithubSource, adopt, splits, unpack};

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
        assert_eq!(
            GithubSource::parse(r"github.com/..\..\repos\digdir/digdir-agents"),
            None,
            "a name that is a path on Windows"
        );
        assert_eq!(GithubSource::parse("github.com/digdir/..@main"), None);
        assert_eq!(GithubSource::parse("github.com/digdir/agents@../../x"), None);
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

        let copy = tempfile::tempdir().expect("a folder");
        unpack(&archive, &copy.path().join("copy")).expect("unpacked");
        assert!(copy.path().join("copy/agents/minimal/agent.yaml").is_file());
    }

    #[test]
    fn each_repository_and_ref_has_a_copy_of_its_own() {
        let mut address =
            GithubSource::parse("https://github.com/Altinn/altinn-studio//agents/minimal").expect("an address");
        assert_eq!(
            address.kept_folder(),
            std::path::Path::new("github.com/Altinn/altinn-studio")
        );
        address.reference = Some("feature/x".to_owned());
        assert_eq!(
            address.kept_folder(),
            std::path::Path::new("github.com/Altinn/altinn-studio@feature-x")
        );
    }

    #[test]
    fn a_clone_downloaded_for_a_worktree_becomes_the_checkout() {
        let folder = tempfile::tempdir().expect("a folder");
        let download = |name: &str, git: bool| {
            let copy = folder.path().join(name);
            std::fs::create_dir_all(copy.join("agents")).expect("a download");
            if git {
                std::fs::create_dir_all(copy.join(".git")).expect("its .git");
            }
            copy
        };

        let clone = download("clone", true);
        let checkout = folder.path().join("repos/checkout");
        assert!(adopt(&clone, &checkout));
        assert!(checkout.join("agents").is_dir() && !clone.exists());

        let archive = download("archive", false);
        assert!(
            !adopt(&archive, &folder.path().join("from-archive")),
            "an archive is no checkout"
        );

        let another = download("another", true);
        let taken = folder.path().join("taken");
        std::fs::create_dir_all(&taken).expect("a folder");
        std::fs::write(taken.join("notes.md"), "mine").expect("a file in it");
        assert!(!adopt(&another, &taken), "a folder that holds files is left alone");
        assert!(another.exists());
    }
}
