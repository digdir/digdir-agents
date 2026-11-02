//! Installing a skill on this machine, where its harnesses discover it.
//!
//! Harnesses read skills from a directory below the user's home or a repository's root, and
//! several share one, so a skill is copied once into each distinct directory the chosen harnesses
//! read. It is a copy rather than a link, so it works on every host and outlives its source.
//!
//! Each copy holds a record of what was installed, so installing again can bring agentctl's own
//! copy up to date, while a skill it did not install, or one edited since, is only replaced when
//! asked to. The record sits in the skill itself, so it moves and goes with it, and anyone who
//! clones a repository sees the same.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    Error, Harness,
    harness::{INSTALL_RECORD, Skill, SkillFile},
};

/// What [`INSTALL_RECORD`] holds.
#[derive(Deserialize, Serialize)]
struct Record {
    /// [`digest`] of the files installed.
    digest: String,
}

/// A digest of a skill's files and their paths, to tell whether they changed. It leaves out
/// executable bits, which Windows does not have, so a copy in a repository installed on one host
/// still counts as unedited on another.
fn digest(files: &[SkillFile]) -> String {
    let mut digest = Sha256::new();
    for file in files {
        // Lengths keep one file's bytes from reading as another's path.
        for part in [file.relative_path.as_bytes(), &file.contents] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
    }
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _infallible = write!(output, "{byte:02x}");
            output
        })
}

/// The root of the repository holding `directory`: the nearest directory with a `.git`, which is
/// a file in a worktree.
#[must_use]
pub fn repository_root(directory: &Path) -> Option<PathBuf> {
    directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

/// What installing a skill does to one directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    /// Nothing is there yet.
    Install,
    /// agentctl's own copy is there, unchanged since, or the same files without a record of it.
    /// The skill replaces it whole, which loses nothing.
    Update,
    /// A skill agentctl did not install is there, or one edited since, or something else. The
    /// skill would replace it whole, so this needs the person's consent.
    Replace,
    /// The skill is there with exactly these files, as agentctl installed it.
    None,
}

/// One directory a skill goes into, and the harnesses that read it.
#[derive(Debug)]
pub struct Placement {
    /// The skill's own directory, named after it.
    pub directory: PathBuf,
    /// Every chosen harness that reads it.
    pub harnesses: Vec<Harness>,
    /// What installing does to it.
    pub change: Change,
}

/// The directories a skill goes into below `root`, the user's home or a repository's root, for
/// `harnesses`, and what installing it would change in each.
///
/// # Errors
///
/// Returns an error when an existing directory cannot be inspected.
pub fn plan(skill: &Skill, root: &Path, harnesses: &[Harness]) -> Result<Vec<Placement>, Error> {
    let mut placements: Vec<Placement> = Vec::new();
    for &harness in harnesses {
        let directory = root.join(harness.skills_directory()).join(skill.name());
        if let Some(placement) = placements.iter_mut().find(|placement| placement.directory == directory) {
            if !placement.harnesses.contains(&harness) {
                placement.harnesses.push(harness);
            }
            continue;
        }
        let change = match std::fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Change::Install,
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.is_dir() => compare(skill, &directory),
            // A link, say, is not a copy agentctl made.
            Ok(_) => Change::Replace,
        };
        placements.push(Placement {
            directory,
            harnesses: vec![harness],
            change,
        });
    }
    Ok(placements)
}

/// What installing `skill` does to the skill directory `directory`.
fn compare(skill: &Skill, directory: &Path) -> Change {
    let field = directory.display().to_string();
    // Whatever cannot be read as a skill differs from this one.
    let Ok(existing) = Skill::read(skill.name().to_owned(), directory, &field) else {
        return Change::Replace;
    };
    let recorded = std::fs::read(directory.join(INSTALL_RECORD))
        .ok()
        .and_then(|record| serde_json::from_slice::<Record>(&record).ok())
        .map(|record| record.digest);
    let installed = digest(&existing.files);
    match recorded {
        Some(recorded) if recorded == installed && existing == *skill => Change::None,
        Some(recorded) if recorded == installed => Change::Update,
        _ if existing == *skill => Change::Update,
        _ => Change::Replace,
    }
}

