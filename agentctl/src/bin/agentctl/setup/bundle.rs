//! Copies what a manifest reads from its directory into the agent's own folder, so the folder
//! stands alone: the source it came from, such as a temporary checkout, can go.

use std::path::{Path, PathBuf};

use agent::{Agent, Error, MountSpec, manifest::resolve_host_path};
use sandbox::image::ImageSource;

use super::{Source, mounts::Protected};

/// Files never copied, wherever they sit: their values belong to the person, not the agent.
const NEVER_COPIED: &str = ".env";

/// Checks every file [`bundle`] would copy from `source` into the agent's folder, before anything
/// is written, since the agent sees all of them. A definition may read what its repository holds.
/// What it reads from elsewhere on this machine, links followed, must pass the check a shared
/// folder does: a definition fetched from GitHub is no more trusted than a folder typed in.
pub(super) fn check_sources(agent: &Agent, source: &Source, protected: &Protected) -> Result<(), Error> {
    let resolve = |path: &Path, field: String| {
        resolve_host_path(&source.directory, path, &field).map(|resolved| (field, resolved))
    };
    let mut sources = vec![(resolve(&agent.spec.home.source, "spec.home.source".to_owned())?, false)];
    for (index, spec) in agent.spec.instructions.iter().enumerate() {
        sources.push((
            resolve(&spec.source, format!("spec.instructions[{index}].source"))?,
            spec.optional,
        ));
    }
    for (index, spec) in agent.spec.skills.iter().enumerate() {
        sources.push((
            resolve(&spec.source, format!("spec.skills[{index}].source"))?,
            spec.optional,
        ));
    }
    let root = own_root(source, protected);
    for ((field, path), optional) in sources {
        if optional && !path.exists() {
            continue;
        }
        check_tree(&path, root.as_deref(), protected, &mut Vec::new()).map_err(|reason| {
            Error::Invalid(format!(
                "{field} copies {} into the agent, but {reason}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

/// Where the definition's own files are, in canonical form: its repository, else its folder. One
/// that holds the home folder, such as a home kept in Git, holds everything and so counts as none.
fn own_root(source: &Source, protected: &Protected) -> Option<PathBuf> {
    let home = protected.user_home.as_deref().and_then(|home| home.canonicalize().ok());
    source
        .repository
        .as_ref()
        .map(|repository| repository.root.as_path())
        .into_iter()
        .chain([source.directory.as_path()])
        .filter_map(|root| root.canonicalize().ok())
        .find(|root| !home.as_ref().is_some_and(|home| home.starts_with(root)))
}

/// Checks `path` and, for a folder, everything in it as [`copy_tree`] reaches it: inside `root`,
/// or allowed by `protected`. `holding` are the folders being checked that hold `path`, so a link
/// back to one of them is refused rather than followed forever.
fn check_tree(
    path: &Path,
    root: Option<&Path>,
    protected: &Protected,
    holding: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("it cannot be read: {error}"))?;
    if !root.is_some_and(|root| canonical.starts_with(root)) {
        protected.check_path(&canonical)?;
    }
    if !canonical.is_dir() {
        return Ok(());
    }
    if holding.contains(&canonical) {
        return Err(format!("{} links back to a folder that holds it", path.display()));
    }
    holding.push(canonical);
    let entries = std::fs::read_dir(path).map_err(|error| format!("it cannot be read: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("it cannot be read: {error}"))?;
        if !entry.file_name().eq_ignore_ascii_case(NEVER_COPIED) {
            check_tree(&entry.path(), root, protected, holding)?;
        }
    }
    holding.pop();
    Ok(())
}

/// Copies the home directory, instructions and skills `agent` reads, resolved against
/// `source_directory`, into `destination`, and points the manifest at the copies:
///
/// - the home directory to `home/`
/// - one instruction file to `instructions.md`, several to `instructions/NN-<name>` in order
/// - each skill to `skills/<name>/`, keeping its name
///
/// Optional sources that are missing are dropped. Bind mount sources and a build context are
/// made absolute, since they stay where they are. Returns the paths written, relative to
/// `destination`.
pub(super) fn bundle(agent: &mut Agent, source_directory: &Path, destination: &Path) -> Result<Vec<String>, Error> {
    let resolve = |path: &Path, field: &str| resolve_host_path(source_directory, path, field);
    let mut written = Vec::new();

    let home = resolve(&agent.spec.home.source, "spec.home.source")?;
    copy_tree(&home, &destination.join("home"))?;
    agent.spec.home.source = PathBuf::from("home");
    written.push("home/".to_owned());

    let mut instructions = Vec::new();
    for (index, spec) in agent.spec.instructions.iter().enumerate() {
        let path = resolve(&spec.source, &format!("spec.instructions[{index}].source"))?;
        if spec.optional && !path.exists() {
            continue;
        }
        instructions.push((spec.clone(), path));
    }
    let single = instructions.len() == 1;
    agent.spec.instructions = instructions
        .into_iter()
        .enumerate()
        .map(|(index, (mut spec, path))| {
            let relative = if single {
                "instructions.md".to_owned()
            } else {
                format!("instructions/{:02}-{}", index + 1, file_name(&path))
            };
            copy_file(&path, &destination.join(&relative))?;
            spec.source = PathBuf::from(&relative);
            written.push(relative);
            Ok(spec)
        })
        .collect::<Result<_, Error>>()?;

    let mut skills = Vec::new();
    for (index, mut spec) in std::mem::take(&mut agent.spec.skills).into_iter().enumerate() {
        let path = resolve(&spec.source, &format!("spec.skills[{index}].source"))?;
        if spec.optional && !path.exists() {
            continue;
        }
        let name = spec
            .name()
            .ok_or_else(|| Error::Invalid(format!("spec.skills[{index}] has no name")))?
            .to_owned();
        let relative = format!("skills/{name}");
        if written.contains(&format!("{relative}/")) {
            return Err(Error::Invalid(format!(
                "two skills are named {name:?}; give one a different `name`"
            )));
        }
        copy_tree(&path, &destination.join(&relative))?;
        spec.source = PathBuf::from(&relative);
        written.push(format!("{relative}/"));
        skills.push(spec);
    }
    agent.spec.skills = skills;

    for (index, mount) in agent.spec.sandbox.mounts.iter_mut().enumerate() {
        if let MountSpec::Bind { source, .. } = mount {
            *source = resolve(source, &format!("spec.sandbox.mounts[{index}].source"))?;
        }
    }
    if let ImageSource::Build { context, .. } = &mut agent.spec.sandbox.image {
        *context = resolve(context, "spec.sandbox.image.context")?;
    }
    Ok(written)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "instructions.md".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), Error> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination)
        .map_err(|error| Error::Invalid(format!("cannot copy {}: {error}", source.display())))?;
    Ok(())
}

/// Copies a directory tree, following links to their contents, which [`check_sources`] allowed,
/// and leaving out `.env` files.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(destination)?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| Error::Invalid(format!("cannot read {}: {error}", source.display())))?;
    for entry in entries {
        let entry = entry?;
        if entry.file_name().eq_ignore_ascii_case(NEVER_COPIED) {
            continue;
        }
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use super::{bundle, check_sources};
    use crate::setup::{mounts::Protected, source::team_draft};

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        std::fs::write(path, contents).expect("a file");
    }

    /// Altinn's layout: the manifest in `agents/minimal/` reads `../home`, `../instructions.md`
    /// and `../_skills/*` beside it.
    #[test]
    fn shared_files_beside_the_manifest_folder_are_flattened_into_the_agent_folder() {
        let source = tempfile::tempdir().expect("a source");
        let root = source.path();
        write(&root.join("home/.claude/settings.json"), "{}");
        write(&root.join("home/.env"), "SECRET=1");
        write(&root.join("instructions.md"), "shared");
        write(&root.join("minimal/environment.md"), "minimal");
        write(&root.join("_skills/app-development/SKILL.md"), "app");
        write(&root.join("_skills/changelog/SKILL.md"), "changelog");
        let manifest = r"
apiVersion: agents.platform/v1alpha1
kind: Agent
metadata: {name: altinn-minimal}
spec:
  sandbox:
    image: {type: reference, reference: example.com/agent:latest}
    platform: {os: linux}
    resources: {cpu: '4', memory: 8Gi, rootFilesystem: {capacity: 64Gi, mode: layered}}
  home: {source: ../home}
  instructions:
    - source: ../instructions.md
    - source: environment.md
    - {source: missing.md, optional: true}
  skills:
    - {source: ../_skills/app-development, name: altinn-studio-app-development}
    - source: ../_skills/changelog
  harnesses: [{type: pi}]
  network: {mode: mediated, allow: all}
";
        write(&root.join("minimal/agent.yaml"), manifest);
        let mut agent = agent::manifest::resolve(&root.join("minimal/agent.yaml"))
            .expect("the manifest resolves")
            .agent;

        let destination = tempfile::tempdir().expect("a destination");
        let written = bundle(&mut agent, &root.join("minimal"), destination.path()).expect("bundled");

        assert_eq!(
            written,
            [
                "home/",
                "instructions/01-instructions.md",
                "instructions/02-environment.md",
                "skills/altinn-studio-app-development/",
                "skills/changelog/"
            ]
        );
        assert_eq!(agent.spec.home.source, PathBuf::from("home"));
        assert_eq!(agent.spec.instructions.len(), 2);
        assert_eq!(agent.spec.skills[0].name(), Some("altinn-studio-app-development"));
        assert_eq!(agent.spec.skills[1].name(), Some("changelog"));
        let out = destination.path();
        assert!(out.join("home/.claude/settings.json").is_file());
        assert!(!out.join("home/.env").exists(), ".env files are never copied");
        assert!(out.join("skills/altinn-studio-app-development/SKILL.md").is_file());

        let written_manifest = serde_yaml_ng::to_string(&agent).expect("serializes");
        std::fs::write(out.join("agent.yaml"), written_manifest).expect("written");
        agent::manifest::resolve(&out.join("agent.yaml")).expect("the bundled manifest resolves on its own");
    }

    /// A machine whose home holds a team's checkout, private folders, an agents folder and a file
    /// of the person's own instructions.
    struct Machine {
        home: tempfile::TempDir,
    }

    impl Machine {
        fn new() -> Self {
            let machine = Self {
                home: tempfile::tempdir().expect("a home"),
            };
            let home = machine.home.path();
            std::fs::create_dir_all(home.join("code/team/.git")).expect("a checkout");
            write(&home.join("code/team/home/.claude/settings.json"), "{}");
            write(&home.join("code/team/_skills/review/SKILL.md"), "review");
            write(&home.join("code/team/agents/x/instructions.md"), "team");
            write(&home.join(".ssh/id_ed25519"), "a private key");
            write(&home.join("Library/Keychains/login.keychain-db"), "a keychain");
            write(&home.join("agents/other/.env"), "TOKEN=x");
            write(&home.join("agent-instructions/everywhere.md"), "mine");
            machine
        }

        fn path(&self, below: &str) -> String {
            self.home.path().join(below).display().to_string()
        }

        /// Checks the definition in `code/team/agents/x`, or `folder` when given, reading `home`,
        /// `instructions` and `skill`.
        fn check(&self, folder: Option<&str>, home: &str, instructions: &str, skill: &str) -> Result<(), String> {
            let folder = self.home.path().join(folder.unwrap_or("code/team/agents/x"));
            let manifest = format!(
                "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {{name: x}}\nspec:\n  sandbox:\n    \
                 image: {{type: reference, reference: example.com/agent:latest}}\n    platform: {{os: linux}}\n    \
                 resources: {{cpu: '2', memory: 4Gi, rootFilesystem: {{capacity: 20Gi, mode: layered}}}}\n  \
                 home: {{source: {home:?}}}\n  instructions:\n    - source: {instructions:?}\n    - {{source: \
                 missing.md, optional: true}}\n  skills:\n    - {{source: {skill:?}, name: review}}\n  harnesses: \
                 [{{type: pi}}]\n  network: {{mode: mediated, allow: all}}\n"
            );
            write(&folder.join("agent.yaml"), &manifest);
            let draft = team_draft(&folder.join("agent.yaml"), "a test").expect("a draft");
            let protected = Protected {
                user_home: Some(self.home.path().to_path_buf()),
                inside: vec![self.home.path().join("agents")],
            };
            check_sources(&draft.agent, &draft.source, &protected).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn a_definition_copies_in_its_repository_and_what_this_machine_allows_but_nothing_private() {
        let machine = Machine::new();
        let everywhere = machine.path("agent-instructions/everywhere.md");
        assert_eq!(
            machine.check(None, "../../home", &everywhere, "../../_skills/review"),
            Ok(()),
            "its repository and the person's own instructions"
        );
        for (home, skill, private) in [
            (machine.path(".ssh"), "../../_skills/review".to_owned(), ".ssh"),
            ("../../home".to_owned(), machine.path("Library/Keychains"), "Library"),
            (
                "../../../../agents/other".to_owned(),
                "../../_skills/review".to_owned(),
                "agents",
            ),
        ] {
            let error = machine
                .check(None, &home, "instructions.md", &skill)
                .expect_err("refused");
            assert!(error.contains(private), "{error}");
        }
    }

    #[test]
    fn a_home_kept_in_git_is_not_a_definitions_repository() {
        let machine = Machine::new();
        std::fs::create_dir_all(machine.home.path().join(".git")).expect("a home in Git");
        write(&machine.home.path().join("defs/x/home/notes.md"), "notes");
        assert_eq!(machine.check(Some("defs/x"), "home", "home/notes.md", "home"), Ok(()));
        let error = machine
            .check(Some("defs/x"), "../../.ssh", "home/notes.md", "home")
            .expect_err("refused");
        assert!(error.contains(".ssh"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_followed_only_where_a_definition_may_read() {
        let machine = Machine::new();
        let home = machine.home.path().join("code/team/home");
        std::os::unix::fs::symlink(machine.home.path().join("code/team/_skills"), home.join("skills"))
            .expect("a link within the repository");
        assert_eq!(
            machine.check(None, "../../home", "instructions.md", "../../_skills/review"),
            Ok(())
        );

        std::os::unix::fs::symlink(machine.home.path().join(".ssh"), home.join("keys")).expect("a link out");
        let error = machine
            .check(None, "../../home", "instructions.md", "../../_skills/review")
            .expect_err("refused");
        assert!(error.contains(".ssh"), "{error}");
        std::fs::remove_file(home.join("keys")).expect("the link removed");

        std::os::unix::fs::symlink(&home, home.join("loop")).expect("a loop");
        let error = machine
            .check(None, "../../home", "instructions.md", "../../_skills/review")
            .expect_err("refused");
        assert!(error.contains("links back"), "{error}");
    }
}
