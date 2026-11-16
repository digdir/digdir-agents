//! Warns on every agent while the filesystem holding the sandboxes' disks runs low on space.
//!
//! Each agent's root disk grows as its guest writes, up to its capacity, and they all share the
//! filesystem their Provider keeps them on. So while that filesystem is low, every agent is at
//! risk, and each one says so with `HostDiskLow=True`. agentd measures the filesystem itself, so
//! the condition is worked out when an agent is read, not stored.

use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

use crate::{
    Condition, ConditionStatus,
    local::{
        config::DaemonConfig,
        disk::{DiskSpace, LowDiskThreshold},
    },
    progress::bytes,
    resources::Changes,
    sandbox::Service,
};

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

    /// Measures the free space where the sandboxes' disks are, against `threshold`. A measurement
    /// that fails keeps what agents show.
    pub async fn measure(&self, threshold: LowDiskThreshold) {
        match self.sandboxes.host_disks().await {
            Ok(spaces) => self.observe(spaces.into_iter().find(|space| threshold.is_low(*space)), threshold),
            Err(error) => tracing::warn!(%error, "free space measurement failed"),
        }
    }

    /// Records `low`, the filesystem below `threshold`, or `None` when none is.
    fn observe(&self, low: Option<DiskSpace>, threshold: LowDiskThreshold) {
        let condition = low.map(|space| {
            let message = format!(
                "{} free on the filesystem holding the sandboxes' disks, below lowDiskThreshold {}{}",
                bytes(space.available),
                threshold,
                match threshold {
                    LowDiskThreshold::Share(_) => format!(" ({})", bytes(threshold.bytes(space.total))),
                    LowDiskThreshold::Size(_) => String::new(),
                },
            );
            let since = self
                .low
                .borrow()
                .as_ref()
                .and_then(|low| low.last_transition_time)
                .unwrap_or_else(time::OffsetDateTime::now_utc);
            Condition {
                kind: Condition::HOST_DISK_LOW.into(),
                status: ConditionStatus::True,
                reason: Condition::REASON_BELOW_THRESHOLD.into(),
                message,
                last_transition_time: Some(since),
            }
        });
        if *self.low.borrow() != condition {
            self.low.replace(condition);
            self.changes.bump();
        }
    }

    /// The condition every agent shows while the filesystem is low.
    #[must_use]
    pub fn condition(&self) -> Option<Condition> {
        self.low.borrow().clone()
    }

    /// Measures the filesystem now and then, against the threshold in `config`, which it reads
    /// again each time so that a change applies without restarting agentd. A setting it cannot
    /// read keeps the last one it could.
    pub async fn run(self, config: PathBuf, mut threshold: LowDiskThreshold) {
        let mut ticker = tokio::time::interval(CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let path = config.clone();
            let loaded = common::blocking::run("reading agentd.toml", move || DaemonConfig::load(&path))
                .await
                .map_err(crate::Error::from)
                .and_then(|loaded| loaded);
            match loaded {
                Ok(loaded) => threshold = loaded.low_disk_threshold,
                Err(error) => tracing::warn!(%error, "keeping the last low disk threshold"),
            }
            self.measure(threshold).await;
        }
    }
}
