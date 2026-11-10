//! Authorization for operations originating inside agent sandboxes.

mod agent_policy;
mod secret_mirror;

pub use agent_policy::{AgentPolicyEngine, PolicyUpdateReceiver, PolicyUpdates, SecretHosts};
pub use secret_mirror::SecretMirror;
