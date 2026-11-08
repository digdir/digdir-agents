//! Mutable storage with a lifecycle independent of a sandbox image.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{InvalidSandboxName, SandboxName};

/// Stable, portable caller-provided Volume name.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VolumeName(SandboxName);

impl VolumeName {
    /// Creates a name accepted by local and Kubernetes-oriented providers.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is a lowercase DNS label.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidVolumeName> {
        SandboxName::new(value).map(Self).map_err(InvalidVolumeName)
    }

    /// Returns the name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for VolumeName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for VolumeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for VolumeName {
    type Err = InvalidVolumeName;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// A Volume name was not a portable lowercase DNS label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidVolumeName(InvalidSandboxName);

impl fmt::Display for InvalidVolumeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid Volume name: {}", self.0)
    }
}

impl std::error::Error for InvalidVolumeName {}

/// Identifies a Volume independently of backend-specific identifiers.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VolumeId(Uuid);

impl VolumeId {
    fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the UUID representation.
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

/// Backend-neutral view of a materialized Volume.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Volume {
    /// Stable backend-neutral identifier.
    pub id: VolumeId,
    /// Stable caller-provided name.
    pub name: VolumeName,
    /// What the Volume held when it was created.
    #[serde(default, skip_serializing_if = "VolumeContents::is_empty")]
    pub contents: VolumeContents,
}

/// What a Volume holds when it is created.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VolumeContents {
    /// Empty storage that sandboxes can write.
    #[default]
    Empty,
    /// A copy of a host directory, taken when the Volume is created. It never changes, so
    /// sandboxes mount it read-only, and any number of them can share it.
    DirectoryCopy {
        /// Absolute host directory the copy is taken from.
        source: std::path::PathBuf,
        /// The sandbox user and group that own every entry of the copy.
        #[serde(default)]
        owner: Owner,
    },
}

/// A sandbox user and group, by their numeric IDs. The default is root.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Owner {
    /// The user ID.
    pub uid: u32,
    /// The group ID.
    pub gid: u32,
}

impl VolumeContents {
    /// Reports whether the Volume starts empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Reports whether sandboxes may only mount the Volume read-only.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        matches!(self, Self::DirectoryCopy { .. })
    }
}

/// Inputs used to ensure a Volume exists.
#[derive(Debug, Eq, PartialEq)]
pub struct EnsureVolumeRequest {
    /// Backend-neutral identity used when a new Volume is materialized.
    id: VolumeId,
    /// Stable caller-provided name.
    pub name: VolumeName,
    /// What a new Volume holds. An existing Volume with other contents is an error.
    pub contents: VolumeContents,
}

impl EnsureVolumeRequest {
    /// Creates a request with a freshly assigned Volume identifier.
    #[must_use]
    pub fn new(name: VolumeName) -> Self {
        Self {
            id: VolumeId::generate(),
            name,
            contents: VolumeContents::Empty,
        }
    }

    /// Sets what a new Volume holds.
    #[must_use]
    pub fn with_contents(mut self, contents: VolumeContents) -> Self {
        self.contents = contents;
        self
    }

    /// Returns the identifier to use if the Volume is newly materialized.
    #[must_use]
    pub const fn id(&self) -> &VolumeId {
        &self.id
    }

    /// Decomposes the request for a Backend implementation.
    #[must_use]
    pub fn into_parts(self) -> (VolumeId, VolumeName, VolumeContents) {
        (self.id, self.name, self.contents)
    }
}
