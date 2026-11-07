//! Declarative agent storage, reconciliation, and continuous repair.

mod controller;
mod convergence;
pub mod memory;
mod reconciler;
mod resource;
mod service;

use std::rc::Rc;

use ::sandbox::LocalFuture;

use crate::{Error, Status};

pub use controller::{Controller, ErrorHandler, Wakeup};
pub use convergence::{Convergence, WaitPolicy};
pub use reconciler::{Reconciler, SessionNotifier};
pub use resource::{AgentId, AgentRecord, ENV_FILE};
pub(crate) use service::reject_secrets_in_copy;
pub use service::{ApplyRequest, ControlPlane, ImmediateStops, Notifier};

/// Separates desired-state writes from reconciler status writes using generation checks.
pub trait AgentStore {
    /// Gets an active agent record by immutable identity.
    fn get(&self, id: AgentId) -> LocalFuture<'_, Result<AgentRecord, Error>>;

    /// Gets an active agent record by its user-facing name.
    fn get_by_name<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<AgentRecord, Error>>;

    /// Lists an independent snapshot of every record.
    fn list(&self) -> LocalFuture<'_, Result<Vec<AgentRecord>, Error>>;

    /// Creates or replaces desired state if the stored generation still matches. An agent it creates gets the labels
    /// of its record as declared, and an update keeps the stored labels.
    fn put(&self, record: AgentRecord, expected_generation: u64) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.put_with_labels(record, expected_generation, crate::labels::LabelChange::default())
                .await
                .map(drop)
        })
    }

    /// Does what [`Self::put`] does, and applies `labels` to the agent's labels in the same write, and returns its
    /// labels as stored. An apply changes both, and one write keeps a concurrent apply or a crash from leaving the
    /// desired state of one with the labels of another, or with none.
    fn put_with_labels(
        &self,
        record: AgentRecord,
        expected_generation: u64,
        labels: crate::labels::LabelChange,
    ) -> LocalFuture<'_, Result<crate::labels::StoredLabels, Error>>;

    /// Replaces observed state if the reconciled generation is still current
    /// and the agent is not being deleted, stamping condition transition times
    /// against the stored status, and returns the status as stored.
    fn update_status(&self, id: AgentId, generation: u64, status: Status) -> LocalFuture<'_, Result<Status, Error>>;

    /// Atomically records the first deletion request.
    fn mark_deleting<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<AgentRecord, Error>>;

    /// Finalizes a soft-deleted record if its desired generation has not changed.
    fn finalize_deletion(&self, id: AgentId, generation: u64) -> LocalFuture<'_, Result<(), Error>>;

    /// Replaces the volume revisions an agent's volume mounts use. Only the reconciler writes them,
    /// before the agent's sandbox mounts them, and then keeps them for the agent's life.
    fn set_volume_revisions(
        &self,
        id: AgentId,
        revisions: crate::volumes::VolumePins,
    ) -> LocalFuture<'_, Result<(), Error>>;

    /// Applies `change` to an active agent's labels in one write, and returns its labels as stored. Changing labels
    /// changes no generation, so `agentctl label` has this write of its own, which leaves the desired state alone.
    fn change_labels(
        &self,
        id: AgentId,
        change: crate::labels::LabelChange,
    ) -> LocalFuture<'_, Result<crate::labels::StoredLabels, Error>>;
}

pub(crate) type SharedAgentStore = Rc<dyn AgentStore>;
