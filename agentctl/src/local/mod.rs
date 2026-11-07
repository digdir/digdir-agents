//! Local daemon infrastructure and host integration.

pub(crate) mod files;
pub mod home;
pub mod process;
pub mod skills;

use std::path::{Path, PathBuf};

/// Returns the root of the Git repository holding `directory`, if any: the nearest folder with a `.git` entry, which a
/// linked worktree keeps as a file.
///
/// The home directory and the folders above it never count, even with a `.git`, as when someone versions their home
/// directory for dotfiles. Such a repository holds all of a person's folders, so counting it would find an agent in
/// `~/agents/<name>` from every folder in the home directory, and search the whole home directory for manifests.
///
/// Reads the filesystem, so it blocks.
#[must_use]
pub fn repository_root(directory: &Path) -> Option<&Path> {
    repository_root_with(directory, home::user_home_directory)
}

fn repository_root_with(directory: &Path, home: impl FnOnce() -> Option<PathBuf>) -> Option<&Path> {
    let root = directory.ancestors().find(|ancestor| ancestor.join(".git").exists())?;
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let holds_home = home().is_some_and(|home| canonical(&home).starts_with(canonical(root)));
    (!holds_home).then_some(root)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::repository_root_with;

    #[test]
    fn the_home_directory_and_the_folders_above_it_are_never_a_repository_root() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let home = temporary.path().join("home");
        let project = home.join("code/project");
        std::fs::create_dir_all(project.join(".git")).expect("project repository");
        std::fs::create_dir_all(project.join("src")).expect("project folder");
        std::fs::create_dir_all(home.join("notes")).expect("folder outside the project");
        let root = |directory: &Path| repository_root_with(directory, || Some(home.clone())).map(Path::to_path_buf);

        assert_eq!(root(&project.join("src")), Some(project.clone()));
        assert_eq!(root(&home.join("notes")), None);

        std::fs::create_dir_all(home.join(".git")).expect("dotfiles repository");
        assert_eq!(root(&home.join("notes")), None, "a repository at the home directory");
        assert_eq!(root(&home), None);
        assert_eq!(
            root(&project.join("src")),
            Some(project.clone()),
            "a repository inside it still counts"
        );

        std::fs::remove_dir(home.join(".git")).expect("no dotfiles repository");
        std::fs::create_dir_all(temporary.path().join(".git")).expect("repository above the home directory");
        assert_eq!(root(&home.join("notes")), None, "a repository above the home directory");
        assert_eq!(
            repository_root_with(&home.join("notes"), || None),
            Some(temporary.path()),
            "without a home directory any repository counts"
        );
    }
}
