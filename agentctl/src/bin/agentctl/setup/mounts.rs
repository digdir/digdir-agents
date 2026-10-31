//! The folders on this machine a new agent shares with the person: the checkout its own repository
//! mounts, the mounts of its definition, a folder the person chooses, what may not be mounted, the
//! agent definitions a folder already holds, and where a folder appears in the sandbox.

use super::answers::{Answers, Choice};
use super::source::manifest_label;
use super::{Draft, SetupArguments, Source};
use crate::{CommandError, CommandResult};
use agent::local::home::ControlPlaneHome;

use std::path::{Component, Path, PathBuf};

use agent::{
    Agent, Error, MountSpec,
    manifest::{self, is_manifest_filename},
};
use sandbox::SandboxPath;

/// The folders right below the home folder that hold settings and credentials, besides those whose
/// name starts with a dot, such as `.ssh`, `.aws` and `.config`.
const PRIVATE_IN_HOME: [&str; 2] = ["Library", "AppData"];

/// What may not reach an agent, nor any folder holding it: the person's settings and credentials,
/// the control plane's state and every agent's secrets. It applies to the folders an agent mounts
/// and the files its definition copies into it, wherever on this machine those come from.
pub(super) struct Protected {
    pub(super) user_home: Option<PathBuf>,
    pub(super) inside: Vec<PathBuf>,
}

impl Protected {
    /// Checks `folder` and returns it in canonical form, or why it may not be mounted.
    pub(super) fn check(&self, folder: &Path) -> Result<PathBuf, String> {
        let folder = self.check_path(folder)?;
        if !folder.is_dir() {
            return Err(format!("{} is not a folder", folder.display()));
        }
        Ok(folder)
    }

    /// Checks `path`, a folder or a file, and returns it in canonical form, or why it may not
    /// reach an agent.
    pub(super) fn check_path(&self, path: &Path) -> Result<PathBuf, String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if path.parent().is_none() {
            return Err("the root of the file system cannot be mounted".into());
        }
        if let Some(home) = self.user_home.as_deref().and_then(|home| home.canonicalize().ok()) {
            if home.starts_with(&path) {
                return Err(format!(
                    "{} holds your whole home folder; choose a project folder inside it",
                    path.display()
                ));
            }
            if let Some(Component::Normal(first)) = path
                .strip_prefix(&home)
                .ok()
                .and_then(|below| below.components().next())
                && let first = first.to_string_lossy()
                && (first.starts_with('.') || PRIVATE_IN_HOME.contains(&first.as_ref()))
            {
                let private = home.join(first.as_ref());
                let place = if path == private {
                    String::new()
                } else {
                    format!(" is in {}, which", private.display())
                };
                return Err(format!(
                    "{}{place} holds settings and credentials and must stay out of agents",
                    path.display()
                ));
            }
        }
        for protected in &self.inside {
            let protected = protected.canonicalize().unwrap_or_else(|_| protected.clone());
            if path.starts_with(&protected) || protected.starts_with(&path) {
                return Err(format!(
                    "{} overlaps {}, which must stay out of agents",
                    path.display(),
                    protected.display()
                ));
            }
        }
        Ok(path)
    }
}

/// The agent manifests in `folder`: in it, in its direct subfolders, and in `agents/*/`, where
/// repositories keep their agents.
pub(super) fn found_manifests(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut directories = vec![folder.to_path_buf()];
    directories.extend(subdirectories(folder));
    directories.extend(subdirectories(&folder.join("agents")));
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_manifest_filename(&path) && !found.contains(&path) {
                found.push(path);
            }
        }
    }
    // Each folder's agent first, then its variants, so the default choice is the agent itself.
    found.sort_by_key(|path| {
        (
            path.parent().map(Path::to_path_buf),
            manifest::variant_from_filename(path).is_some(),
            path.clone(),
        )
    });
    found
}

fn subdirectories(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut directories: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    directories.sort();
    directories
}

/// The Git repository an agent's manifest is in. A relative bind mount source in the manifest
/// mounts a part of it, such as `../..` for the whole repository from `agents/minimal`, so it
/// needs a checkout of that repository on this machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Repository {
    /// Where setup reads it: the temporary checkout it fetched, or the local one the manifest is in.
    pub(super) root: PathBuf,
    /// Its name, such as `altinn-studio`.
    pub(super) name: String,
    /// Whether `root` is the temporary checkout, which is removed when setup ends.
    pub(super) fetched: bool,
}

