//! What of this machine an agent gets: the folders it shares with the person, those of its
//! definition and one the person chooses, where a folder appears in the sandbox, the files its
//! definition reads, and what none of these may be. Also the agent definitions a folder holds.

use super::answers::{Answers, Choice};
use super::{Draft, SetupArguments};
use crate::{CommandError, CommandResult};
use agent::local::home::ControlPlaneHome;

use std::path::{Component, Path, PathBuf};

use agent::{Agent, Error, MountSpec, manifest};
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
pub(super) fn protected_folders(home: &ControlPlaneHome) -> Protected {
    Protected {
        user_home: agent::local::home::user_home_directory(),
        inside: vec![home.path().to_path_buf()],
    }
}

/// What the agent shares, as decided so far: the mounts of its definition left out, by their
/// place and by where the agent sees them.
#[derive(Default)]
struct Shares {
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
fn listed(agent: &Agent, directory: &Path, shares: &Shares) -> Result<Vec<Shared>, Error> {
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
        let source = manifest::resolve_host_path(directory, source, "spec.sandbox.mounts")?;
        let source = source.canonicalize().unwrap_or(source);
        listed.push(Shared {
            index,
            source,
            target: target.as_str().to_owned(),
            read_only: *read_only,
        });
    }
    Ok(listed)
}

/// Removes from `agent` the mounts `shares` leaves out. The others stay as its definition has them,
/// so the person's variant restates them only when one is left out.
fn apply(agent: &mut Agent, shares: &Shares) {
    let mut removed = shares.left_out.clone();
    removed.sort_unstable();
    removed.dedup();
    for index in removed.into_iter().rev() {
        agent.spec.sandbox.mounts.remove(index);
    }
}

/// Decides which folders on this machine the agent shares with the person, read and written by
/// both, and applies it: mounts of its definition left out, and a folder of the person's own. A
/// relative mount of the definition's own repository, such as `../..`, shares the checkout the
/// person's variant is in. The flags decide, and what they leave open is asked, or left as the
/// definition has it.
pub(super) fn choose_shared(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    protected: &Protected,
) -> CommandResult<()> {
    let ask = answers.interactive() && !arguments.yes;
    let mut shares = Shares::default();
    for target in &arguments.unshare {
        let listed = listed(&draft.agent, &draft.source.directory, &shares)?;
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
            let listed = listed(&draft.agent, &draft.source.directory, &shares)?;
            Some(check_added(&os, &listed, protected, &folder.display().to_string()).map_err(Error::Invalid)?)
        }
        None if ask => ask_shares(answers, draft, &mut shares, protected)?,
        None => None,
    };
    apply(&mut draft.agent, &shares);
    if let Some(folder) = &added {
        mount_unless_mounted(draft, folder)?;
    }
    Ok(())
}

/// What the person does with the folders the agent shares.
#[derive(Clone, Copy)]
enum Change {
    Keep,
    Add,
    LeaveOut,
}

