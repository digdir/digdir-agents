//! Where one run, of setup or a login, keeps what it needs for a while, such as the sign-in agent's
//! empty home. It sits below the control-plane home, never in a folder a person mounts, and goes
//! when the run ends.
//!
//! A run that is killed, such as with Ctrl-C at a question, cannot remove its folder or delete its
//! sign-in agent. Each run therefore holds a lock on its folder for as long as it lives, and names
//! its sign-in agent there, so the next run knows a dead run's leftovers from a live one's and
//! removes them.

use std::{fs::File, path::Path, time::Duration};

use agent::Error;

const PREFIX: &str = "run-";
/// Locked for as long as the run that made the folder lives; the operating system lets go of it
/// when that process ends, however it ends.
const LOCK_FILE: &str = ".lock";
/// The sign-in agent the run started, for a later run to delete if this one could not.
const SIGN_IN_AGENT_FILE: &str = ".sign-in-agent";
/// How long a new folder is taken to belong to a live run, lock or not, so that another run does
/// not remove it in the moment between making it and locking it.
const GRACE: Duration = Duration::from_mins(1);

/// One run's staging folder, removed when it is dropped.
pub(crate) struct Staging {
    // Held only to keep the lock; dropped first, so that the folder can be removed on Windows,
    // where an open file pins it.
    _lock: File,
    directory: tempfile::TempDir,
}

impl Staging {
    /// Makes a new staging folder below `home` and locks it for this run.
    pub(crate) fn create(home: &Path) -> Result<Self, Error> {
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(home)?;
        let lock = File::create(directory.path().join(LOCK_FILE))?;
        lock.try_lock()
            .map_err(|error| Error::Invalid(format!("could not lock this run's folder: {error}")))?;
        Ok(Self { _lock: lock, directory })
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Records the sign-in agent this run starts, before it is created.
    pub(crate) fn record_sign_in_agent(&self, name: &str) -> Result<(), Error> {
        std::fs::write(self.path().join(SIGN_IN_AGENT_FILE), name)?;
        Ok(())
    }
}

/// What other runs left below `home`.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Leftovers {
    /// Sign-in agents of runs that ended before they could delete them. Only these are deleted,
    /// so neither a live run's nor a person's own agent of a similar name is.
    pub(crate) dead_sign_in_agents: Vec<String>,
}

/// Removes the staging folders of runs that ended without removing them, and reports the sign-in
/// agents they left. A folder whose lock nobody holds is a dead run's.
pub(crate) fn sweep(home: &Path) -> Leftovers {
    sweep_older_than(home, GRACE)
}

/// Sweeps as [`sweep`] does, taking a folder younger than `grace` to be a live run's.
fn sweep_older_than(home: &Path, grace: Duration) -> Leftovers {
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
        if !alive(&folder, grace) {
            let sign_in_agent = std::fs::read_to_string(folder.join(SIGN_IN_AGENT_FILE)).ok();
            leftovers.dead_sign_in_agents.extend(sign_in_agent);
            std::fs::remove_dir_all(&folder).ok();
        }
    }
    leftovers
}

/// Whether the run that made `folder` still holds its lock.
fn alive(folder: &Path, grace: Duration) -> bool {
    let new = std::fs::metadata(folder)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age < grace));
    // Taking the lock proves its run has ended; it is let go again when the file closes. An older
    // folder without a lock is one an earlier agentctl made, or one left half made.
    new || File::open(folder.join(LOCK_FILE)).is_ok_and(|lock| lock.try_lock().is_err())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{GRACE, Staging, sweep_older_than};

    #[test]
    fn a_dead_runs_folder_is_removed_and_a_live_ones_kept() {
        let home = tempfile::tempdir().expect("a home");
        let live = Staging::create(home.path()).expect("a live run");
        live.record_sign_in_agent("agentctl-signin-live").expect("recorded");
        let dead = home.path().join("run-dead");
        std::fs::create_dir_all(&dead).expect("a dead run's folder");
        std::fs::write(dead.join(".lock"), "").expect("its lock file, held by nobody");
        std::fs::write(dead.join(".sign-in-agent"), "agentctl-signin-dead").expect("its sign-in agent");
        let unlocked = home.path().join("run-new");
        std::fs::create_dir_all(&unlocked).expect("a folder a run has made but not locked yet");

        let young = sweep_older_than(home.path(), GRACE);
        assert!(dead.exists() && unlocked.exists(), "a new folder is kept, lock or not");
        assert_eq!(young.dead_sign_in_agents, Vec::<String>::new());

        let leftovers = sweep_older_than(home.path(), std::time::Duration::ZERO);
        assert!(!dead.exists(), "the dead run's folder is removed");
        assert!(live.path().exists(), "the live run's folder is kept");
        assert_eq!(leftovers.dead_sign_in_agents, ["agentctl-signin-dead"]);
        let path = live.path().to_path_buf();
        drop(live);
        assert!(!path.exists(), "a run removes its own folder when it ends");
    }
}