impl Repository {
    /// The local Git checkout `folder` is in, by the `.git` at its root.
    pub(super) fn containing(folder: &Path) -> Option<Self> {
        let folder = folder.canonicalize().ok()?;
        let root = folder.ancestors().find(|folder| folder.join(".git").exists())?;
        Some(Self {
            name: root.file_name()?.to_string_lossy().into_owned(),
            root: root.to_path_buf(),
            fetched: false,
        })
    }

    /// The temporary checkout of the repository `name` at `root`.
    pub(super) fn fetched(root: &Path, name: &str) -> Result<Self, Error> {
        Ok(Self {
            root: root.canonicalize()?,
            name: name.to_owned(),
            fetched: true,
        })
    }
}

/// A bind mount of the agent's own repository.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct RepositoryMount {
    /// Its place among the agent's mounts.
    pub(super) index: usize,
    /// The folder it mounts, relative to the repository's root; empty for the whole repository.
    pub(super) within: PathBuf,
}

/// The bind mounts of `agent`, whose manifest is in `source_directory`, that mount a part of
/// `repository` by a relative source. One that climbs out of the repository is refused: it would
/// mount whatever lies beside the checkout, which for a fetched one is setup's own state.
pub(super) fn repository_mounts(
    agent: &Agent,
    source_directory: &Path,
    repository: &Repository,
) -> Result<Vec<RepositoryMount>, Error> {
    let source_directory = source_directory.canonicalize()?;
    let mut found = Vec::new();
    for (index, mount) in agent.spec.sandbox.mounts.iter().enumerate() {
        let MountSpec::Bind { source, .. } = mount else {
            continue;
        };
        if !is_relative_source(source) {
            continue;
        }
        let resolved = normalized(&source_directory.join(source));
        let within = resolved.strip_prefix(&repository.root).map_err(|_| {
            Error::Invalid(format!(
                "spec.sandbox.mounts[{index}].source {} leaves the repository {} the manifest is in; mount a folder \
                 of the repository, or give an absolute path",
                source.display(),
                repository.name
            ))
        })?;
        found.push(RepositoryMount {
            index,
            within: within.to_path_buf(),
        });
    }
    Ok(found)
}

/// Whether a manifest's bind mount `source` is resolved against the manifest's folder, rather
/// than being absolute or below the home directory. A path from the root without a drive, such as
/// `/srv/data` on Windows, is not absolute there, but it is not resolved against the folder either.
pub(super) fn is_relative_source(source: &Path) -> bool {
    source.is_relative() && !source.has_root() && !source.starts_with("~")
}

/// `path` with `.` and `..` worked out from the path alone, as it reads, not as links lead.
fn normalized(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// The read-write bind mount that puts `folder` in the agent's working directory under its own
/// name, as a checkout cloned there would be.
pub(super) fn bind_into_working_directory(os: &str, folder: &Path) -> Result<MountSpec, Error> {
    Ok(MountSpec::Bind {
        source: folder.to_path_buf(),
        target: working_directory_target(os, folder)?,
        read_only: false,
    })
}

/// Where [`bind_into_working_directory`] puts `folder` in the sandbox.
pub(super) fn working_directory_target(os: &str, folder: &Path) -> Result<SandboxPath, Error> {
    let working = agent::sandbox::platform::working_directory(os)
        .ok_or_else(|| Error::Invalid(format!("cannot mount folders into a {os} agent")))?;
    let name = folder
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .replace(|character: char| character == '/' || character.is_control(), "-")
        })
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .ok_or_else(|| Error::Invalid(format!("{} has no folder name to mount it under", folder.display())))?;
    Ok(SandboxPath::new(format!("{working}/{name}")))
}

/// What no agent may mount or have copied in, nor any folder holding it.
pub(super) fn protected_folders(agents_root: &Path, home: &ControlPlaneHome) -> Protected {
    Protected {
        user_home: agent::local::home::user_home_directory(),
        inside: vec![agents_root.to_path_buf(), home.path().to_path_buf()],
    }
}

/// The mounts of the definition's own repository, by a relative source such as `../..`, which
/// share a checkout of it.
struct RepositoryShares {
    repository: Repository,
    found: Vec<RepositoryMount>,
}

impl RepositoryShares {
    /// The mounts of `agent`'s own repository, when its definition, from `source`, has any.
    fn of(agent: &Agent, source: &Source) -> Result<Option<Self>, Error> {
        let Some(repository) = source.repository.clone() else {
            return Ok(None);
        };
        let found = repository_mounts(agent, &source.directory, &repository)?;
        Ok((!found.is_empty()).then_some(Self { repository, found }))
    }

