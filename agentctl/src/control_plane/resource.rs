//! Internal identity and persisted representation of an agent resource.

use ::sandbox::mount::Mount;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Agent, Error, MountSpec};

/// Immutable identity of one agent incarnation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AgentId(Uuid);

impl AgentId {
    pub(crate) fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for AgentId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// Stored desired and observed agent state plus local source provenance.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentRecord {
    /// Immutable identity of this agent incarnation.
    pub id: AgentId,
    /// Absolute directory against which manifest-relative sources are resolved.
    pub source_directory: std::path::PathBuf,
    /// Absolute path of the manifest last applied, when the client reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<std::path::PathBuf>,
    /// Absolute path of the environment file, when it is not [`ENV_FILE`] beside the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<std::path::PathBuf>,
    /// Desired state and most recently observed status.
    pub agent: Agent,
    /// Canonical sources of the optional bind mounts whose source existed when the agent was created, by target.
    /// The others are left out of its sandbox, whose mounts cannot change, until the agent is recreated.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub optional_mount_sources: std::collections::BTreeMap<::sandbox::SandboxPath, std::path::PathBuf>,
    /// The volume revision each volume mount uses, by target, fixed once the agent first gets them all.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub volume_revisions: crate::volumes::VolumePins,
}

/// Default environment file name, resolved in the source directory.
pub const ENV_FILE: &str = ".env";

impl AgentRecord {
    /// Returns the host file that supplies declared manifest values.
    #[must_use]
    pub fn env_file_path(&self) -> std::path::PathBuf {
        self.env_file
            .clone()
            .unwrap_or_else(|| self.source_directory.join(ENV_FILE))
    }

    /// Returns the mounts the agent's sandbox gets, which leave out the optional bind mounts whose source did not
    /// exist when the agent was created, and volumes until the agent's revisions of them are fixed.
    #[must_use]
    pub fn sandbox_mounts(&self) -> Vec<Mount> {
        self.indexed_sandbox_mounts().map(|(_, mount)| mount).collect()
    }

    /// Returns [`Self::sandbox_mounts`], each with its index in `spec.sandbox.mounts`. An optional bind mount takes
    /// the source it had when the agent was created, and a volume the copy of the agent's revision of it.
    pub(crate) fn indexed_sandbox_mounts(&self) -> impl Iterator<Item = (usize, Mount)> + '_ {
        self.agent
            .spec
            .sandbox
            .mounts
            .iter()
            .enumerate()
            .filter_map(|(index, mount)| {
                let mount = match mount {
                    MountSpec::Bind {
                        source,
                        target,
                        read_only,
                        optional,
                    } => Mount::Bind {
                        source: if *optional {
                            self.optional_mount_sources.get(target)?.clone()
                        } else {
                            source.clone()
                        },
                        target: target.clone(),
                        read_only: *read_only,
                    },
                    // A copy, the only kind of volume so far, never changes. Agent setup puts a writable
                    // mount's writable layer over it.
                    MountSpec::Volume { target, .. } => Mount::Volume {
                        id: self.volume_revisions.get(target)?.as_ref()?.storage.volume.clone(),
                        target: target.clone(),
                        read_only: true,
                    },
                    MountSpec::Tmpfs { target, capacity } => Mount::Tmpfs {
                        target: target.clone(),
                        capacity: *capacity,
                    },
                };
                Some((index, mount))
            })
    }

    /// Derives the Provider-independent sandbox name for this agent incarnation.
    ///
    /// # Errors
    ///
    /// Returns an error only if the stable agent identity cannot form a valid sandbox name.
    pub fn sandbox_name(&self) -> Result<::sandbox::SandboxName, Error> {
        ::sandbox::SandboxName::new(format!("agent-{}", self.id))
            .map_err(|error| Error::Database(format!("Agent ID cannot identify its sandbox: {error}")))
    }

    /// Refuses work that needs the sandbox running while the agent is stopped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stopped`] when the agent's run state is Stopped.
    pub fn reject_stopped(&self) -> Result<(), Error> {
        if self.agent.spec.is_stopped() {
            return Err(Error::Stopped(self.agent.metadata.name.clone()));
        }
        Ok(())
    }

    /// Derives the hostname the sandbox reports: the agent name.
    ///
    /// # Errors
    ///
    /// Returns an error only if the validated agent name cannot form a hostname.
    pub fn sandbox_hostname(&self) -> Result<::sandbox::Hostname, Error> {
        ::sandbox::Hostname::new(self.agent.metadata.name.clone())
            .map_err(|error| Error::Database(format!("Agent name cannot be its sandbox hostname: {error}")))
    }
}
