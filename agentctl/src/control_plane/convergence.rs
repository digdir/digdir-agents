//! Requests converging an agent and waiting for the outcome.
//!
//! A waiter reads readiness and failure from the stored agent and rereads it
//! whenever the daemon-wide revision advances, so it can skip intermediate
//! states but never miss the terminal one. Progress is observed separately,
//! through the agent's provisioning state.

use std::time::Duration;

use crate::{AgentId, Error, FailureKind, ReconcileFailure, resources::Changes};

use super::{SharedAgentStore, Wakeup};

/// Longest a waiter goes without rereading the stored agent.
const RECHECK_INTERVAL: Duration = Duration::from_secs(30);
/// How long a waiter lets changes gather before rereading the stored agent.
/// Every progress event of any agent advances the revision, so a waiter
/// rereads once per burst instead of once per event.
const SETTLE: Duration = Duration::from_millis(50);

/// How long a request waits for the agent it woke.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitPolicy {
    /// Returns after one reconciliation pass with that pass's outcome.
    FirstPass,
    /// Keeps waiting through transient failures, which the background controller
    /// retries, until the agent has its desired run state or that state is
    /// invalid. A running agent's guest recorded as unresponsive ends the wait
    /// instead.
    UntilConverged,
}

/// Wakes agent convergence and lets a request wait for it.
#[derive(Clone)]
pub struct Convergence {
    wakeup: Wakeup,
    store: SharedAgentStore,
    changes: Changes,
}

impl Convergence {
    /// Pairs the controller's wake-up handle with the stored agents and their revision.
    #[must_use]
    pub fn new(wakeup: Wakeup, store: SharedAgentStore, changes: Changes) -> Self {
        Self { wakeup, store, changes }
    }

    /// Wakes convergence of the named agent, waits according to `wait`, and
    /// returns the agent as stored when the wait ended. Converged means its
    /// desired run state was recorded for its generation: Ready when it runs,
    /// stopped when it is stopped. A caller that needs the sandbox running
    /// refuses a stopped agent itself, before and after converging.
    ///
    /// # Errors
    ///
    /// Returns `Error::Invalid` when desired state must change, the first pass's
    /// failure under [`WaitPolicy::FirstPass`], `Error::SandboxUnresponsive`
    /// when a running agent's guest is unresponsive, `Error::Conflict` when the
    /// agent is being or was deleted, `Error::NotFound` when it does not exist,
    /// or a storage error.
    pub async fn converge(&self, name: &str, wait: WaitPolicy) -> Result<super::AgentRecord, Error> {
        let record = self.store.get_by_name(name).await?;
        if record.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Conflict);
        }
        let id = record.id;
        let woken = self.wakeup.reconcile(id).await;
        let record = self.get(id).await?;
        match (wait, woken) {
            (WaitPolicy::FirstPass, Ok(())) => return Ok(record),
            (WaitPolicy::FirstPass, Err(failure)) => {
                // A stalled guest is reported as the stall, not as a daemon failure.
                return Err(record.agent.status.unresponsive().map_or_else(
                    || failure.into(),
                    |stalled| Error::SandboxUnresponsive(stalled.detail()),
                ));
            }
            (WaitPolicy::UntilConverged, Err(failure)) if failure.kind == FailureKind::Invalid => {
                return Err(failure.into());
            }
            (WaitPolicy::UntilConverged, _) => {}
        }
        loop {
            let revision = self.changes.revision();
            let record = self.get(id).await?;
            if let Some(outcome) = outcome(&record) {
                return outcome.map(|()| record);
            }
            self.changes
                .changed_since(Some(revision), SETTLE, RECHECK_INTERVAL)
                .await;
        }
    }

    /// Hints convergence of every agent, for host state any of them may wait on, such as the login
    /// an optional harness installation needs. The periodic pass still follows a dropped hint.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the agents cannot be listed.
    pub async fn wake_all(&self) -> Result<(), Error> {
        for record in self.store.list().await? {
            self.wakeup.notify(record.id);
        }
        Ok(())
    }

    /// Reads the waited-on agent, which must still exist.
    async fn get(&self, id: AgentId) -> Result<super::AgentRecord, Error> {
        match self.store.get(id).await {
            Ok(record) if record.agent.metadata.deletion_timestamp.is_none() => Ok(record),
            Ok(_) | Err(Error::NotFound) => Err(Error::Conflict),
            Err(error) => Err(error),
        }
    }
}

/// Ends a wait on `record`: `Ok` once a pass for its generation recorded its
/// desired run state, an error when it cannot get there without a change, and
/// `None` while it still may.
fn outcome(record: &super::AgentRecord) -> Option<Result<(), Error>> {
    let status = &record.agent.status;
    let stopped = record.agent.spec.is_stopped();
    if status.observed_generation == record.agent.metadata.generation {
        if (stopped && status.is_stopped()) || (!stopped && status.is_ready()) {
            return Some(Ok(()));
        }
        if let Some(message) = status.invalid() {
            return Some(Err(ReconcileFailure {
                kind: FailureKind::Invalid,
                message,
            }
            .into()));
        }
    }
    // A stopped agent reaches nothing in its guest, so a stall recorded before it stopped is stale.
    if !stopped && let Some(stalled) = status.unresponsive() {
        return Some(Err(Error::SandboxUnresponsive(stalled.detail())));
    }
    None
}