    /// Where the agent sees its repository.
    fn targets(&self, agent: &Agent) -> String {
        let targets: Vec<&str> = self
            .found
            .iter()
            .filter_map(|mount| match &agent.spec.sandbox.mounts[mount.index] {
                MountSpec::Bind { target, .. } => Some(target.as_str()),
                MountSpec::Tmpfs { .. } => None,
            })
            .collect();
        targets.join(" and ")
    }

    fn shares(&self, index: usize) -> Option<&RepositoryMount> {
        self.found.iter().find(|mount| mount.index == index)
    }

    /// `answer` as a checkout of the repository these mounts can share, or why it is not one.
    fn checkout(&self, protected: &Protected, answer: &str) -> Result<PathBuf, String> {
        if answer.trim().is_empty() {
            return Err(format!("name a checkout of {}", self.repository.name));
        }
        let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
        let checkout = protected.check(&path)?;
        match self
            .found
            .iter()
            .map(|mount| checkout.join(&mount.within))
            .find(|folder| !folder.is_dir())
        {
            Some(missing) => Err(format!("{} has no folder {}", checkout.display(), missing.display())),
            None => Ok(checkout),
        }
    }

    /// Asks for the checkout to share, offering `current`.
    fn ask_checkout(
        &self,
        answers: &Answers,
        protected: &Protected,
        question: &str,
        current: Option<&Path>,
    ) -> CommandResult<PathBuf> {
        let current = current.map(|current| current.display().to_string());
        let answer = answers.text("--checkout", question, current.as_deref(), |answer| {
            self.checkout(protected, answer).map(|_| ())
        })?;
        Ok(self.checkout(protected, &answer).map_err(Error::Invalid)?)
    }
}

/// What the agent shares, as decided so far: the checkout its repository's mounts share, and the
/// mounts of its definition left out, by their place and by where the agent sees them.
#[derive(Default)]
struct Shares {
    checkout: Option<PathBuf>,
    left_out: Vec<usize>,
    unshared: Vec<String>,
}

/// A folder on this machine the agent shares.
struct Shared {
    /// Its place among the agent's mounts.
    index: usize,
    source: PathBuf,
    target: String,
    read_only: bool,
}

impl Shared {
    fn describe(&self) -> String {
        let access = if self.read_only { "read-only" } else { "read-write" };
        format!(
            "- {} ({access}), which the agent sees at {}",
            self.source.display(),
            self.target
        )
    }
}

/// The folders `agent`, whose manifest is in `directory`, shares once `shares` applies.
fn listed(
    agent: &Agent,
    directory: &Path,
    repository: Option<&RepositoryShares>,
    shares: &Shares,
) -> Result<Vec<Shared>, Error> {
    let mut listed = Vec::new();
    for (index, mount) in agent.spec.sandbox.mounts.iter().enumerate() {
        let MountSpec::Bind {
            source,
            target,
            read_only,
        } = mount
        else {
            continue;
        };
        if shares.left_out.contains(&index) {
            continue;
        }
        let source = match (
            repository.and_then(|repository| repository.shares(index)),
            &shares.checkout,
        ) {
            (Some(mount), Some(checkout)) => within(checkout, &mount.within),
            (Some(_), None) => continue,
            (None, _) => {
                let source = manifest::resolve_host_path(directory, source, "spec.sandbox.mounts")?;
                source.canonicalize().unwrap_or(source)
            }
        };
        listed.push(Shared {
            index,
            source,
            target: target.as_str().to_owned(),
            read_only: *read_only,
        });
    }
    Ok(listed)
}

/// Applies `shares` to `agent`: its repository's mounts share the checkout chosen, or are left out
/// without one, and the mounts left out are removed.
fn apply(agent: &mut Agent, repository: Option<&RepositoryShares>, shares: &Shares) {
    let mut removed = shares.left_out.clone();
    for mount in repository
        .map(|repository| repository.found.as_slice())
        .unwrap_or_default()
    {
        match &shares.checkout {
            Some(checkout) => {
                if let MountSpec::Bind { source, .. } = &mut agent.spec.sandbox.mounts[mount.index] {
                    *source = within(checkout, &mount.within);
                }
            }
            None => removed.push(mount.index),
        }
    }
    removed.sort_unstable();
    removed.dedup();
    for index in removed.into_iter().rev() {
        agent.spec.sandbox.mounts.remove(index);
    }
}