/// Shows the folders the agent shares and lets the person change them, until they keep them or
/// share a folder of their own, which it returns.
fn ask_shares(
    answers: &Answers,
    draft: &Draft,
    shares: &mut Shares,
    protected: &Protected,
) -> CommandResult<Option<PathBuf>> {
    let os = &draft.agent.spec.sandbox.platform.os;
    loop {
        let listed = listed(&draft.agent, &draft.source.directory, shares)?;
        if listed.is_empty() {
            let lines = [
                "You and the agent then work on the same files, and what it changes lands in your folder.",
                "Without one, it works on its own copies, which you reach over SSH.",
                "Share a folder on this machine with the agent?",
            ];
            let share = answers.choose(
                "--mount",
                &lines.join("\n"),
                vec![
                    Choice::new(false, "No, it works on its own copies"),
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
        let mut choices = vec![Choice::new(Change::Keep, "Keep them")];
        choices.push(Choice::new(Change::Add, "Share another folder too"));
        choices.push(Choice::new(Change::LeaveOut, "Leave one out"));
        match answers.choose("--mount", &lines.join("\n"), choices, 0)? {
            Change::Keep => return Ok(None),
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
            // Where the agent is applied from, as a download moves.
            let source = manifest::resolve_host_path(&draft.final_directory(), source, "spec.sandbox.mounts")?;
            Ok((source.canonicalize().unwrap_or(source), target.as_str().to_owned()))
        })
        .collect()
}

/// Mounts `folder` in the agent's working directory, unless the agent mounts it already.
fn mount_unless_mounted(draft: &mut Draft, folder: &Path) -> CommandResult<()> {
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

/// Checks every folder the agent mounts before anything is written, those its definition names as
/// well as those chosen here, since a definition cloned from GitHub is no more trusted than a
/// folder typed in. agentd does not know the folders `protected` keeps out of agents.
pub(super) fn check_mounts(draft: &Draft, protected: &Protected) -> CommandResult<()> {
    for (source, target) in bind_mounts(draft)? {
        // A dry run's stand-in for a clone not made yet has nothing there to check.
        let not_made = draft
            .source
            .copy
            .as_ref()
            .is_some_and(|copy| !copy.kept.exists() && source.starts_with(&copy.kept));
        if not_made {
            continue;
        }
        check_mount(&source, &target, protected).map_err(CommandError::Message)?;
    }
    Ok(draft.agent.validate()?)
}

/// Checks what the agent's definition gives it from this machine besides its folders: its home,
/// instructions and skills. What lies in its repository is its own. Anything else must pass the
/// check a shared folder does, so a definition cloned from GitHub cannot hand the agent `~/.ssh`.
/// agentd refuses links inside them itself.
pub(super) fn check_sources(draft: &Draft, protected: &Protected) -> CommandResult<()> {
    let spec = &draft.agent.spec;
    let mut sources = vec![("spec.home.source".to_owned(), &spec.home.source, false)];
    for (index, instruction) in spec.instructions.iter().enumerate() {
        sources.push((
            format!("spec.instructions[{index}].source"),
            &instruction.source,
            instruction.optional,
        ));
    }
    for (index, skill) in spec.skills.iter().enumerate() {
        sources.push((format!("spec.skills[{index}].source"), &skill.source, skill.optional));
    }
    let directory = &draft.source.directory;
    // A download has no `.git` that tells where its repository starts.
    let root = draft
        .source
        .copy
        .as_ref()
        .map_or_else(|| own_root(directory, protected), |copy| copy.root.canonicalize().ok());
    for (field, source, optional) in sources {
        let resolved = manifest::resolve_host_path(directory, source, &field)?;
        if optional && !resolved.exists() {
            continue;
        }
        let inside = resolved
            .canonicalize()
            .is_ok_and(|canonical| root.as_ref().is_some_and(|root| canonical.starts_with(root)));
        if !inside {
            protected.check_path(&resolved).map_err(|reason| {
                CommandError::Message(format!("{field} gives the agent {}, but {reason}", resolved.display()))
            })?;
        }
    }
    Ok(())
}

/// Where a definition in `directory` keeps its own files: its repository, else its folder. One
/// that holds the home folder, such as a home kept in Git, holds everything and so counts as none.
fn own_root(directory: &Path, protected: &Protected) -> Option<PathBuf> {
    let home = protected.user_home.as_deref().and_then(|home| home.canonicalize().ok());
    let directory = directory.canonicalize().ok()?;
    crate::discovery::repository_root(&directory)
        .into_iter()
        .chain([directory.as_path()])
        .find(|root| !home.as_ref().is_some_and(|home| home.starts_with(root)))
        .map(Path::to_path_buf)
}

/// Why the agent may not mount `source` at `target`: it must be a folder here, and not one
/// `protected` keeps out of agents. agentd refuses a folder holding a `.env` itself.
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
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fmt::Write as _;

    use super::{Protected, Shares, apply, check_added, check_mount, check_sources, listed};
    use crate::setup::source::team_draft;

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
    fn leaving_a_mount_out_keeps_the_others_as_the_definition_has_them() {
        let checkout = tempfile::tempdir().expect("a checkout");
        let folder = checkout.path().join("agents/desktop");
        std::fs::create_dir_all(&folder).expect("folders");
        std::fs::create_dir_all(checkout.path().join("data")).expect("a data folder");
        let mut agent = agent_mounting(&["../..", "../../data"]);
        let shares = Shares {
            left_out: vec![1],
            unshared: vec!["/srv/m1".into()],
        };

        let kept = listed(&agent, &folder, &shares).expect("listed");
        apply(&mut agent, &shares);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].source, checkout.path().canonicalize().expect("canonical"));
        assert_eq!(
            agent.spec.sandbox.mounts,
            agent_mounting(&["../.."]).spec.sandbox.mounts,
            "a relative mount shares the checkout the variant is in, so it stays relative"
        );
    }

    /// A home holding a team's checkout, private folders, an agents folder and a file of the
    /// person's own instructions.
    fn machine() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("a home");
        for (path, contents) in [
            ("code/team/home/.claude/settings.json", "{}"),
            ("code/team/_skills/review/SKILL.md", "review"),
            ("code/team/agents/x/instructions.md", "team"),
            ("defs/x/home/notes.md", "notes"),
            (".ssh/id_ed25519", "a private key"),
            ("Library/Keychains/login.keychain-db", "a keychain"),
            ("agents/other/notes.md", "another agent"),
            ("agent-instructions/everywhere.md", "mine"),
        ] {
            let path = home.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("folders");
            std::fs::write(path, contents).expect("a file");
        }
        std::fs::create_dir_all(home.path().join("code/team/.git")).expect("a checkout");
        home
    }

    /// Checks the definition in `folder` below `home`, reading `home_source`, `instructions` and
    /// `skill`.
    fn check(
        home: &std::path::Path,
        folder: &str,
        home_source: &str,
        instructions: &str,
        skill: &str,
    ) -> Result<(), String> {
        let folder = home.join(folder);
        let manifest = format!(
            "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {{name: x}}\nspec:\n  sandbox:\n    \
             image: {{type: reference, reference: example.com/agent:latest}}\n    platform: {{os: linux}}\n    \
             resources: {{cpu: '2', memory: 4Gi, rootFilesystem: {{capacity: 20Gi, mode: layered}}}}\n  \
             home: {{source: {home_source:?}}}\n  instructions:\n    - source: {instructions:?}\n    - {{source: \
             missing.md, optional: true}}\n  skills:\n    - {{source: {skill:?}, name: review}}\n  harnesses: \
             [{{type: pi}}]\n  network: {{mode: mediated, allow: all}}\n"
        );
        std::fs::write(folder.join("agent.yaml"), manifest).expect("a manifest");
        let save_as = "mine".parse().expect("a variant name");
        let draft = team_draft(&folder.join("agent.yaml"), "a test", &save_as).expect("a draft");
        let protected = Protected {
            user_home: Some(home.to_path_buf()),
            inside: vec![home.join("agents")],
        };
        check_sources(&draft, &protected).map_err(|error| error.to_string())
    }

    #[test]
    fn a_definition_gives_its_repository_and_what_this_machine_allows_but_nothing_private() {
        let home = machine();
        let home = home.path();
        let everywhere = home.join("agent-instructions/everywhere.md").display().to_string();
        let team = "code/team/agents/x";
        assert_eq!(
            check(home, team, "../../home", &everywhere, "../../_skills/review"),
            Ok(())
        );
        for (home_source, skill, private) in [
            (
                home.join(".ssh").display().to_string(),
                "../../_skills/review".to_owned(),
                ".ssh",
            ),
            (
                "../../home".to_owned(),
                home.join("Library/Keychains").display().to_string(),
                "Library",
            ),
            (
                "../../../../agents/other".to_owned(),
                "../../_skills/review".to_owned(),
                "agents",
            ),
        ] {
            let error = check(home, team, &home_source, "instructions.md", &skill).expect_err("refused");
            assert!(error.contains(private), "{error}");
        }
    }

    #[test]
    fn a_home_kept_in_git_is_not_a_definitions_repository() {
        let home = machine();
        let home = home.path();
        std::fs::create_dir_all(home.join(".git")).expect("a home in Git");
        assert_eq!(check(home, "defs/x", "home", "home/notes.md", "home"), Ok(()));
        let error = check(home, "defs/x", "../../.ssh", "home/notes.md", "home").expect_err("refused");
        assert!(error.contains(".ssh"), "{error}");
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
