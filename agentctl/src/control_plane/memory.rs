//! Single-threaded in-memory agent control plane components.

use std::{cell::RefCell, collections::BTreeMap};

use sandbox::LocalFuture;
use time::OffsetDateTime;

use crate::{AgentId, Error, Status, resources::Changes};

use super::{AgentRecord, AgentStore};

/// In-memory agent store with generation-based compare-and-swap writes.
///
/// Like the database, every successful write advances its change history.
#[derive(Default)]
pub struct InMemoryAgentStore {
    state: RefCell<State>,
    changes: Changes,
}

#[derive(Default)]
struct State {
    records: BTreeMap<AgentId, AgentRecord>,
    active_names: BTreeMap<String, AgentId>,
    labels: BTreeMap<AgentId, crate::labels::StoredLabels>,
}

impl InMemoryAgentStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty store whose writes advance `changes`.
    #[must_use]
    pub fn with_changes(changes: Changes) -> Self {
        Self {
            changes,
            ..Self::default()
        }
    }

    fn changed<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        if result.is_ok() {
            self.changes.bump();
        }
        result
    }
}

impl AgentStore for InMemoryAgentStore {
    fn get(&self, id: AgentId) -> LocalFuture<'_, Result<AgentRecord, Error>> {
        Box::pin(async move {
            let state = self.state.borrow();
            let record = state.records.get(&id).ok_or(Error::NotFound)?;
            (state.active_names.get(&record.agent.metadata.name) == Some(&id))
                .then(|| record.clone())
                .ok_or(Error::NotFound)
        })
    }

    fn get_by_name<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<AgentRecord, Error>> {
        Box::pin(async move {
            let state = self.state.borrow();
            let id = state.active_names.get(name).ok_or(Error::NotFound)?;
            state.records.get(id).cloned().ok_or(Error::NotFound)
        })
    }

    fn list(&self) -> LocalFuture<'_, Result<Vec<AgentRecord>, Error>> {
        Box::pin(async move {
            let state = self.state.borrow();
            Ok(state
                .active_names
                .values()
                .filter_map(|id| state.records.get(id))
                .cloned()
                .collect())
        })
    }

    fn put_with_labels(
        &self,
        mut record: AgentRecord,
        expected_generation: u64,
        labels: crate::labels::LabelChange,
    ) -> LocalFuture<'_, Result<crate::labels::StoredLabels, Error>> {
        Box::pin(async move {
            let result = (|| {
                record.agent.status.scrub_projected();
                let id = record.id;
                let name = record.agent.metadata.name.clone();
                let mut state = self.state.borrow_mut();
                let State {
                    records,
                    active_names,
                    labels: stored_labels,
                } = &mut *state;
                if expected_generation == 0 {
                    if active_names.contains_key(&name) || records.contains_key(&id) {
                        return Err(Error::Conflict);
                    }
                    let mut stored = crate::labels::StoredLabels {
                        declared: std::mem::take(&mut record.agent.metadata.labels),
                        ..crate::labels::StoredLabels::default()
                    };
                    stored.apply(&labels);
                    record.agent.metadata.labels = stored.effective();
                    active_names.insert(name, id);
                    stored_labels.insert(id, stored.clone());
                    records.insert(id, record);
                    return Ok(stored);
                }

                let active_id = active_names.get(&name).copied().ok_or(Error::Conflict)?;
                if active_id != id {
                    return Err(Error::Conflict);
                }
                let current = records.get_mut(&id).ok_or(Error::Conflict)?;
                if current.agent.metadata.generation != expected_generation
                    || current.agent.metadata.deletion_timestamp.is_some()
                {
                    return Err(Error::Conflict);
                }
                // As in the database, they are decided when the agent is created.
                record.optional_mount_sources = std::mem::take(&mut current.optional_mount_sources);
                // Only the reconciler pins volume revisions, as in the database.
                record.volume_revisions = std::mem::take(&mut current.volume_revisions);
                let stored = stored_labels.entry(id).or_default();
                stored.apply(&labels);
                record.agent.metadata.labels = stored.effective();
                *current = record;
                Ok(stored.clone())
            })();
            self.changed(result)
        })
    }

    fn update_status(
        &self,
        id: AgentId,
        generation: u64,
        mut status: Status,
    ) -> LocalFuture<'_, Result<Status, Error>> {
        Box::pin(async move {
            let result = (|| {
                status.scrub_projected();
                let mut state = self.state.borrow_mut();
                let name = state
                    .records
                    .get(&id)
                    .map(|record| record.agent.metadata.name.clone())
                    .ok_or(Error::NotFound)?;
                if state.active_names.get(&name) != Some(&id) {
                    return Err(Error::NotFound);
                }
                let record = state.records.get_mut(&id).ok_or(Error::NotFound)?;
                if record.agent.metadata.generation != generation || record.agent.metadata.deletion_timestamp.is_some()
                {
                    return Err(Error::Conflict);
                }
                status.stamp_transitions(&record.agent.status, OffsetDateTime::now_utc());
                record.agent.status = status.clone();
                Ok(status)
            })();
            self.changed(result)
        })
    }

    fn mark_deleting<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<AgentRecord, Error>> {
        Box::pin(async move {
            let result = (|| {
                let mut state = self.state.borrow_mut();
                let id = state.active_names.get(name).copied().ok_or(Error::NotFound)?;
                let record = state.records.get_mut(&id).ok_or(Error::NotFound)?;
                if record.agent.metadata.deletion_timestamp.is_none() {
                    record.agent.metadata.deletion_timestamp = Some(OffsetDateTime::now_utc());
                }
                Ok(record.clone())
            })();
            self.changed(result)
        })
    }

    fn finalize_deletion(&self, id: AgentId, generation: u64) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            let result = (|| {
                let mut state = self.state.borrow_mut();
                let record = state.records.get_mut(&id).ok_or(Error::NotFound)?;
                if record.agent.metadata.generation != generation || record.agent.metadata.deletion_timestamp.is_none()
                {
                    return Err(Error::Conflict);
                }
                let name = record.agent.metadata.name.clone();
                if state.active_names.get(&name) != Some(&id) {
                    return Err(Error::NotFound);
                }
                state.active_names.remove(&name);
                Ok(())
            })();
            self.changed(result)
        })
    }

    fn set_volume_revisions(
        &self,
        id: AgentId,
        revisions: crate::volumes::VolumePins,
    ) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            let result = (|| {
                let mut state = self.state.borrow_mut();
                let record = state.records.get_mut(&id).ok_or(Error::NotFound)?;
                record.volume_revisions = revisions;
                Ok(())
            })();
            self.changed(result)
        })
    }

    fn change_labels(
        &self,
        id: AgentId,
        change: crate::labels::LabelChange,
    ) -> LocalFuture<'_, Result<crate::labels::StoredLabels, Error>> {
        Box::pin(async move {
            let result = (|| {
                let mut state = self.state.borrow_mut();
                let State {
                    records,
                    active_names,
                    labels,
                } = &mut *state;
                let record = records.get_mut(&id).ok_or(Error::NotFound)?;
                if active_names.get(&record.agent.metadata.name) != Some(&id) {
                    return Err(Error::NotFound);
                }
                if record.agent.metadata.deletion_timestamp.is_some() {
                    return Err(Error::Conflict);
                }
                let stored = labels.entry(id).or_default();
                stored.apply(&change);
                record.agent.metadata.labels = stored.effective();
                Ok(stored.clone())
            })();
            self.changed(result)
        })
    }
}
