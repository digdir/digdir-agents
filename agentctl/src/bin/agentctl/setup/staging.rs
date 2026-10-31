//! Where one setup run stages what it copies from: an extracted standard agent, a fetched
//! checkout, the sign-in agent's empty home. It sits below the control-plane home, never in a
//! folder a person mounts, and goes when the run ends.
//!
//! A run that is killed, such as with Ctrl-C at a question, cannot remove its folder or delete its
//! sign-in agent. Each run therefore holds a lock on its folder for as long as it lives, and names
//! its sign-in agent there, so the next run knows a dead run's leftovers from a live one's and
//! removes them.

use std::{fs::File, path::Path};

use agent::Error;

const PREFIX: &str = "setup-";
/// Locked for as long as the run that made the folder lives; the operating system lets go of it
/// when that process ends, however it ends.
const LOCK_FILE: &str = ".lock";
/// The sign-in agent the run started, for a later run to delete if this one could not.
const SIGN_IN_AGENT_FILE: &str = ".sign-in-agent";

/// One run's staging folder, removed when it is dropped.
pub(super) struct Staging {
    // Held only to keep the lock; dropped first, so that the folder can be removed on Windows,
    // where an open file pins it.
    _lock: File,
    directory: tempfile::TempDir,
}

impl Staging {
    /// Makes a new staging folder below `home` and locks it for this run.
    pub(super) fn create(home: &Path) -> Result<Self, Error> {
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(home)?;
        let lock = File::create(directory.path().join(LOCK_FILE))?;
        lock.try_lock()
            .map_err(|error| Error::Invalid(format!("could not lock the setup folder: {error}")))?;
        Ok(Self { _lock: lock, directory })
    }

    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Records the sign-in agent this run starts, before it is created.
    pub(super) fn record_sign_in_agent(&self, name: &str) -> Result<(), Error> {
        std::fs::write(self.path().join(SIGN_IN_AGENT_FILE), name)?;
        Ok(())
    }
}

/// What other runs left below `home`.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Leftovers {
    /// Sign-in agents of runs that are still going, which must be left alone.
    pub(super) live_sign_in_agents: Vec<String>,
}

/// Removes the staging folders of runs that ended without removing them, and reports the sign-in
/// agents that runs still going own. A folder whose lock nobody holds is a dead run's.
pub(super) fn sweep(home: &Path) -> Leftovers {
    let mut leftovers = Leftovers::default();
    let Ok(entries) = std::fs::read_dir(home) else {
        return leftovers;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let ours = entry.file_name().to_string_lossy().starts_with(PREFIX) && folder.is_dir();
        if !ours {
            continue;
        }
        let sign_in_agent = std::fs::read_to_string(folder.join(SIGN_IN_AGENT_FILE)).ok();
        if alive(&folder) {
            leftovers.live_sign_in_agents.extend(sign_in_agent);
        } else {
            std::fs::remove_dir_all(&folder).ok();
        }
    }
    leftovers
}

/// Whether the run that made `folder` still holds its lock.
fn alive(folder: &Path) -> bool {
    // Taking the lock proves its run has ended; it is let go again when the file closes. A folder
    // without a lock is one an earlier agentctl made, or one half made.
    File::open(folder.join(LOCK_FILE)).is_ok_and(|lock| lock.try_lock().is_err())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{Staging, sweep};

    #[test]
    fn a_dead_runs_folder_is_removed_and_a_live_ones_kept() {
        let home = tempfile::tempdir().expect("a home");
        let live = Staging::create(home.path()).expect("a live run");
        live.record_sign_in_agent("agentctl-signin-live").expect("recorded");
        let dead = home.path().join("setup-dead");
        std::fs::create_dir_all(&dead).expect("a dead run's folder");
        std::fs::write(dead.join(".lock"), "").expect("its lock file, held by nobody");
        std::fs::write(dead.join(".sign-in-agent"), "agentctl-signin-dead").expect("its sign-in agent");

        let leftovers = sweep(home.path());

        assert!(!dead.exists(), "the dead run's folder is removed");
        assert!(live.path().exists(), "the live run's folder is kept");
        assert_eq!(leftovers.live_sign_in_agents, ["agentctl-signin-live"]);
        let path = live.path().to_path_buf();
        drop(live);
        assert!(!path.exists(), "a run removes its own folder when it ends");
    }
}
