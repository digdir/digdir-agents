//! Observable agent provisioning progress.
//!
//! The Sandbox SDK reports progress as events and defines what they mean as a
//! folded [`::sandbox::progress::Progress`]. The reconciler folds every event of
//! a pass into the agent's [`Provisioning`] in [`ProvisioningState`]; readers
//! see its current value, and a daemon-wide revision tells them when it
//! changed. A resync of a Ready agent is published only if it fails. Durable
//! readiness and failure are the agent's stored conditions.

mod observer;
mod state;

pub use observer::SandboxObserver;
pub use state::{Provisioning, ProvisioningState};

/// Platform setup inside the sandbox: harnesses, home, instructions and Skills.
pub const SETUP: ::sandbox::Phase = ::sandbox::Phase::new("agentSetup", "Set up agent");
/// Declared SSH access to the sandbox.
pub const SSH_ACCESS: ::sandbox::Phase = ::sandbox::Phase::new("sshAccess", "Configure SSH access");
/// Stopping the sandbox of an agent whose run state is Stopped.
pub const SANDBOX_STOP: ::sandbox::Phase = ::sandbox::Phase::new("sandboxStop", "Stop sandbox");
/// Declared VNC access to the agent's desktop.
pub const VNC_ACCESS: ::sandbox::Phase = ::sandbox::Phase::new("vncAccess", "Configure VNC access");

/// One agent's stored status and the progress of its latest pass, as of one
/// revision, returned by `agents.v1.progress`.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentProgress {
    /// Revision to follow from next.
    pub revision: crate::resources::Revision,
    /// Stored status: conditions and failure class.
    pub status: crate::Status,
    /// Progress of the latest pass, with the output the caller has not seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning: Option<Provisioning>,
}

/// Where a follower is in one pass's output.
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutputPosition {
    /// Pass the position belongs to.
    pub pass: crate::resources::Revision,
    /// First output line the follower has not seen.
    pub sequence: u64,
}
