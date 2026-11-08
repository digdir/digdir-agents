//! Volumes: storage that agents mount by name and share.
//!
//! A `copy` volume holds a read-only copy of a host directory. Applying it again takes a new copy,
//! a revision, when the directory has changed. A revision never changes, so an agent keeps the one
//! it was created with while newer agents get the latest, and every agent on a revision shares one
//! copy on disk. A revision goes once it is not the latest and nothing uses it.
//!
//! A copy becomes a revision only once it is taken. Until then, and after it fails, it is the
//! volume's one copy in progress, so a failed copy never stands between agents and the latest
//! revision.

mod controller;
mod fingerprint;
pub mod memory;
mod progress;
mod reconciler;
mod revisions;
mod service;

use std::{path::PathBuf, rc::Rc};

use ::sandbox::LocalFuture;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Error, Metadata};

pub use controller::{Controller, ErrorHandler, Wakeup};
pub use progress::{CHECK_SOURCE, COPY_SOURCE, CopyProgress, VERIFY_SOURCE};
pub use reconciler::Reconciler;
pub use revisions::VolumeRevisions;
pub use service::{ApplyVolumeRequest, VolumeService};

/// Kind of a volume manifest.
pub const KIND: &str = "Volume";

/// A volume as its manifest declares it and agentd observes it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Volume {
    /// Manifest schema version.
    pub api_version: String,
    /// Resource kind. Always [`KIND`].
    pub kind: String,
    /// Resource identity and API-managed metadata.
    pub metadata: Metadata,
    /// What the volume holds.
    pub spec: VolumeSpec,
    /// Its revisions and the agents that use them.
    #[serde(default, skip_serializing_if = "VolumeStatus::is_empty")]
    pub status: VolumeStatus,
}

/// What a volume holds, by kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum VolumeSpec {
    /// A read-only copy of a host directory. Applying the volume again takes a new copy when the
    /// directory has changed.
    Copy {
        /// Host directory, resolved relative to the manifest directory at apply time.
        source: PathBuf,
    },
}

/// Where a volume was last applied from, the copies agents can mount, and the copy being taken.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VolumeStatus {
    /// Where the volume was last applied from. Applying its manifest again takes a new copy when the
    /// source changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::Provenance>,
    /// Copies agents can mount, oldest first. New agents get the last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revisions: Vec<Revision>,
    /// The copy being taken, or the last one that failed, which applying the volume again retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<CopyStatus>,
}

impl VolumeStatus {
    /// Reports whether nothing has been observed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.provenance.is_none() && self.revisions.is_empty() && self.copy.is_none()
    }

    /// The manifest the volume was last applied from, when the client reported it.
    #[must_use]
    pub fn manifest_path(&self) -> Option<&std::path::Path> {
        self.provenance.as_ref()?.manifest_path.as_deref()
    }

    /// The revision new agents get.
    #[must_use]
    pub fn latest(&self) -> Option<&Revision> {
        self.revisions.last()
    }

    /// The copy being taken, unless it failed.
    #[must_use]
    pub fn copying(&self) -> Option<&CopyStatus> {
        self.copy.as_ref().filter(|copy| copy.failure.is_none())
    }

    /// The names of the agents using any of the revisions, sorted.
    #[must_use]
    pub fn agents(&self) -> Vec<&str> {
        let mut agents: Vec<&str> = self
            .revisions
            .iter()
            .flat_map(|revision| revision.agents.iter().map(String::as_str))
            .collect();
        agents.sort_unstable();
        agents.dedup();
        agents
    }
}

/// A copy agents can mount. It never changes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Revision {
    /// Number of the revision, counting from 1 for each volume.
    pub number: u64,
    /// When the copy was requested.
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
    /// Fingerprint of the source the copy holds.
    pub fingerprint: String,
    /// The directory copied, which an earlier revision keeps when the volume's source changes.
    pub source: PathBuf,
    /// Regular files in the copy.
    pub files: u64,
    /// Bytes in those files.
    pub bytes: u64,
    /// Where the copy is stored.
    pub storage: RevisionStorage,
    /// Agents whose sandboxes mount the revision, as agentd reports it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Progress of taking the copy, while it is the latest copy of the volume agentd took since it
    /// started, as agentd reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Provisioning>,
}

/// A copy being taken, or one that failed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CopyStatus {
    /// Number of the revision it becomes, the one after the latest.
    pub number: u64,
    /// When the copy was requested.
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
    /// Fingerprint of the source when the copy was requested, which the copy is checked against.
    pub fingerprint: String,
    /// Regular files in the source then.
    pub files: u64,
    /// Bytes in those files.
    pub bytes: u64,
    /// Why the copy failed, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// Progress of taking it, as agentd reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Provisioning>,
}

