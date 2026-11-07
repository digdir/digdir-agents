//! Takes the copies volumes request and removes the revisions nothing uses.

use std::{collections::BTreeSet, rc::Rc};

use ::sandbox::{LocalFuture, ProgressReporter};

use crate::{Error, control_plane::AgentStore, controller::Reconcile};

use super::{
    CHECK_SOURCE, COPY_SOURCE, CopyProgress, CopyStatus, Revision, RevisionStorage, VERIFY_SOURCE, VolumeId,
    VolumeRecord, VolumeRevisions, VolumeSpec, fingerprint, revision_users, revision_volume_name,
};

/// Converges one volume: takes the copy it requests, deletes the revisions that are not the latest
/// and that no agent uses, and removes a deleted volume once none are left.
pub struct Reconciler {
    volumes: VolumeRevisions,
    agents: Rc<dyn AgentStore>,
    sandboxes: Rc<crate::sandbox::Service>,
    agent_wakeup: Option<crate::control_plane::Wakeup>,
    progress: CopyProgress,
}

impl Reconciler {
    /// Creates a reconciler over the volumes, which it shares with the agents choosing revisions, the
    /// agent store and the sandbox Providers holding copies.
    #[must_use]
    pub fn new(volumes: VolumeRevisions, agents: Rc<dyn AgentStore>, sandboxes: Rc<crate::sandbox::Service>) -> Self {
        Self {
            volumes,
            agents,
            sandboxes,
            agent_wakeup: None,
            progress: CopyProgress::default(),
        }
    }

    /// Reports the progress of each copy to `progress`, which clients read.
    #[must_use]
    pub fn with_progress(mut self, progress: CopyProgress) -> Self {
        self.progress = progress;
        self
    }

    /// Wakes the agents waiting for a volume once its copy is taken or has failed.
    #[must_use]
    pub fn with_agent_wakeup(mut self, wakeup: crate::control_plane::Wakeup) -> Self {
        self.agent_wakeup = Some(wakeup);
        self
    }

