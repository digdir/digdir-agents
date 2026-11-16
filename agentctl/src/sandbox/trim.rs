//! Returning the blocks a guest has freed to the host.
//!
//! A sandbox's disk file on the host keeps every block its guest ever wrote until the guest
//! discards it. ext4 trims only the block groups that changed since its last trim, so a frequent
//! trim stays cheap. A layered root has no file system in the guest to trim, so only a direct one
//! is trimmed.

use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use ::sandbox::{RootFilesystemMode, SandboxPath, SandboxState, execution::ExecutionSpec};

use crate::{
    Error,
    control_plane::{AgentRecord, AgentStore},
};

use super::Service;

/// How often each running agent with a direct root filesystem has it trimmed.
pub const TRIM_INTERVAL: Duration = Duration::from_mins(15);

/// Exit status of the shell when `fstrim` is not in the guest.
const NOT_FOUND: i32 = 127;

/// Trims the root filesystem of each running agent every [`TRIM_INTERVAL`].
pub struct Trimmer {
    agents: Rc<dyn AgentStore>,
    sandboxes: Rc<Service>,
    /// Sandboxes whose guest cannot trim, which are not asked again.
    untrimmable: RefCell<HashSet<::sandbox::SandboxId>>,
}

/// What one trim found.
enum Trimmed {
    Trimmed,
    /// The guest cannot trim, for the reason given.
    Unable(String),
}

impl Trimmer {
    /// Creates a trimmer over the stored agents and their sandboxes.
    #[must_use]
    pub fn new(agents: Rc<dyn AgentStore>, sandboxes: Rc<Service>) -> Self {
        Self {
            agents,
            sandboxes,
            untrimmable: RefCell::default(),
        }
    }

    /// Trims every [`TRIM_INTERVAL`] while `agentd` runs.
    pub async fn run(self) {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + TRIM_INTERVAL, TRIM_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            self.trim_all().await;
        }
    }

    async fn trim_all(&self) {
        let records = match self.agents.list().await {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error, "listing agents to trim their disks failed");
                return;
            }
        };
        for record in records {
            if record.agent.metadata.deletion_timestamp.is_some() || record.agent.spec.is_stopped() {
                continue;
            }
            let Some(id) = record.agent.status.sandbox.as_ref().and_then(super::Assignment::id) else {
                continue;
            };
            if self.untrimmable.borrow().contains(id) {
                continue;
            }
            let agent = &record.agent.metadata.name;
            match self.trim(&record, id).await {
                Ok(None | Some(Trimmed::Trimmed)) => {}
                Ok(Some(Trimmed::Unable(reason))) => {
                    // The agent works without it, so this is no condition. The host decides
                    // whether the guest's disk can discard, so asking again would not help.
                    tracing::info!(agent, "the guest's disk is not trimmed: {reason}");
                    self.untrimmable.borrow_mut().insert(id.clone());
                }
                Err(error) => tracing::warn!(agent, %error, "trimming the guest's disk failed"),
            }
        }
    }

    /// Trims the root filesystem of the agent's sandbox `id`, unless it is not running or not direct.
    async fn trim(&self, record: &AgentRecord, id: &::sandbox::SandboxId) -> Result<Option<Trimmed>, Error> {
        let sandbox = self.sandboxes.open(record).await?;
        let snapshot = sandbox.snapshot();
        if snapshot.state != SandboxState::Running
            || snapshot.resources.root_filesystem().mode() != RootFilesystemMode::Direct
        {
            return Ok(None);
        }
        // Executions run as root. The shell finds `fstrim` wherever the image keeps it.
        let spec = ExecutionSpec::command(
            SandboxPath::new("/bin/sh"),
            ["-c".to_owned(), "exec fstrim /".to_owned()],
        );
        let output = self
            .sandboxes
            .guard_guest(record, id, async { Ok(sandbox.run_execution(spec).await?) })
            .await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        Ok(Some(match output.status.code {
            0 => Trimmed::Trimmed,
            NOT_FOUND => Trimmed::Unable("the image has no `fstrim`".into()),
            // fstrim's message when the disk does not accept discards, as when the host turns them off.
            _ if stderr.contains("not supported") => Trimmed::Unable(stderr.to_owned()),
            code => {
                return Err(Error::SandboxSetup(format!(
                    "`fstrim /` exited with code {code}: {stderr}"
                )));
            }
        }))
    }
}