impl CopyStatus {
    /// Reports whether `other` is the same request, rather than a newer one for the same number.
    pub(crate) fn same_request(&self, other: &Self) -> bool {
        self.number == other.number && self.created == other.created && self.fingerprint == other.fingerprint
    }
}

/// Immutable identity of one volume incarnation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VolumeId(Uuid);

impl VolumeId {
    pub(crate) fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl std::fmt::Display for VolumeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for VolumeId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// A volume's stored state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VolumeRecord {
    /// Immutable identity of this volume incarnation.
    pub id: VolumeId,
    /// Write counter that guards every replacement against a concurrent one.
    pub version: u64,
    /// The volume, with its source resolved to a canonical path, and its status without what is
    /// only reported.
    pub volume: Volume,
    /// Copies no revision holds, which cleanup deletes: one that failed and could not be deleted
    /// then, or one a newer request replaced while it was taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded: Vec<RevisionStorage>,
}

impl VolumeRecord {
    /// Reports whether deletion was requested.
    #[must_use]
    pub const fn is_deleting(&self) -> bool {
        self.volume.metadata.deletion_timestamp.is_some()
    }

    /// The directory the volume was last applied from, which a relative source resolves against.
    #[must_use]
    pub fn source_directory(&self) -> Option<&std::path::Path> {
        self.volume
            .status
            .provenance
            .as_ref()
            .map(|provenance| provenance.source_directory.as_path())
    }
}

/// Where a revision's copy is stored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RevisionStorage {
    /// Sandbox Provider holding the copy.
    pub provider: crate::sandbox::ProviderId,
    /// The Provider's Volume holding it.
    pub volume: ::sandbox::volume::VolumeId,
}

/// The volume revisions an agent's volume mounts use, by mount target. An optional mount whose
/// volume could not be mounted when the agent got its revisions has none, so it stays left out.
pub type VolumePins = std::collections::BTreeMap<::sandbox::SandboxPath, Option<PinnedRevision>>;

/// The revision of a volume an agent's mount uses, fixed when the agent first gets one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PinnedRevision {
    /// The volume.
    pub volume: VolumeId,
    /// The revision's number.
    pub revision: u64,
    /// Where its copy is stored.
    pub storage: RevisionStorage,
}

/// Stores volume records, each replaced only if it has not changed since it was read.
pub trait VolumeStore {
    /// Gets a volume record by immutable identity.
    fn get(&self, id: VolumeId) -> LocalFuture<'_, Result<VolumeRecord, Error>>;

    /// Gets a volume record by its user-facing name.
    fn get_by_name<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<VolumeRecord, Error>>;

    /// Lists every volume record, by name.
    fn list(&self) -> LocalFuture<'_, Result<Vec<VolumeRecord>, Error>>;

    /// Creates a record when `expected_version` is 0, otherwise replaces it if its stored version is
    /// still `expected_version`. The stored record's version is `expected_version + 1`.
    fn put(&self, record: VolumeRecord, expected_version: u64) -> LocalFuture<'_, Result<(), Error>>;

    /// Removes a record if its stored version is still `expected_version`.
    fn remove(&self, id: VolumeId, expected_version: u64) -> LocalFuture<'_, Result<(), Error>>;
}

pub(crate) type SharedVolumeStore = Rc<dyn VolumeStore>;

/// Decodes a YAML or JSON volume manifest.
///
/// # Errors
///
/// Returns an error when its syntax or structure is invalid.
pub fn decode(bytes: &[u8]) -> Result<Volume, Error> {
    let volume: Volume = serde_yaml_ng::from_slice(bytes)?;
    if volume.kind != KIND {
        return Err(Error::Invalid(format!("kind must be {KIND:?}")));
    }
    Ok(volume)
}

/// The names of the agents using each revision, by volume and revision number. An agent being
/// deleted still counts until its sandbox is released.
pub(crate) fn revision_users(
    agents: &[crate::control_plane::AgentRecord],
) -> std::collections::BTreeMap<(VolumeId, u64), Vec<String>> {
    let mut users = std::collections::BTreeMap::<_, Vec<String>>::new();
    for agent in agents {
        for pin in agent.volume_revisions.values().flatten() {
            users
                .entry((pin.volume, pin.revision))
                .or_default()
                .push(agent.agent.metadata.name.clone());
        }
    }
    for names in users.values_mut() {
        names.sort();
        names.dedup();
    }
    users
}

/// The Provider Volume name of a revision's copy. A retry reuses the revision's number, so the
/// name finds the copy an interrupted attempt left, which is replaced, as it may be of an older
/// source, or deleted with its volume.
pub(crate) fn revision_volume_name(id: VolumeId, number: u64) -> Result<::sandbox::volume::VolumeName, Error> {
    ::sandbox::volume::VolumeName::new(format!("copy-{}-{number}", id.as_uuid().simple()))
        .map_err(|error| Error::Database(format!("volume revision cannot name its copy: {error}")))
}
