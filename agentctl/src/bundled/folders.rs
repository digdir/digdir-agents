//! agentd's copies of the bundled definitions, one folder each, named after the definition.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{Error, control_plane::AgentStore};

use super::{Definition, Definitions};

/// agentd's copies of its bundled definitions, one folder each, named after the definition.
///
/// agentd writes them when it starts, so each holds the version this build carries, and an agent
/// made from one reads that version from then on, as an agent applied from a checkout reads what
/// the checkout holds after a pull.
#[derive(Clone, Debug)]
pub struct Folders {
    root: PathBuf,
}

impl Folders {
    /// Keeps the copies in `root`, which agentd creates when it first writes one.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The folder holding every copy.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder holding the copy of the definition named `name`.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Writes the copy of `definition`, replacing the one there, and returns its folder. agentd
    /// does so when it starts, before any request reads one.
    ///
    /// # Errors
    ///
    /// Returns an error when the copy cannot be written.
    pub async fn write(&self, definition: &Definition) -> Result<PathBuf, Error> {
        let (root, target, partial) = self.write_beside(definition).await?;
        let name = definition.name().to_owned();
        tokio::task::spawn_blocking(move || {
            let replaced = root.join(format!(".{name}.replaced"));
            let _ = std::fs::remove_dir_all(&replaced);
            match std::fs::rename(&target, &replaced) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            std::fs::rename(&partial, &target).inspect_err(|_| {
                let _ = std::fs::remove_dir_all(&partial);
            })?;
            let _ = std::fs::remove_dir_all(&replaced);
            Ok(target)
        })
        .await
        .map_err(|error| Error::Daemon(format!("writing a bundled definition failed: {error}")))?
        .map_err(Error::from)
    }

    /// Returns the folder of the copy of `definition`, writing it if it is missing, which it is
    /// only if someone removed it since agentd started. A copy another request writes first is
    /// kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the copy cannot be written.
    pub async fn ensure(&self, definition: &Definition) -> Result<PathBuf, Error> {
        let target = self.path(definition.name());
        if tokio::fs::try_exists(&target).await? {
            return Ok(target);
        }
        let (_, target, partial) = self.write_beside(definition).await?;
        tokio::task::spawn_blocking(move || {
            if let Err(error) = std::fs::rename(&partial, &target) {
                let _ = std::fs::remove_dir_all(&partial);
                if !target.is_dir() {
                    return Err(error);
                }
            }
            Ok(target)
        })
        .await
        .map_err(|error| Error::Daemon(format!("writing a bundled definition failed: {error}")))?
        .map_err(Error::from)
    }

    /// Writes the files of `definition` to a new folder beside its copy's, returning the root,
    /// the copy's folder and the new one.
    async fn write_beside(&self, definition: &Definition) -> Result<(PathBuf, PathBuf, PathBuf), Error> {
        let root = self.root.clone();
        let target = self.path(definition.name());
        let files = definition.files().clone();
        let name = definition.name().to_owned();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            let partial = tempfile::Builder::new()
                .prefix(&format!(".{name}."))
                .tempdir_in(&root)?;
            for (path, file) in &files {
                let destination = partial.path().join(path);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&destination, &file.contents)?;
                // Windows has no executable bit.
                #[cfg(unix)]
                set_executable(&destination, file.executable)?;
            }
            Ok::<_, std::io::Error>((root, target, partial.keep()))
        })
        .await
        .map_err(|error| Error::Daemon(format!("writing a bundled definition failed: {error}")))?
        .map_err(Error::from)
    }

    /// Removes the copies of definitions this build does not carry that no agent was made from,
    /// and anything else left in the folder, such as a copy agentd did not finish writing.
    ///
    /// # Errors
    ///
    /// Returns an error when the agents cannot be read or a copy cannot be removed.
    pub async fn remove_unused(&self, carried: &Definitions, agents: &dyn AgentStore) -> Result<(), Error> {
        let kept = agents
            .list()
            .await?
            .into_iter()
            .filter_map(|record| {
                let relative = record.source_directory.strip_prefix(&self.root).ok()?;
                relative.components().next().map(|first| first.as_os_str().to_owned())
            })
            .chain(carried.iter().map(|definition| OsString::from(definition.name())))
            .collect::<BTreeSet<_>>();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || remove_unused(&root, &kept))
            .await
            .map_err(|error| Error::Daemon(format!("removing unused bundled definitions failed: {error}")))?
            .map_err(Error::from)
    }
}

fn remove_unused(root: &Path, kept: &BTreeSet<OsString>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if kept.contains(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        tracing::debug!(path = %path.display(), "removed an unused bundled definition");
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if executable {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
