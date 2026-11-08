//! Applying, reading and deleting volumes.

use std::{collections::BTreeMap, path::PathBuf, rc::Rc};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{Error, control_plane::AgentStore};

use super::{
    CopyProgress, CopyStatus, KIND, SharedVolumeStore, Volume, VolumeId, VolumeRecord, VolumeSpec, VolumeStatus,
    Wakeup, fingerprint,
};

/// Desired state submitted by a client, with the local context to resolve its source in.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApplyVolumeRequest {
    /// Absolute directory against which a relative source is resolved.
    pub source_directory: PathBuf,
    /// Absolute path of the manifest, when the client reads one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
    /// The volume as its manifest declares it.
    pub volume: Volume,
}

/// Stores the volumes clients declare and reports their revisions.
pub struct VolumeService {
    store: SharedVolumeStore,
    agents: Rc<dyn AgentStore>,
    wakeup: Wakeup,
    progress: CopyProgress,
    home: Option<PathBuf>,
}

impl VolumeService {
    /// Creates a service over the volume and agent stores, waking the volume controller after writes.
    #[must_use]
    pub fn new(store: SharedVolumeStore, agents: Rc<dyn AgentStore>, wakeup: Wakeup) -> Self {
        Self {
            store,
            agents,
            wakeup,
            progress: CopyProgress::default(),
            home: None,
        }
    }

    /// Refuses a source that holds agentd's home directory, `home`, or is inside it, since it holds
    /// agentd's database and the copies themselves.
    #[must_use]
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// Reports the progress of the copies the volume reconciler takes, read from `progress`.
    #[must_use]
    pub fn with_progress(mut self, progress: CopyProgress) -> Self {
        self.progress = progress;
        self
    }

