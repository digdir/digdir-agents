//! Which revision of each volume an agent mounts.

use std::rc::Rc;

use crate::{Error, FailureKind};

use super::{PinnedRevision, SharedVolumeStore, VolumePins, memory::InMemoryVolumeStore};

/// Why an agent waits for its volumes, and whether only changing a volume can end the wait.
pub(crate) type Waiting = (String, Option<FailureKind>);

/// The volumes agents mount, and the lock between agents choosing revisions and cleanup.
///
/// Agents choose only the latest revision, and cleanup never deletes it, so the lock only has to
/// cover a revision that stops being the latest in between.
#[derive(Clone)]
pub struct VolumeRevisions {
    store: SharedVolumeStore,
    choosing: Rc<tokio::sync::Mutex<()>>,
}

impl Default for VolumeRevisions {
    /// No volumes, so an agent that mounts one waits for it to exist.
    fn default() -> Self {
        Self::new(Rc::new(InMemoryVolumeStore::new()))
    }
}

impl VolumeRevisions {
    /// Creates the handle over `store`. Its clones share the lock.
    #[must_use]
    pub fn new(store: SharedVolumeStore) -> Self {
        Self {
            store,
            choosing: Rc::default(),
        }
    }

    pub(crate) fn store(&self) -> &SharedVolumeStore {
        &self.store
    }

    /// Holds off choosing and cleanup on every other clone until the guard is dropped.
    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.choosing.lock().await
    }

    /// Chooses the latest revision of each volume an agent mounts, by mount target and volume name,
    /// or says why the agent waits. An optional volume is left out when only changing it could end
    /// the wait: it does not exist, is being deleted or its copy failed. One taking its first copy
    /// is waited for.
    ///
    /// Hold [`Self::lock`] until the choice is stored, so cleanup sees it.
    pub(crate) async fn choose(
        &self,
        mounts: &[(::sandbox::SandboxPath, String, bool)],
    ) -> Result<Result<VolumePins, Waiting>, Error> {
        let mut pins = VolumePins::new();
        let mut missing = Vec::new();
        let mut waiting = Vec::new();
        let mut failure = None;
        for (target, name, optional) in mounts {
            let volume = match self.store.get_by_name(name).await {
                Ok(volume) => Some(volume),
                Err(Error::VolumeNotFound(_)) => None,
                Err(error) => return Err(error),
            };
            let latest = volume
                .as_ref()
                .filter(|volume| !volume.is_deleting())
                .and_then(|volume| Some((volume.id, volume.volume.status.latest()?)));
            if let Some((volume, revision)) = latest {
                pins.insert(
                    target.clone(),
                    Some(PinnedRevision {
                        volume,
                        revision: revision.number,
                        storage: revision.storage.clone(),
                        exposed_files: revision.exposed_files.clone(),
                    }),
                );
                continue;
            }
            let copying = volume
                .as_ref()
                .is_some_and(|volume| !volume.is_deleting() && volume.volume.status.copying().is_some());
            if *optional && !copying {
                pins.insert(target.clone(), None);
                continue;
            }
            let Some(volume) = volume else {
                missing.push(format!("{name:?}"));
                continue;
            };
            if volume.is_deleting() {
                waiting.push(format!("volume {name:?} is being deleted"));
                failure = Some(FailureKind::Invalid);
            } else if let Some(reason) = volume
                .volume
                .status
                .copy
                .as_ref()
                .and_then(|copy| copy.failure.as_ref())
            {
                waiting.push(format!("volume {name:?} has no copy: {reason}"));
                failure = Some(FailureKind::Invalid);
            } else {
                waiting.push(format!("volume {name:?} is taking its first copy"));
            }
        }
        // One hint covers every missing volume, however many the agent mounts.
        match missing.as_slice() {
            [] => {}
            [name] => waiting.insert(
                0,
                format!("volume {name} does not exist; apply a manifest that declares it"),
            ),
            names => waiting.insert(
                0,
                format!(
                    "volumes {} do not exist; apply manifests that declare them",
                    names.join(", ")
                ),
            ),
        }
        Ok(if waiting.is_empty() {
            Ok(pins)
        } else {
            Err((waiting.join("; "), failure))
        })
    }
}
