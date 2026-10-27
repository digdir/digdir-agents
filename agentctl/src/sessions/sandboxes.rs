//! Resolving a session's owning agent to its materialized sandbox.
//!
//! The reconciler and the session service both need "the sandbox this
//! session's agent runs in"; this is the one place that lookup lives.

use std::rc::Rc;

use ::sandbox::SandboxHandle;

use crate::{AgentId, Error, control_plane::AgentRecord, control_plane::AgentStore};

/// Agent records and their sandboxes, as one lookup.
pub struct AgentSandboxes {
    agents: Rc<dyn AgentStore>,
    sandboxes: Rc<crate::sandbox::Service>,
}

impl AgentSandboxes {
    /// Pairs the agent store with the sandbox service.
    #[must_use]
    pub fn new(agents: Rc<dyn AgentStore>, sandboxes: Rc<crate::sandbox::Service>) -> Self {
        Self { agents, sandboxes }
    }

    /// Gets the active agent record by name.
    ///
    /// # Errors
    ///
    /// Returns an error when no active agent has that name.
    pub async fn agent_by_name(&self, name: &str) -> Result<AgentRecord, Error> {
        self.agents.get_by_name(name).await
    }

    /// Gets an agent record by identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent does not exist.
    pub async fn agent(&self, id: AgentId) -> Result<AgentRecord, Error> {
        self.agents.get(id).await
    }

    /// Opens the materialized sandbox of `record`.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent has no materialized sandbox or its
    /// Provider cannot open it.
    pub async fn open(&self, record: &AgentRecord) -> Result<SandboxHandle, Error> {
        self.sandboxes.open(record).await
    }
}
