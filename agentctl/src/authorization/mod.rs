//! Authorization for operations originating inside agent sandboxes.

mod agent_policy;

pub(crate) use agent_policy::host_pattern_covers;
pub use agent_policy::{AgentPolicyEngine, SecretHosts};
