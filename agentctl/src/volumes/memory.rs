//! In-memory volume store.

use std::{cell::RefCell, collections::BTreeMap};

use sandbox::LocalFuture;

use crate::{Error, resources::Changes};

use super::{VolumeId, VolumeRecord, VolumeStore};

/// In-memory volume store with version-based compare-and-swap writes.
///
/// Like the database, every successful write advances its change history.
#[derive(Default)]
pub struct InMemoryVolumeStore {
    records: RefCell<BTreeMap<VolumeId, VolumeRecord>>,
    changes: Changes,
}

impl InMemoryVolumeStore {
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
}

impl VolumeStore for InMemoryVolumeStore {
    fn get(&self, id: VolumeId) -> LocalFuture<'_, Result<VolumeRecord, Error>> {
        Box::pin(async move {
            self.records
                .borrow()
                .get(&id)
                .cloned()
                .ok_or_else(|| Error::VolumeNotFound(id.to_string()))
        })
    }

    fn get_by_name<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<VolumeRecord, Error>> {
        Box::pin(async move {
            self.records
                .borrow()
                .values()
                .find(|record| record.volume.metadata.name == name)
                .cloned()
                .ok_or_else(|| Error::VolumeNotFound(name.to_owned()))
        })
    }

    fn list(&self) -> LocalFuture<'_, Result<Vec<VolumeRecord>, Error>> {
        Box::pin(async move {
            let mut records = self.records.borrow().values().cloned().collect::<Vec<_>>();
            records.sort_by(|left, right| left.volume.metadata.name.cmp(&right.volume.metadata.name));
            Ok(records)
        })
    }

    fn put(&self, mut record: VolumeRecord, expected_version: u64) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            let mut records = self.records.borrow_mut();
            let current = records
                .get(&record.id)
                .map(|current| current.version)
                .unwrap_or_default();
            let name_taken = records
                .values()
                .any(|other| other.id != record.id && other.volume.metadata.name == record.volume.metadata.name);
            if current != expected_version || name_taken {
                return Err(Error::Conflict);
            }
            record.version = expected_version + 1;
            records.insert(record.id, record);
            self.changes.bump();
            Ok(())
        })
    }

    fn remove(&self, id: VolumeId, expected_version: u64) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            let mut records = self.records.borrow_mut();
            if records.get(&id).map(|record| record.version) != Some(expected_version) {
                return Err(Error::Conflict);
            }
            records.remove(&id);
            self.changes.bump();
            Ok(())
        })
    }
}