    /// Converges one volume, as its controller does.
    ///
    /// # Errors
    ///
    /// Returns an error when the volume or agents cannot be read or stored, or a copy cannot be
    /// deleted.
    pub async fn reconcile(&self, id: VolumeId) -> Result<(), Error> {
        loop {
            let record = match self.volumes.store().get(id).await {
                Ok(record) => record,
                Err(Error::VolumeNotFound(_)) => {
                    self.progress.forget(id);
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let Some(requested) = record.volume.status.copying().cloned() else {
                break;
            };
            let outcome = if record.is_deleting() {
                Err(("the volume was deleted before its copy was taken".to_owned(), None))
            } else {
                let progress = self.progress.begin(id, requested.number);
                let outcome = self.copy(&record, &requested, &progress).await;
                match &outcome {
                    Ok(_) => self.progress.succeed(id),
                    Err((message, _)) => self.progress.fail(id, message),
                }
                outcome
            };
            self.update(id, |record| {
                let current = record
                    .volume
                    .status
                    .copying()
                    .is_some_and(|copy| copy.same_request(&requested));
                match &outcome {
                    // A newer request replaced this one while it was taken, or the volume is being
                    // deleted, so the copy is of no use.
                    Ok(storage) if !current || record.is_deleting() => record.discarded.push(storage.clone()),
                    // The request holds the fingerprint, files and bytes of the source the copy is of.
                    Ok(storage) => {
                        record.volume.status.copy = None;
                        record.volume.status.revisions.push(Revision {
                            number: requested.number,
                            created: requested.created,
                            fingerprint: requested.fingerprint.clone(),
                            files: requested.files,
                            bytes: requested.bytes,
                            storage: storage.clone(),
                            agents: Vec::new(),
                            progress: None,
                        });
                    }
                    Err((message, leftover)) => {
                        record.discarded.extend(leftover.clone());
                        if !current {
                            return leftover.is_some();
                        }
                        if let Some(copy) = &mut record.volume.status.copy {
                            copy.failure = Some(message.clone());
                        }
                    }
                }
                true
            })
            .await?;
            self.wake_waiting_agents(&record.volume.metadata.name).await?;
        }
        self.remove_unused(id).await
    }

    /// Wakes the agents that mount the volume `name` and have no revision of it yet.
    async fn wake_waiting_agents(&self, name: &str) -> Result<(), Error> {
        let Some(wakeup) = &self.agent_wakeup else {
            return Ok(());
        };
        for agent in self.agents.list().await? {
            let waiting = agent.agent.spec.sandbox.mounts.iter().any(|mount| {
                matches!(mount, crate::MountSpec::Volume { volume, target, .. }
                    if volume == name && !agent.volume_revisions.contains_key(target))
            });
            if waiting {
                wakeup.notify(agent.id);
            }
        }
        Ok(())
    }

    /// Takes the copy `requested`. The source is fingerprinted again once it is copied, and a copy of
    /// a source that changed since it was requested is discarded, since it may mix old and new files.
    /// A failure returns why, and the copy it could not delete, if any.
    async fn copy(
        &self,
        record: &VolumeRecord,
        requested: &CopyStatus,
        progress: &ProgressReporter,
    ) -> Result<RevisionStorage, (String, Option<RevisionStorage>)> {
        let VolumeSpec::Copy { source } = &record.volume.spec;
        // A phase still open when the copy fails ends as failed with it.
        let prepare = async {
            let checking = progress.start_phase(CHECK_SOURCE).await;
            crate::control_plane::reject_secrets_in_copy(self.agents.as_ref(), "spec.source", source).await?;
            checking.complete().await;
            let copying = progress.start_phase(COPY_SOURCE).await;
            let name = revision_volume_name(record.id, requested.number)?;
            let (provider, volume) = self
                .sandboxes
                .ensure_directory_copy(&name, source, progress.clone())
                .await?;
            copying.complete().await;
            Ok::<_, Error>(RevisionStorage { provider, volume })
        };
        let storage = prepare.await.map_err(|error| (error.to_string(), None))?;
        let verifying = progress.start_phase(VERIFY_SOURCE).await;
        let failure = match fingerprint::take(source.clone()).await {
            Ok(copied) if copied.digest == requested.fingerprint => {
                verifying.complete().await;
                return Ok(storage);
            }
            Ok(_) => format!(
                "{} changed after the volume was applied, so the copy may mix old and new files; apply the volume \
                 again once it has settled",
                source.display()
            ),
            Err(error) => error.to_string(),
        };
        let leftover = match self.sandboxes.delete_volume(&storage.provider, &storage.volume).await {
            Ok(true) => None,
            Ok(false) => Some(storage),
            Err(error) => {
                tracing::warn!(volume = %record.id, %error, "failed to delete a copy that failed; cleanup retries");
                Some(storage)
            }
        };
        Err((failure, leftover))
    }

    /// Deletes the revisions that are not the latest and that no agent uses, and the copies no revision
    /// holds, then a deleted volume once nothing is left. A copy that a sandbox still mounts, such as
    /// one an agent's retained sandbox keeps, stays until a later pass.
    ///
    /// Agents choose revisions while holding the same lock, so none chooses one deleted here.
    async fn remove_unused(&self, id: VolumeId) -> Result<(), Error> {
        let _choosing = self.volumes.lock().await;
        let users = revision_users(&self.agents.list().await?);
        let record = self.volumes.store().get(id).await?;
        let latest = (!record.is_deleting())
            .then(|| record.volume.status.latest().map(|revision| revision.number))
            .flatten();
        let unused = record
            .volume
            .status
            .revisions
            .iter()
            .filter(|revision| Some(revision.number) != latest && !users.contains_key(&(id, revision.number)))
            .map(|revision| (Some(revision.number), &revision.storage));
        let mut removed = BTreeSet::new();
        let mut deleted = Vec::new();
        let mut failure = None;
        for (number, storage) in unused.chain(record.discarded.iter().map(|storage| (None, storage))) {
            match self.sandboxes.delete_volume(&storage.provider, &storage.volume).await {
                Ok(true) => {
                    removed.extend(number);
                    deleted.push(storage.clone());
                }
                Ok(false) => {}
                Err(error) => failure = Some(error),
            }
        }
        if !deleted.is_empty() {
            self.update(id, |record| {
                record
                    .volume
                    .status
                    .revisions
                    .retain(|revision| !removed.contains(&revision.number));
                record.discarded.retain(|storage| !deleted.contains(storage));
                true
            })
            .await?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        loop {
            let record = self.volumes.store().get(id).await?;
            let empty = record.volume.status.revisions.is_empty() && record.discarded.is_empty();
            if !record.is_deleting() || !empty || record.volume.status.copying().is_some() {
                return Ok(());
            }
            match self.volumes.store().remove(id, record.version).await {
                Ok(()) => {
                    self.progress.forget(id);
                    return Ok(());
                }
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Applies `change` to the stored record until it is stored without a concurrent write in
    /// between. `change` returns whether it changed anything.
    async fn update(&self, id: VolumeId, change: impl Fn(&mut VolumeRecord) -> bool) -> Result<(), Error> {
        loop {
            let mut record = self.volumes.store().get(id).await?;
            let expected_version = record.version;
            if !change(&mut record) {
                return Ok(());
            }
            match self.volumes.store().put(record, expected_version).await {
                Ok(()) => return Ok(()),
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Reconcile<VolumeId> for Reconciler {
    fn reconcile(&self, key: VolumeId) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(Self::reconcile(self, key))
    }
}