    /// Stores a volume's desired state and, when its source changed since the latest copy, requests a
    /// new one, which the volume controller takes.
    ///
    /// # Errors
    ///
    /// Returns an error when the volume is invalid, its source would expose secrets or cannot be
    /// read, it is being deleted, or it cannot be stored.
    pub async fn apply(&self, request: ApplyVolumeRequest) -> Result<Volume, Error> {
        let mut desired = request.volume;
        validate(&desired)?;
        if !request.source_directory.is_absolute() {
            return Err(Error::Invalid("sourceDirectory must be absolute".into()));
        }
        let VolumeSpec::Copy { source } = &mut desired.spec;
        *source = resolve_source(&request.source_directory, source).await?;
        let fingerprint = self.fingerprint_source(source.clone()).await?;
        let provenance = crate::Provenance {
            source_directory: request.source_directory,
            manifest_path: request.manifest_path,
            env_file: None,
        };
        desired.metadata.generation = 1;
        desired.metadata.deletion_timestamp = None;
        desired.metadata.restart_timestamp = None;
        desired.status = VolumeStatus {
            provenance: Some(provenance.clone()),
            ..VolumeStatus::default()
        };

        loop {
            let (record, expected_version) = match self.store.get_by_name(&desired.metadata.name).await {
                Ok(mut current) => {
                    if current.is_deleting() {
                        return Err(Error::Invalid(format!(
                            "volume {:?} is being deleted; apply it again once it is gone",
                            desired.metadata.name
                        )));
                    }
                    // Another folder's volume has the name, as for agents. Applying it would point
                    // this volume at that folder's source.
                    if let Some(directory) = current
                        .source_directory()
                        .filter(|directory| *directory != provenance.source_directory)
                    {
                        return Err(Error::Invalid(format!(
                            "a volume named {:?} was already applied from {}; choose another name in metadata.name or \
                             pass `--name`",
                            desired.metadata.name,
                            directory.display()
                        )));
                    }
                    let expected_version = current.version;
                    let spec_changed = current.volume.spec != desired.spec;
                    let status = &mut current.volume.status;
                    // A copy is taken of a source that differs from the one the copy being taken, or
                    // else the latest revision, holds. A failed copy no longer matters then.
                    let needs_copy = status.copying().map_or_else(
                        || {
                            status
                                .latest()
                                .is_none_or(|revision| revision.fingerprint != fingerprint.digest)
                        },
                        |copy| copy.fingerprint != fingerprint.digest,
                    );
                    let failed = status.copy.as_ref().is_some_and(|copy| copy.failure.is_some());
                    if !spec_changed && !needs_copy && !failed && status.provenance.as_ref() == Some(&provenance) {
                        return self.view(current).await;
                    }
                    if needs_copy {
                        let number = status.latest().map_or(1, |revision| revision.number + 1);
                        status.copy = Some(requested_copy(number, &fingerprint));
                    } else if failed {
                        status.copy = None;
                    }
                    status.provenance = Some(provenance.clone());
                    if spec_changed {
                        current.volume.spec = desired.spec.clone();
                        current.volume.metadata.generation += 1;
                    }
                    (current, expected_version)
                }
                Err(Error::VolumeNotFound(_)) => {
                    let mut volume = desired.clone();
                    volume.status.copy = Some(requested_copy(1, &fingerprint));
                    let record = VolumeRecord {
                        id: VolumeId::generate(),
                        version: 0,
                        volume,
                        discarded: Vec::new(),
                    };
                    (record, 0)
                }
                Err(error) => return Err(error),
            };
            let id = record.id;
            match self.store.put(record, expected_version).await {
                Ok(()) => {
                    self.wakeup.notify(id);
                    return self.view(self.store.get(id).await?).await;
                }
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Fingerprints a resolved source, refusing one the copy must not hold: agentd's home, or a
    /// secret file.
    async fn fingerprint_source(&self, source: PathBuf) -> Result<fingerprint::Fingerprint, Error> {
        if let Some(home) = &self.home {
            let home = tokio::fs::canonicalize(home).await.unwrap_or_else(|_| home.clone());
            if home.starts_with(&source) || source.starts_with(&home) {
                return Err(Error::Invalid(format!(
                    "spec.source {} overlaps agentd's home {}, which holds its database and the copies; copy another \
                     directory",
                    source.display(),
                    home.display()
                )));
            }
        }
        crate::control_plane::reject_secrets_in_copy(self.agents.as_ref(), "spec.source", &source).await?;
        fingerprint::take(source).await
    }

    /// Gets a volume with its revisions and the agents that use them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::VolumeNotFound`] when no volume has the name.
    pub async fn get(&self, name: &str) -> Result<Volume, Error> {
        let record = self.store.get_by_name(name).await?;
        self.view(record).await
    }

    /// Lists every volume, by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the volumes or agents cannot be read.
    pub async fn list(&self) -> Result<Vec<Volume>, Error> {
        let users = super::revision_users(&self.agents.list().await?);
        Ok(self
            .store
            .list()
            .await?
            .into_iter()
            .map(|record| report(record, &users, &self.progress))
            .collect())
    }

    /// Requests a volume's deletion. New agents can no longer mount it, and it goes once no agent
    /// uses any of its revisions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::VolumeNotFound`] when no volume has the name.
    pub async fn delete(&self, name: &str) -> Result<(), Error> {
        loop {
            let mut record = self.store.get_by_name(name).await?;
            if record.is_deleting() {
                return Ok(());
            }
            let expected_version = record.version;
            record.volume.metadata.deletion_timestamp = Some(OffsetDateTime::now_utc());
            let id = record.id;
            match self.store.put(record, expected_version).await {
                Ok(()) => {
                    self.wakeup.notify(id);
                    return Ok(());
                }
                Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn view(&self, record: VolumeRecord) -> Result<Volume, Error> {
        let users = super::revision_users(&self.agents.list().await?);
        Ok(report(record, &users, &self.progress))
    }
}

/// The volume a record holds, with the agents using each revision and the progress of its latest copy.
fn report(record: VolumeRecord, users: &BTreeMap<(VolumeId, u64), Vec<String>>, progress: &CopyProgress) -> Volume {
    let mut volume = record.volume;
    for revision in &mut volume.status.revisions {
        revision.agents = users.get(&(record.id, revision.number)).cloned().unwrap_or_default();
        revision.progress = progress.get(record.id, revision.number);
    }
    if let Some(copy) = &mut volume.status.copy {
        copy.progress = progress.get(record.id, copy.number);
    }
    volume
}

fn requested_copy(number: u64, fingerprint: &fingerprint::Fingerprint) -> CopyStatus {
    CopyStatus {
        number,
        created: OffsetDateTime::now_utc(),
        fingerprint: fingerprint.digest.clone(),
        files: fingerprint.files,
        bytes: fingerprint.bytes,
        failure: None,
        progress: None,
    }
}

fn validate(volume: &Volume) -> Result<(), Error> {
    if volume.api_version != crate::API_VERSION {
        return Err(Error::Invalid(format!("apiVersion must be {:?}", crate::API_VERSION)));
    }
    if volume.kind != KIND {
        return Err(Error::Invalid(format!("kind must be {KIND:?}")));
    }
    ::sandbox::SandboxName::new(volume.metadata.name.clone())
        .map_err(|error| Error::Invalid(format!("metadata.name: {error}")))?;
    let VolumeSpec::Copy { source } = &volume.spec;
    if source.as_os_str().is_empty() {
        return Err(Error::Invalid("spec.source must not be empty".into()));
    }
    Ok(())
}

async fn resolve_source(source_directory: &std::path::Path, source: &std::path::Path) -> Result<PathBuf, Error> {
    let unresolved = crate::manifest::resolve_host_path(source_directory, source, "spec.source")?;
    let resolved = tokio::fs::canonicalize(&unresolved).await.map_err(|error| {
        Error::Invalid(format!(
            "spec.source {} cannot be resolved: {error}",
            unresolved.display()
        ))
    })?;
    if !tokio::fs::metadata(&resolved).await?.is_dir() {
        return Err(Error::Invalid(format!(
            "spec.source {} must identify a directory",
            resolved.display()
        )));
    }
    Ok(resolved)
}
