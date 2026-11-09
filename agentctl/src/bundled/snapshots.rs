//! agentd's copies of the bundled definitions its agents use, one folder per digest.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::{Error, Origin, control_plane::AgentStore};

use super::Definition;

/// agentd's copies of its bundled definitions, a folder each, named by digest.
///
/// Each is written once and never changed, so an agent reads the definition it was made from for
/// as long as it lives, whichever build of agentd runs it. A copy goes once no agent uses it and
/// this build no longer carries it.
///
/// Like volume revisions, writing one and recording the agent that uses it race cleanup, so both
/// hold [`Self::lock`].
#[derive(Clone)]
pub struct Snapshots {
    root: PathBuf,
    lock: Rc<tokio::sync::Mutex<()>>,
    /// The digests of the definitions this build carries, whose copies stay.
    carried: Rc<BTreeSet<String>>,
}

impl Snapshots {
    /// Keeps the copies in `root`, which agentd creates when it first needs it.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Rc::default(),
            carried: Rc::default(),
        }
    }

    /// Keeps the copies of `definitions`, which this build carries, even while no agent uses them.
    #[must_use]
    pub fn carrying(mut self, definitions: &super::Definitions) -> Self {
        self.carried = Rc::new(
            definitions
                .iter()
                .map(|definition| definition.digest().to_owned())
                .collect(),
        );
        self
    }

    /// The folder holding the copy of the definition with `digest`.
    #[must_use]
    pub fn path(&self, digest: &str) -> PathBuf {
        self.root.join(digest)
    }

    /// Holds off cleanup until the guard is dropped. Hold it from [`Self::write`] until the agent
    /// using the copy is stored.
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// Writes the copy of `definition` unless it is there already, and returns its folder.
    ///
    /// # Errors
    ///
    /// Returns an error when the copy cannot be written.
    pub async fn write(&self, definition: &Definition) -> Result<PathBuf, Error> {
        let target = self.path(definition.digest());
        if tokio::fs::try_exists(&target).await? {
            return Ok(target);
        }
        let root = self.root.clone();
        let files = definition.files().clone();
        let digest = definition.digest().to_owned();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            // Written beside its final place and renamed into it, so a copy is either whole or absent.
            let partial = tempfile::Builder::new()
                .prefix(&format!(".{digest}."))
                .tempdir_in(&root)?;
            for (path, file) in &files {
                let destination = partial.path().join(path);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&destination, &file.contents)?;
                set_executable(&destination, file.executable)?;
            }
            let partial = partial.keep();
            std::fs::rename(&partial, &target).inspect_err(|_| {
                let _ = std::fs::remove_dir_all(&partial);
            })?;
            Ok::<_, std::io::Error>(target)
        })
        .await
        .map_err(|error| Error::Daemon(format!("writing a bundled definition failed: {error}")))?
        .map_err(Error::from)
    }

    /// Removes every copy that no agent in `agents` uses and that this build does not carry, and
    /// anything else left in the folder, such as a copy a stopped agentd did not finish writing.
    ///
    /// # Errors
    ///
    /// Returns an error when the agents cannot be read or a copy cannot be removed.
    pub async fn remove_unused(&self, agents: &dyn AgentStore) -> Result<(), Error> {
        let _guard = self.lock().await;
        let used = agents
            .list()
            .await?
            .into_iter()
            .filter_map(|record| match record.origin {
                Origin::Bundled { digest, .. } => Some(digest),
                Origin::Local => None,
            })
            .chain(self.carried.iter().cloned())
            .collect::<BTreeSet<_>>();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || remove_unused(&root, &used))
            .await
            .map_err(|error| Error::Daemon(format!("removing unused bundled definitions failed: {error}")))?
            .map_err(Error::from)
    }
}

fn remove_unused(root: &Path, used: &BTreeSet<String>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(|name| used.contains(name)) {
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

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}
