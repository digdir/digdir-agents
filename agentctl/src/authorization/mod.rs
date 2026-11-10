//! Authorization for operations originating inside agent sandboxes.

mod agent_policy;

pub use agent_policy::{AgentPolicyEngine, SecretHosts};
pub(crate) use agent_policy::{host_matches, host_pattern_covers};
