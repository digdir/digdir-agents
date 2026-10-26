//! Installing a skill on this machine, where its harnesses discover it.
//!
//! Harnesses read skills from a directory below the user's home or a repository's root, and
//! several share one, so a skill is copied once into each distinct directory the chosen harnesses
//! read. It is a copy rather than a link, so it works on every host and outlives its source.

use std::path::{Path, PathBuf};

use crate::{Error, Harness, harness::Skill};

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
    /// Something else is there, which the skill replaces whole.
    Replace,
    /// The skill is there with exactly these files.
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
            // Whatever cannot be read as a skill, such as a link, differs from this one.
            Ok(metadata) if metadata.is_dir() => {
                let field = directory.display().to_string();
                match Skill::read(skill.name().to_owned(), &directory, &field) {
                    Ok(existing) if existing == *skill => Change::None,
                    _ => Change::Replace,
                }
            }
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
            remove_if_present(&staging)?;
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
        std::fs::write(path, &file.contents)?;
    }
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

    fn files_below(directory: &Path) -> Vec<(String, String)> {
        let mut files = Vec::new();
        crate::local::files::walk_source(directory, "installed", |relative, path, is_dir| {
            if !is_dir {
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
    fn a_different_skill_of_the_same_name_is_replaced_whole() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let old = skill_in(temporary.path(), &[("SKILL.md", "old"), ("stale.md", "gone")]);
        let home = temporary.path().join("home");
        install(&old, &home, &[Harness::ALL[1]]);
        std::fs::remove_dir_all(temporary.path().join("source")).expect("remove source");
        let new = skill_in(temporary.path(), &[("SKILL.md", "new")]);

        let placements = install(&new, &home, &[Harness::ALL[1]]);

        assert_eq!(placements[0].change, Change::Replace);
        assert_eq!(
            files_below(&placements[0].directory),
            [("SKILL.md".into(), "new".into())]
        );
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
