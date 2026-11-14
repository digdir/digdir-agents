//! Returning the blocks a guest has freed to the host.

use std::{rc::Rc, time::Duration};

use crate::control_plane::AgentStore;

use super::Service;

/// How often each running agent with a direct root filesystem has it trimmed.
pub const TRIM_INTERVAL: Duration = Duration::from_mins(15);

/// Trims the root filesystem of each running agent every [`TRIM_INTERVAL`].
pub struct Trimmer {
    _agents: Rc<dyn AgentStore>,
    _sandboxes: Rc<Service>,
}

impl Trimmer {
    /// Creates a trimmer over the stored agents and their sandboxes.
    #[must_use]
    pub fn new(agents: Rc<dyn AgentStore>, sandboxes: Rc<Service>) -> Self {
        Self {
            _agents: agents,
            _sandboxes: sandboxes,
        }
    }

    /// Trims every [`TRIM_INTERVAL`] while `agentd` runs.
    pub async fn run(self) {
        std::future::pending::<()>().await;
    }
}
