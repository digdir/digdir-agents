//! Connections agentd dials into agent sandboxes for its clients.

use std::{num::NonZeroU16, rc::Rc};

use crate::{AgentId, Error, control_plane};

use super::GuestConnection;

/// Dials TCP connections inside the sandboxes of agents, addressed by agent name.
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

    /// Dials `port` on the loopback interface of the named agent's sandbox.
    /// With `uid`, an agent created again under the same name since is refused,
    /// as a pod's port forward ends with the pod.
    ///
    /// Every connection runs over a sandbox connection of its own. Streams that
    /// share one stall together whenever any one of their readers falls behind,
    /// so one slow forwarded download would freeze every SSH session into the
    /// same agent. A fresh one also needs no recovery after the sandbox's
    /// runtime restarts.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent is missing, replaced, deleting or stopped, its
    /// sandbox is not running, or nothing in the guest accepts the connection.
    pub async fn connect(&self, name: &str, uid: Option<AgentId>, port: NonZeroU16) -> Result<GuestConnection, Error> {
        let record = self.agents.get_by_name(name).await?;
        if uid.is_some_and(|uid| uid != record.id) {
            return Err(Error::Invalid(format!(
                "agent {name:?} was replaced by a new agent of the same name; forward to the new one to reach it"
            )));
        }
        if record.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Conflict);
        }
        record.reject_stopped()?;
        let dialer = self.sandboxes.guest_tcp_dialer(&record).await?;
        dialer.connect("127.0.0.1", port.get()).await
    }
}