/// The folder `within` of `checkout`. Joining an empty path would add a trailing separator.
fn within(checkout: &Path, within: &Path) -> PathBuf {
    if within.as_os_str().is_empty() {
        checkout.to_path_buf()
    } else {
        checkout.join(within)
    }
}

/// Decides which folders on this machine the agent shares with the person, read and written by
/// both, and applies it: the checkout its repository's mounts share, mounts of its definition left
/// out, and a folder of the person's own. The flags decide, and what they leave open is asked when
/// `ask` allows, or left as the definition has it. Returns a manifest to start from instead, with
/// the folder, when a standard agent was chosen and the folder holds agent definitions the person
/// chooses one of.
pub(super) fn choose_shared(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    protected: &Protected,
    ask: bool,
) -> CommandResult<Option<(PathBuf, PathBuf)>> {
    let ask = ask && answers.interactive() && !arguments.yes;
    let repository = RepositoryShares::of(&draft.agent, &draft.source)?;
    let mut shares = Shares::default();
    if let Some(repository) = &repository {
        shares.checkout = first_checkout(arguments, answers, &draft.agent, repository, protected, ask)?;
    }
    for target in &arguments.unshare {
        let listed = listed(&draft.agent, &draft.source.directory, repository.as_ref(), &shares)?;
        let unshared = listed.iter().find(|shared| shared.target == *target).ok_or_else(|| {
            Error::Invalid(format!(
                "the agent shares no folder at {target}; --unshare names where the agent sees one"
            ))
        })?;
        shares.left_out.push(unshared.index);
        shares.unshared.push(target.clone());
    }
    let os = draft.agent.spec.sandbox.platform.os.clone();
    let added = match &arguments.mount {
        Some(folder) => {
            let listed = listed(&draft.agent, &draft.source.directory, repository.as_ref(), &shares)?;
            Some(check_added(&os, &listed, protected, &folder.display().to_string()).map_err(Error::Invalid)?)
        }
        None if ask && !arguments.no_mount => ask_shares(answers, draft, repository.as_ref(), &mut shares, protected)?,
        None => None,
    };
    if let Some(folder) = &added
        && draft.source.from_catalog
        && let Some(manifest) = offer_found_manifests(answers, folder)?
    {
        return Ok(Some((manifest, folder.clone())));
    }
    apply(&mut draft.agent, repository.as_ref(), &shares);
    if let Some(folder) = &added {
        mount_unless_mounted(draft, folder)?;
    }
    Ok(None)
}

/// The checkout the repository's mounts share before anything else is asked: the one `--checkout`
/// names, else the one a local manifest is in. A fetched repository is removed when setup ends, so
/// sharing it takes a checkout on this machine, asked for when `ask` allows; without one, its mounts
/// are left out.
fn first_checkout(
    arguments: &SetupArguments,
    answers: &Answers,
    agent: &Agent,
    repository: &RepositoryShares,
    protected: &Protected,
    ask: bool,
) -> CommandResult<Option<PathBuf>> {
    let named = match &arguments.checkout {
        Some(checkout) => Some(checkout.clone()),
        None if !repository.repository.fetched => Some(repository.repository.root.clone()),
        None => None,
    };
    if let Some(checkout) = named {
        let checkout = repository
            .checkout(protected, &checkout.display().to_string())
            .map_err(Error::Invalid)?;
        return Ok(Some(checkout));
    }
    let name = &repository.repository.name;
    let at = repository.targets(agent);
    if !ask {
        eprintln!(
            "warning: the agent does not share {name}, which its definition shares at {at}: setup fetched {name} \
             only to read the definition. Pass --checkout with a checkout of {name} to share it."
        );
        return Ok(None);
    }
    let lines = [
        format!("The agent's definition shares its repository, {name}, at {at}."),
        format!(
            "Setup fetched {name} only to read the definition, so sharing it takes a checkout of {name} on this \
             machine."
        ),
        format!("Share a checkout of {name} with the agent?"),
    ];
    let share = answers.choose(
        "--checkout",
        &lines.join("\n"),
        vec![
            Choice::new(false, "No, leave it out"),
            Choice::new(true, "Yes, choose the checkout"),
        ],
        0,
    )?;
    if !share {
        return Ok(None);
    }
    let question = format!("Checkout of {name} to share");
    repository.ask_checkout(answers, protected, &question, None).map(Some)
}

/// What the person does with the folders the agent shares.
#[derive(Clone, Copy)]
enum Change {
    Keep,
    Checkout,
    Add,
    LeaveOut,
}

