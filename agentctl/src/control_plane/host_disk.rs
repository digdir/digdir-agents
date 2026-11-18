//! Warns on every agent while the filesystem holding the sandboxes' disks runs low on space.
//!
//! Each agent's root disk grows as its guest writes, up to its capacity, and they all share the
//! filesystem their Provider keeps them on. So while that filesystem is low, every agent is at
//! risk, and each one says so with `HostDiskLow=True`. agentd measures the filesystem itself, so
//! the condition is worked out when an agent is read, not stored. A request that would make or
//! start a sandbox measures it again, so that it is refused on the free space it would take.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use crate::{
    Condition, ConditionStatus, Error,
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

/// How long a request that would make or start a sandbox waits for the free space before it goes
/// ahead without it, so that a filesystem that stalls costs the check rather than the request.
const ADMIT_DEADLINE: Duration = Duration::from_secs(5);

/// The free space where the Providers in `sandboxes` keep the sandboxes' disks, as last measured.
#[derive(Clone)]
pub struct HostDisk {
    sandboxes: Rc<Service>,
    threshold: Rc<Cell<LowDiskThreshold>>,
    low: Rc<RefCell<Option<Condition>>>,
    fullest: Rc<Cell<Option<DiskSpace>>>,
    changes: Changes,
}

impl HostDisk {
    /// Starts with nothing measured where the Providers in `sandboxes` keep the sandboxes' disks,
    /// and `threshold` as the free space below which they are low. Every change to what agents
    /// show advances `changes`.
    #[must_use]
    pub fn new(sandboxes: Rc<Service>, threshold: LowDiskThreshold, changes: Changes) -> Self {
        Self {
            sandboxes,
            threshold: Rc::new(Cell::new(threshold)),
            low: Rc::default(),
            fullest: Rc::default(),
            changes,
        }
    }

    /// Measures the free space where the sandboxes' disks are, against `threshold`, which requests
    /// are checked against from then on. A measurement that fails keeps what agents show.
    pub async fn measure(&self, threshold: LowDiskThreshold) {
        self.threshold.set(threshold);
        if let Err(error) = self.measure_now().await {
            tracing::warn!(%error, "free space measurement failed");
        }
    }

    /// Measures the free space against the current threshold, records what agents show, and
    /// returns the space of a filesystem below it, if any.
    async fn measure_now(&self) -> Result<Option<DiskSpace>, Error> {
        let spaces = self.sandboxes.host_disks().await?;
        // Read after measuring, so a threshold changed meanwhile is the one the space is held to,
        // and the one a refusal names.
        let threshold = self.threshold.get();
        // A change in it alone wakes no watcher. Watches reply at least every 30 seconds anyway,
        // and the free space changes with nearly every measurement.
        self.fullest
            .set(spaces.iter().copied().min_by_key(|space| space.available));
        let low = spaces.into_iter().find(|space| threshold.is_low(*space));
        self.observe(low, threshold);
        Ok(low)
    }

    /// Records `low`, the filesystem below `threshold`, or `None` when none is.
    fn observe(&self, low: Option<DiskSpace>, threshold: LowDiskThreshold) {
        let condition = low.map(|space| {
            let message = low_message(space, threshold);
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

    /// The space on the filesystem with the least free space where the sandboxes' disks are, as
    /// last measured, or `None` before the first measurement.
    #[must_use]
    pub fn space(&self) -> Option<DiskSpace> {
        self.fullest.get()
    }

    /// Refuses a request that would make or start a sandbox while the filesystem is low, unless
    /// `allowed`. A new sandbox's disk grows into the space the agents already running need, so
    /// the person decides whether it may. An agent already running keeps its space and is not
    /// refused.
    ///
    /// It measures the filesystem again rather than trusting the last measurement, which can be
    /// up to [`CHECK_INTERVAL`] old, or missing before the first one. When that measurement fails
    /// or stalls, it lets the request through and logs why, since a fault in measuring should not
    /// keep every agent from starting.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] saying how much is free, and how to start the agent anyway.
    pub async fn admit(&self, allowed: bool) -> Result<(), Error> {
        if allowed {
            return Ok(());
        }
        match tokio::time::timeout(ADMIT_DEADLINE, self.measure_now()).await {
            Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(low))) => Err(Error::Invalid(format!(
                "Not starting the agent: {}. Free space there, or pass --allow-low-disk to start it anyway",
                low_message(low, self.threshold.get())
            ))),
            Ok(Err(error)) => {
                tracing::warn!(%error, "starting an agent without knowing the free space, as measuring it failed");
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    deadline = ?ADMIT_DEADLINE,
                    "starting an agent without knowing the free space, as measuring it stalled"
                );
                Ok(())
            }
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
    pub async fn run(self, config: PathBuf) {
        let mut threshold = self.threshold.get();
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

/// What is free on the filesystem with `space`, below `threshold`.
fn low_message(space: DiskSpace, threshold: LowDiskThreshold) -> String {
    format!(
        "{} free on the filesystem holding the sandboxes' disks, below lowDiskThreshold {}{}",
        bytes(space.available),
        threshold,
        match threshold {
            LowDiskThreshold::Share(_) => format!(" ({})", bytes(threshold.bytes(space.total))),
            LowDiskThreshold::Size(_) => String::new(),
        },
    )
}

/// The warning `apply` gives when `room`, what the root disks on the filesystem with `space` can
/// still grow into, is more than it has free. Disks may never fill to their capacity, so it is
/// advice rather than a refusal, but if they do, every agent on the filesystem fails its writes.
#[must_use]
pub(super) fn overcommitted(space: DiskSpace, room: u64) -> Option<Condition> {
    (room > space.available).then(|| Condition {
        kind: Condition::HOST_DISK_OVERCOMMITTED.into(),
        status: ConditionStatus::True,
        reason: Condition::REASON_CAPACITY_OVER_FREE_SPACE.into(),
        message: format!(
            "the agents' root disks can grow by another {} up to their capacity, more than the {} free on the \
             filesystem holding them",
            bytes(room),
            bytes(space.available),
        ),
        last_transition_time: None,
    })
}
