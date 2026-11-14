//! Warns on every agent while the filesystem holding the sandboxes' disks runs low on space.
//!
//! Each agent's root disk grows as its guest writes, up to its capacity, and they all share the
//! filesystem their Provider keeps them on. So while that filesystem is low, every agent is at
//! risk, and each one says so with `HostDiskLow=True`. agentd measures the filesystem itself, so
//! the condition is worked out when an agent is read, not stored.

use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

use crate::{Condition, local::disk::LowDiskThreshold, resources::Changes, sandbox::Service};

/// How often agentd measures the filesystem. A build can write gigabytes in a minute.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// The free space where the Providers in `sandboxes` keep the sandboxes' disks, as last measured.
#[derive(Clone)]
pub struct HostDisk {
    sandboxes: Rc<Service>,
    low: Rc<RefCell<Option<Condition>>>,
    changes: Changes,
}

impl HostDisk {
    /// Starts with nothing measured where the Providers in `sandboxes` keep the sandboxes' disks.
    /// Every change to what agents show advances `changes`.
    #[must_use]
    pub fn new(sandboxes: Rc<Service>, changes: Changes) -> Self {
        Self {
            sandboxes,
            low: Rc::default(),
            changes,
        }
    }

    /// Measures the free space where the sandboxes' disks are, against `threshold`.
    pub async fn measure(&self, threshold: LowDiskThreshold) {
        let _ = (threshold, &self.sandboxes, &self.low, &self.changes);
    }

    /// The condition every agent shows while the filesystem is low.
    #[must_use]
    pub fn condition(&self) -> Option<Condition> {
        None
    }

    /// Measures the filesystem now and then, against the threshold in `config`.
    pub async fn run(self, config: PathBuf, threshold: LowDiskThreshold) {
        let _ = (config, threshold, CHECK_INTERVAL);
        std::future::pending::<()>().await;
    }
}