/// Makes the changes `placements` from [`plan`] describe.
///
/// Each skill directory is assembled next to where it goes and renamed into place, so a harness
/// that watches its skills never reads half of one.
///
/// # Errors
///
/// Returns an error when a directory cannot be written. Directories already changed stay changed.
pub fn apply(skill: &Skill, placements: &[Placement]) -> Result<(), Error> {
    for placement in placements {
        if placement.change == Change::None {
            continue;
        }
        let directory = &placement.directory;
        let parent = directory
            .parent()
            .ok_or_else(|| Error::Invalid(format!("{} has no parent directory", directory.display())))?;
        std::fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".{}.agentctl-{}", skill.name(), std::process::id()));
        remove_if_present(&staging)?;
        if let Err(error) = stage(skill, &staging).and_then(|()| swap_in(&staging, directory)) {
            // What went wrong matters more than a leftover the next install clears.
            let _ignored = remove_if_present(&staging);
            return Err(error);
        }
    }
    Ok(())
}

fn stage(skill: &Skill, staging: &Path) -> Result<(), Error> {
    std::fs::create_dir(staging)?;
    for file in &skill.files {
        let path = file
            .relative_path
            .split('/')
            .fold(staging.to_path_buf(), |path, component| path.join(component));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.contents)?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&path)?.permissions();
            // Runnable by whoever may read it, as `chmod +x` with the usual umask does.
            permissions.set_mode(permissions.mode() | (permissions.mode() & 0o444) >> 2);
            std::fs::set_permissions(&path, permissions)?;
        }
    }
    let record = Record {
        digest: digest(&skill.files),
    };
    std::fs::write(staging.join(INSTALL_RECORD), serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}

/// Renames `staging` to `directory`, moving what is there aside first: Windows cannot rename onto
/// an existing directory.
fn swap_in(staging: &Path, directory: &Path) -> Result<(), Error> {
    if std::fs::symlink_metadata(directory).is_err() {
        return Ok(std::fs::rename(staging, directory)?);
    }
    let mut aside = staging.as_os_str().to_owned();
    aside.push(".replaced");
    let aside = PathBuf::from(aside);
    remove_if_present(&aside)?;
    std::fs::rename(directory, &aside)?;
    if let Err(error) = std::fs::rename(staging, directory) {
        std::fs::rename(&aside, directory)?;
        return Err(error.into());
    }
    remove_if_present(&aside)
}

