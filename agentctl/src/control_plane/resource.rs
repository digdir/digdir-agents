//! Internal identity and persisted representation of an agent resource.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Agent, Error};

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