/// Shows the folders the agent shares and lets the person change them, until they keep them or
/// share a folder of their own, which it returns.
fn ask_shares(
    answers: &Answers,
    draft: &Draft,
    repository: Option<&RepositoryShares>,
    shares: &mut Shares,
    protected: &Protected,
) -> CommandResult<Option<PathBuf>> {
    let os = &draft.agent.spec.sandbox.platform.os;
    loop {
        let listed = listed(&draft.agent, &draft.source.directory, repository, shares)?;
        if listed.is_empty() {
            let lines = [
                "You and the agent then work on the same files, and what it changes lands in your folder.",
                "Without one, it works on its own copies, which you reach over SSH or in your editor.",
                "Share a folder on this machine with the agent?",
            ];
            let share = answers.choose(
                "--mount",
                &lines.join("\n"),
                vec![
                    Choice::new(false, "No, it works on its own copies").flag("--no-mount", &[]),
                    Choice::new(true, "Yes, choose a folder"),
                ],
                0,
            )?;
            return if share {
                ask_folder(answers, os, &listed, protected).map(Some)
            } else {
                Ok(None)
            };
        }
        let mut lines = vec!["The agent shares these folders on this machine with you:".to_owned()];
        lines.extend(listed.iter().map(Shared::describe));
        lines.push("Keep them as they are?".to_owned());
        let mut choices = vec![Choice::new(Change::Keep, "Keep them").flag("--no-mount", &[])];
        // Another checkout is offered while the repository is shared, not once it is left out.
        let shared_repository =
            repository.filter(|repository| listed.iter().any(|shared| repository.shares(shared.index).is_some()));
        if let Some(repository) = shared_repository {
            choices.push(Choice::new(
                Change::Checkout,
                format!(
                    "Share another checkout of {}, such as a worktree",
                    repository.repository.name
                ),
            ));
        }
        choices.push(Choice::new(Change::Add, "Share another folder too"));
        choices.push(Choice::new(Change::LeaveOut, "Leave one out"));
        match answers.choose("--mount", &lines.join("\n"), choices, 0)? {
            Change::Keep => return Ok(None),
            Change::Checkout => {
                if let Some(repository) = shared_repository {
                    let question = format!(
                        "Checkout of {} to share, such as another worktree of it",
                        repository.repository.name
                    );
                    let current = shares.checkout.clone();
                    shares.checkout =
                        Some(repository.ask_checkout(answers, protected, &question, current.as_deref())?);
                }
            }
            Change::Add => return ask_folder(answers, os, &listed, protected).map(Some),
            Change::LeaveOut => leave_one_out(answers, &listed, shares)?,
        }
    }
}

/// Asks which of the `listed` folders the agent does not share after all.
fn leave_one_out(answers: &Answers, listed: &[Shared], shares: &mut Shares) -> CommandResult<()> {
    let choices = listed
        .iter()
        .map(|shared| {
            Choice::new(
                (shared.index, shared.target.clone()),
                format!("{}, at {}", shared.source.display(), shared.target),
            )
        })
        .collect();
    let (index, target) = answers.choose("--unshare", "Which folder should the agent not share?", choices, 0)?;
    shares.left_out.push(index);
    shares.unshared.push(target);
    let unshared: Vec<&str> = shares.unshared.iter().map(String::as_str).collect();
    answers.record("--unshare", &unshared);
    Ok(())
}

/// Asks for a folder of the person's own to share with an agent running `os`.
fn ask_folder(answers: &Answers, os: &str, listed: &[Shared], protected: &Protected) -> CommandResult<PathBuf> {
    let working = agent::sandbox::platform::working_directory(os).unwrap_or("its working directory");
    let question = format!("Folder to share; the agent sees it in {working} under its own name");
    let answer = answers.text("--mount", &question, None, |answer| {
        check_added(os, listed, protected, answer).map(|_| ())
    })?;
    Ok(check_added(os, listed, protected, &answer).map_err(Error::Invalid)?)
}

/// `answer` as a folder of the person's own to share, or why it cannot be one: it must be a folder
/// agents may mount, not shared already, and land where no other `listed` folder does.
fn check_added(os: &str, listed: &[Shared], protected: &Protected, answer: &str) -> Result<PathBuf, String> {
    if answer.trim().is_empty() {
        return Err("name a folder to share".into());
    }
    let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
    let folder = protected.check(&path)?;
    if let Some(shared) = listed.iter().find(|shared| shared.source == folder) {
        return Err(format!("{} is shared already, at {}", folder.display(), shared.target));
    }
    let target = working_directory_target(os, &folder).map_err(|error| error.to_string())?;
    if listed.iter().any(|shared| shared.target == target.as_str()) {
        return Err(format!(
            "the agent shares another folder at {} already; choose a folder of another name",
            target.as_str()
        ));
    }
    Ok(folder)
}

