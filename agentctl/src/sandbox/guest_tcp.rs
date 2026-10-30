//! Connections agentd dials into agent sandboxes for its clients.

use std::rc::Rc;

use crate::{AgentId, Error, control_plane};

use super::GuestTarget;

/// Resolves agents, addressed by name, to the sandboxes TCP connections are dialed in.
pub struct GuestTcp {
    agents: Rc<dyn control_plane::AgentStore>,
    sandboxes: Rc<super::Service>,
}

impl GuestTcp {
    /// Creates a dialer over the stored agents and their sandboxes.
    #[must_use]
    pub const fn new(agents: Rc<dyn control_plane::AgentStore>, sandboxes: Rc<super::Service>) -> Self {
        Self { agents, sandboxes }
    }

    /// Resolves the named agent to its running sandbox, which another thread
    /// then dials. With `uid`, an agent created again under the same name
    /// since is refused, as a pod's port forward ends with the pod.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent is missing, replaced, deleting or
    /// stopped, or its sandbox is not running.
    pub async fn resolve(&self, name: &str, uid: Option<AgentId>) -> Result<GuestTarget, Error> {
        let record = self.agents.get_by_name(name).await?;
        if uid.is_some_and(|uid| uid != record.id) {
            return Err(Error::Invalid(format!(
                "agent {name:?} was replaced by a new agent of the same name. Start a new forward to reach it"
            )));
        }
        if record.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Conflict);
        }
        record.reject_stopped()?;
        self.sandboxes.guest_tcp_target(&record).await
    }
}