/// Removes a file, directory or link, without following a link.
fn remove_if_present(path: &Path) -> Result<(), Error> {
    let removed = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.is_file() => std::fs::remove_file(path),
        Ok(_) => std::fs::remove_dir_all(path),
    };
    Ok(removed?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn skill_in(root: &Path, files: &[(&str, &str)]) -> Skill {
        let source = root.join("source").join("review");
        for (relative, contents) in files {
            let path = source.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, contents).expect("file");
        }
        Skill::read("review".into(), &source, "source").expect("skill")
    }

    fn install(skill: &Skill, root: &Path, harnesses: &[Harness]) -> Vec<Placement> {
        let placements = plan(skill, root, harnesses).expect("plan");
        apply(skill, &placements).expect("apply");
        placements
    }

    /// The skill's own files below `directory`, without the install record.
    fn files_below(directory: &Path) -> Vec<(String, String)> {
        let mut files = Vec::new();
        crate::local::files::walk_source(directory, "installed", |relative, path, is_dir| {
            if !is_dir && relative != Path::new(INSTALL_RECORD) {
                let contents = std::fs::read_to_string(path).expect("contents");
                files.push((relative.to_string_lossy().replace('\\', "/"), contents));
            }
            Ok(())
        })
        .expect("walk");
        files.sort();
        files
    }

    #[test]
    fn harnesses_that_share_a_directory_get_one_copy() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let skill = skill_in(temporary.path(), &[("SKILL.md", "review"), ("scripts/run.sh", "echo")]);
        let home = temporary.path().join("home");

        let placements = install(&skill, &home, &Harness::ALL);

        let mut directories = Harness::ALL.map(Harness::skills_directory).to_vec();
        directories.dedup();
        assert!(
            directories.len() < Harness::ALL.len(),
            "some harnesses share a directory"
        );
        assert_eq!(placements.len(), directories.len());
        for placement in &placements {
            let readers = Harness::ALL
                .into_iter()
                .filter(|harness| home.join(harness.skills_directory()).join("review") == placement.directory)
                .collect::<Vec<_>>();
            assert_eq!(placement.harnesses, readers);
            assert_eq!(
                files_below(&placement.directory),
                [
                    ("SKILL.md".into(), "review".into()),
                    ("scripts/run.sh".into(), "echo".into())
                ]
            );
        }
        // Nothing is left beside the skills.
        let entries = std::fs::read_dir(home.join(Harness::ALL[0].skills_directory()))
            .expect("skills directory")
            .count();
        assert_eq!(entries, 1);
    }

    #[test]
    fn installing_the_same_skill_again_changes_nothing() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let skill = skill_in(temporary.path(), &[("SKILL.md", "review")]);
        let home = temporary.path().join("home");
        install(&skill, &home, &[Harness::ALL[0]]);

        let placements = plan(&skill, &home, &[Harness::ALL[0]]).expect("plan");

        assert_eq!(placements[0].change, Change::None);
    }

    #[test]
    fn an_installed_skill_is_updated_whole() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let old = skill_in(temporary.path(), &[("SKILL.md", "old"), ("stale.md", "gone")]);
        let home = temporary.path().join("home");
        install(&old, &home, &[Harness::ALL[1]]);
        std::fs::remove_dir_all(temporary.path().join("source")).expect("remove source");
        let new = skill_in(temporary.path(), &[("SKILL.md", "new")]);

        let placements = install(&new, &home, &[Harness::ALL[1]]);

        assert_eq!(placements[0].change, Change::Update);
        assert_eq!(
            files_below(&placements[0].directory),
            [("SKILL.md".into(), "new".into())]
        );
        assert_eq!(
            plan(&new, &home, &[Harness::ALL[1]]).expect("plan")[0].change,
            Change::None
        );
    }

    #[test]
    fn an_installed_skill_edited_since_is_only_replaced_when_asked() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let old = skill_in(temporary.path(), &[("SKILL.md", "old")]);
        let home = temporary.path().join("home");
        let placements = install(&old, &home, &[Harness::ALL[0]]);
        std::fs::write(placements[0].directory.join("notes.md"), "mine").expect("edit");
        std::fs::remove_dir_all(temporary.path().join("source")).expect("remove source");
        let new = skill_in(temporary.path(), &[("SKILL.md", "new")]);

        let placements = plan(&new, &home, &[Harness::ALL[0]]).expect("plan");

        assert_eq!(placements[0].change, Change::Replace);
    }

    #[test]
    fn a_skill_agentctl_did_not_install_is_only_replaced_when_asked_unless_it_is_the_same() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let skill = skill_in(temporary.path(), &[("SKILL.md", "review")]);
        let home = temporary.path().join("home");
        let existing = home.join(Harness::ALL[0].skills_directory()).join("review");
        std::fs::create_dir_all(&existing).expect("existing directory");

        for (contents, change) in [("theirs", Change::Replace), ("review", Change::Update)] {
            std::fs::write(existing.join("SKILL.md"), contents).expect("existing skill");
            let placements = plan(&skill, &home, &[Harness::ALL[0]]).expect("plan");
            assert_eq!(placements[0].change, change, "{contents}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_can_be_run_still_can_once_installed() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source").join("review").join("run.sh");
        let mode = |path: &Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o111;
        let old = skill_in(temporary.path(), &[("SKILL.md", "review"), ("run.sh", "echo")]);
        let home = temporary.path().join("home");
        let placements = install(&old, &home, &[Harness::ALL[0]]);
        let installed = placements[0].directory.join("run.sh");
        assert_eq!(mode(&installed), 0);

        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).expect("make runnable");
        let new = Skill::read("review".into(), source.parent().expect("skill"), "source").expect("skill");
        let placements = install(&new, &home, &[Harness::ALL[0]]);

        assert_eq!(placements[0].change, Change::Update);
        assert_ne!(mode(&installed), 0);
    }

    #[test]
    fn a_directory_without_a_skill_counts_as_different() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let skill = skill_in(temporary.path(), &[("SKILL.md", "review")]);
        let home = temporary.path().join("home");
        let existing = home.join(Harness::ALL[2].skills_directory()).join("review");
        std::fs::create_dir_all(&existing).expect("existing directory");

        let placements = plan(&skill, &home, &[Harness::ALL[2]]).expect("plan");

        assert_eq!(placements[0].change, Change::Replace);
    }

    #[test]
    fn the_repository_root_is_the_nearest_directory_with_git() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repository = temporary.path().join("repository");
        let nested = repository.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("nested directory");
        std::fs::create_dir(repository.join(".git")).expect("git directory");

        assert_eq!(repository_root(&nested), Some(repository.clone()));
        assert_eq!(repository_root(&repository), Some(repository));
    }
}