/// The host folders `draft` mounts already, resolved, with where each appears in the sandbox.
pub(super) fn bind_mounts(draft: &Draft) -> Result<Vec<(PathBuf, String)>, Error> {
    draft
        .agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            agent::MountSpec::Bind { source, target, .. } => Some((source, target)),
            agent::MountSpec::Tmpfs { .. } => None,
        })
        .map(|(source, target)| {
            let source = manifest::resolve_host_path(&draft.source.directory, source, "spec.sandbox.mounts")?;
            Ok((source.canonicalize().unwrap_or(source), target.as_str().to_owned()))
        })
        .collect()
}

/// Mounts `folder` in the agent's working directory, unless the agent mounts it already, as the
/// repository mount of an agent chosen from it does.
pub(super) fn mount_unless_mounted(draft: &mut Draft, folder: &Path) -> CommandResult<()> {
    if bind_mounts(draft)?.iter().any(|(source, _)| source == folder) {
        return Ok(());
    }
    let os = draft.agent.spec.sandbox.platform.os.clone();
    draft
        .agent
        .spec
        .sandbox
        .mounts
        .push(bind_into_working_directory(&os, folder)?);
    Ok(())
}

/// When the folder holds agent definitions, offers to start from one of them.
pub(super) fn offer_found_manifests(answers: &Answers, folder: &Path) -> CommandResult<Option<PathBuf>> {
    let found = found_manifests(folder);
    if found.is_empty() {
        return Ok(None);
    }
    let mut choices = vec![Choice::new(None, "No, keep the agent chosen above")];
    choices.extend(found.into_iter().map(|manifest| {
        let label = format!("Use {}", manifest_label(folder, &manifest));
        let from = manifest.display().to_string();
        Choice::new(Some(manifest), label)
            .flag("--source", &["team"])
            .flag("--from", &[&from])
    }));
    let question = format!(
        "{} defines agents of its own. Start from one of them instead?",
        folder.display()
    );
    let chosen = answers.choose("--from", &question, choices, 0)?;
    if chosen.is_some() {
        answers.forget("--archetype");
    }
    Ok(chosen)
}

/// Checks every folder the agent mounts before anything is written, those its definition names as
/// well as those chosen here, since a definition fetched from GitHub is no more trusted than a
/// folder typed in. agentd refuses a missing folder or a `.env` too, but only after the agent's
/// folder is written, and it does not know the folders `protected` keeps out of agents.
pub(super) fn check_mounts(draft: &Draft, protected: &Protected) -> CommandResult<()> {
    for (source, target) in bind_mounts(draft)? {
        check_mount(&source, &target, protected).map_err(CommandError::Message)?;
    }
    Ok(draft.agent.validate()?)
}

/// Why the agent may not mount `source` at `target`: it must be a folder here, not one `protected`
/// keeps out of agents, and hold no `.env`, whose values the agent would see.
fn check_mount(source: &Path, target: &str, protected: &Protected) -> Result<(), String> {
    if !source.is_dir() {
        return Err(format!(
            "the agent mounts {} at {target}, but it is not a folder here",
            source.display()
        ));
    }
    protected
        .check(source)
        .map_err(|reason| format!("the agent may not mount {} at {target}: {reason}", source.display()))?;
    if let Some(found) = agent::control_plane::find_dot_env(source).map_err(|error| error.to_string())? {
        return Err(format!(
            "the agent mounts {} at {target}, which holds {}; the agent would see its values. Move it out of the \
             folder, or mount another",
            source.display(),
            found.display()
        ));
    }
    Ok(())
}

/// Refuses a relative bind mount that resolves into setup's staging folder outside a repository
/// it knows, as of a standard agent extracted there: the folder is removed when setup ends.
pub(super) fn refuse_staged_mounts(draft: &Draft, staging: &Path) -> CommandResult<()> {
    if draft.source.repository.is_some() {
        return Ok(());
    }
    let staging = staging.canonicalize().unwrap_or_else(|_| staging.to_path_buf());
    for (source, target) in bind_mounts(draft)? {
        if source.starts_with(&staging) {
            return Err(CommandError::Message(format!(
                "the agent mounts {} at {target}, a folder setup removes when it ends",
                source.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fmt::Write as _;

    use super::{
        Protected, Repository, RepositoryMount, RepositoryShares, Shares, apply, check_added, check_mount,
        found_manifests, listed, repository_mounts,
    };

    #[test]
    fn manifests_are_found_beside_one_level_down_and_in_agents() {
        let folder = tempfile::tempdir().expect("a folder");
        let root = folder.path();
        for path in [
            "agent.yaml",
            "tool/agent.nested.yaml",
            "agents/minimal/agent.yaml",
            "agents/minimal/agent.worktree.yaml",
            "deep/er/agent.yaml",
            ".hidden/agent.yaml",
            "tool/notes.yaml",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
            std::fs::write(path, "").expect("a file");
        }
        let found: Vec<_> = found_manifests(root)
            .into_iter()
            .map(|path| {
                let below = path.strip_prefix(root).expect("below");
                let parts: Vec<_> = below
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect();
                parts.join("/")
            })
            .collect();
        assert_eq!(
            found,
            [
                "agent.yaml",
                "agents/minimal/agent.yaml",
                "agents/minimal/agent.worktree.yaml",
                "tool/agent.nested.yaml"
            ]
        );
    }

    #[test]
    fn home_and_protected_folders_cannot_be_mounted() {
        let home = tempfile::tempdir().expect("a home");
        let project = home.path().join("code/project");
        let agents = home.path().join("agents");
        std::fs::create_dir_all(&project).expect("a project");
        std::fs::create_dir_all(agents.join("worker")).expect("an agent folder");
        let protected = Protected {
            user_home: Some(home.path().to_path_buf()),
            inside: vec![agents.clone()],
        };
        assert!(protected.check(&project).is_ok());
        assert!(protected.check(home.path()).is_err());
        assert!(protected.check(&agents.join("worker")).is_err());
        assert!(protected.check(&home.path().join("missing")).is_err());
        for private in [".ssh", ".aws", ".config/gh", "Library/LaunchAgents", "AppData/Roaming"] {
            let folder = home.path().join(private);
            std::fs::create_dir_all(&folder).expect("a private folder");
            let error = protected.check(&folder).expect_err("refused");
            assert!(error.contains("settings and credentials"), "{error}");
        }
        assert!(protected.check(&project.join("..")).is_ok());
    }

    #[test]
    fn a_definition_may_not_mount_what_a_person_may_not() {
        let home = tempfile::tempdir().expect("a home");
        let project = home.path().join("code/project");
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(&project).expect("a project");
        std::fs::create_dir_all(&ssh).expect("an .ssh");
        let protected = Protected {
            user_home: Some(home.path().to_path_buf()),
            inside: vec![ssh.clone()],
        };
        assert!(check_mount(&project, "/srv/project", &protected).is_ok());
        for refused in [ssh.as_path(), home.path()] {
            let error = check_mount(refused, "/srv/keys", &protected).expect_err("refused");
            assert!(error.contains("may not mount"), "{error}");
        }
        std::fs::write(project.join(".env"), "TOKEN=x\n").expect("a .env");
        let error = check_mount(&project, "/srv/project", &protected).expect_err("holds a .env");
        assert!(error.contains(".env"), "{error}");
    }

    fn agent_mounting(sources: &[&str]) -> agent::Agent {
        let mut mounts = String::new();
        for (index, source) in sources.iter().enumerate() {
            let _ = writeln!(
                mounts,
                "      - {{type: bind, source: {source:?}, target: /srv/m{index}, readOnly: false}}"
            );
        }
        let yaml = format!(
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {{name: worker}}\nspec:\n  sandbox:\n    \
             image: {{type: reference, reference: example.com/agent}}\n    platform: {{os: linux}}\n    \
             resources: {{cpu: '2', memory: 4Gi, rootFilesystem: {{capacity: 20Gi, mode: layered}}}}\n    \
             mounts:\n{mounts}  home: {{source: home}}\n  harnesses: [{{type: pi}}]\n  \
             network: {{mode: mediated, allow: all}}\n"
        );
        serde_yaml_ng::from_str(&yaml).expect("an agent")
    }

    #[test]
    fn relative_mounts_are_parts_of_the_repository_and_one_leaving_it_is_refused() {
        let checkout = tempfile::tempdir().expect("a checkout");
        let manifest_folder = checkout.path().join("agents/desktop");
        std::fs::create_dir_all(&manifest_folder).expect("folders");
        std::fs::create_dir(checkout.path().join(".git")).expect("a .git");
        let repository = Repository::containing(&manifest_folder).expect("a repository");
        assert_eq!(repository.root, checkout.path().canonicalize().expect("canonical"));
        assert!(!repository.fetched);

        let agent = agent_mounting(&["../..", "../../src", "/srv/data", "~/notes"]);
        let found = repository_mounts(&agent, &manifest_folder, &repository).expect("mounts");
        assert_eq!(
            found,
            [
                RepositoryMount {
                    index: 0,
                    within: "".into()
                },
                RepositoryMount {
                    index: 1,
                    within: "src".into()
                },
            ]
        );
        let leaving = agent_mounting(&["../../.."]);
        let error = repository_mounts(&leaving, &manifest_folder, &repository).expect_err("leaves the repository");
        assert!(error.to_string().contains("leaves the repository"), "{error}");
    }

    #[test]
    fn the_repository_shares_the_checkout_chosen_and_without_one_is_left_out() {
        let checkout = tempfile::tempdir().expect("a checkout");
        let root = checkout.path().canonicalize().expect("canonical");
        let manifest_folder = root.join("agents/desktop");
        std::fs::create_dir_all(&manifest_folder).expect("folders");
        std::fs::create_dir(root.join(".git")).expect("a .git");
        let data = tempfile::tempdir().expect("a data folder");
        let data = data.path().canonicalize().expect("canonical");
        let mut agent = agent_mounting(&["../..", &data.display().to_string(), "../../src"]);
        let repository = Repository::containing(&manifest_folder).expect("a repository");
        let found = repository_mounts(&agent, &manifest_folder, &repository).expect("mounts");
        let repository = RepositoryShares { repository, found };

        let worktree = tempfile::tempdir().expect("a worktree");
        let worktree = worktree.path().canonicalize().expect("canonical");
        let shares = Shares {
            checkout: Some(worktree.clone()),
            left_out: vec![2],
            unshared: vec!["/srv/m2".into()],
        };
        let shown: Vec<_> = listed(&agent, &manifest_folder, Some(&repository), &shares)
            .expect("listed")
            .into_iter()
            .map(|shared| (shared.source, shared.target))
            .collect();
        assert_eq!(
            shown,
            [(worktree.clone(), "/srv/m0".into()), (data.clone(), "/srv/m1".into())]
        );
        let mut applied = agent.clone();
        apply(&mut applied, Some(&repository), &shares);
        let sources: Vec<_> = applied
            .spec
            .sandbox
            .mounts
            .iter()
            .filter_map(|mount| match mount {
                agent::MountSpec::Bind { source, .. } => Some(source.clone()),
                agent::MountSpec::Tmpfs { .. } => None,
            })
            .collect();
        assert_eq!(sources, [worktree, data]);

        let fetched = Shares::default();
        let shown = listed(&agent, &manifest_folder, Some(&repository), &fetched).expect("listed");
        assert_eq!(shown.len(), 1, "only the data folder is shared without a checkout");
        apply(&mut agent, Some(&repository), &fetched);
        assert_eq!(agent.spec.sandbox.mounts.len(), 1);
    }

    #[test]
    fn a_folder_of_ones_own_is_refused_when_shared_already_or_landing_on_another() {
        let home = tempfile::tempdir().expect("a home");
        let project = home.path().join("code/project");
        let other = home.path().join("other/project");
        std::fs::create_dir_all(&project).expect("a project");
        std::fs::create_dir_all(&other).expect("another of that name");
        let protected = Protected {
            user_home: Some(home.path().to_path_buf()),
            inside: Vec::new(),
        };
        let project = project.canonicalize().expect("canonical");
        let target = super::working_directory_target("linux", &project).expect("a target");
        let shared = vec![super::Shared {
            index: 0,
            source: project.clone(),
            target: target.as_str().to_owned(),
            read_only: false,
        }];
        let answer = project.display().to_string();
        let error = check_added("linux", &shared, &protected, &answer).expect_err("shared already");
        assert!(error.contains("shared already"), "{error}");
        let answer = other.display().to_string();
        let error = check_added("linux", &shared, &protected, &answer).expect_err("same name");
        assert!(
            error.contains(&format!("another folder at {}", target.as_str())),
            "{error}"
        );
        assert!(check_added("linux", &[], &protected, &answer).is_ok());
        assert!(check_added("linux", &[], &protected, " ").is_err());
    }
}
