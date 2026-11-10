//! Agent automation and the local declarative control plane.
//!
//! This crate depends on the generic sandbox crates. The reverse dependency is
//! deliberately impossible in the workspace graph.

pub mod accounts;
pub mod authorization;
pub mod bundled;
pub mod control_api;
pub mod control_plane;
mod controller;
pub mod definitions;
mod dockerfile;
mod environment;
pub mod exposed;
pub mod github;
pub mod harness;
mod host_pattern;
pub mod local;
pub mod manifest;
pub mod names;
pub mod persistence;
pub mod platform_api;
pub mod progress;
pub mod reference;
pub mod release;
pub mod resources;
pub mod sandbox;
pub mod secrets;
pub mod sessions;
pub mod sign_in;
pub mod ssh;
pub mod upgrade;
pub mod vnc;
pub mod volumes;

pub use control_plane::AgentId;
pub use controller::{FailureKind, ReconcileFailure};
pub use harness::{Effort, Harness, HarnessAuthMode, HarnessSpec, Model, ModelSelection};
pub use manifest::{
    API_VERSION, AccessSpec, AccountSpec, Agent, AgentVariant, AgentVariantName, Condition, ConditionStatus,
    EnvironmentSpec, HomeSpec, InstructionsSpec, KIND, Lifetime, Metadata, MountSpec, MountedVolume, NestedSandboxes,
    NetworkAllow, NetworkMode, NetworkSpec, Origin, PlatformManifestSpec, Provenance, ResolvedManifest,
    ResourcesManifestSpec, RunState, SandboxManifestSpec, SecretSpec, SkillSpec, Spec, Status, VARIANT_KIND,
};
pub use reference::DefinitionReference;

/// Version embedded in a matched `agentctl`/`agentd` build.
#[must_use]
pub const fn build_version() -> &'static str {
    match release_version() {
        Some(version) => version,
        None => env!("CARGO_PKG_VERSION"),
    }
}

/// Release version embedded by packaging, absent from ordinary development builds.
#[must_use]
pub const fn release_version() -> Option<&'static str> {
    option_env!("AGENT_VERSION")
}

use thiserror::Error;

/// Errors exposed by the agent control plane.
#[derive(Debug, Error)]
pub enum Error {
    /// A request or the agent resource is invalid: desired state must change before another
    /// reconciliation pass can succeed, so waiters fail fast and nothing retries. The message says
    /// what is wrong on its own, as it is shown without a prefix.
    #[error("{0}")]
    Invalid(String),
    /// The requested agent does not exist.
    #[error("Agent not found")]
    NotFound,
    /// The named volume does not exist.
    #[error("Volume {0:?} not found")]
    VolumeNotFound(String),
    /// A desired-state field that only a new agent takes changed. The message says how to change it, since the
    /// person applying it has to.
    #[error("{0} cannot change on an existing agent. Delete the agent and apply again, which starts its disk over")]
    Immutable(&'static str),
    /// A compare-and-swap operation observed a newer generation.
    #[error("Agent resource changed concurrently")]
    Conflict,
    /// Persistent control-plane state could not be read or written.
    #[error("control-plane database failed: {0}")]
    Database(String),
    /// Immutable agent setup failed inside a running sandbox. Treated as transient:
    /// the background controller retries and waiters keep following.
    #[error("Agent sandbox setup failed: {0}")]
    SandboxSetup(String),
    /// The sandbox's guest stopped making progress while its VM kept running.
    /// Treated as transient: the guest may recover, and the background
    /// controller keeps observing it.
    #[error("Agent sandbox is not responding: {0}")]
    SandboxUnresponsive(String),
    /// The named agent is stopped, so nothing runs in its sandbox until it is
    /// started. Desired state must change, so waiters fail fast.
    #[error("Agent {0:?} is stopped; run `agentctl start agent/{0}`")]
    Stopped(String),
    /// A generic sandbox operation failed.
    #[error("Sandbox operation failed: {0}")]
    Sandbox(#[from] ::sandbox::Error),
    /// Session lifecycle or attachment failed.
    #[error("Session operation failed: {0}")]
    Session(String),
    /// Blocking work stopped or panicked.
    #[error(transparent)]
    Blocking(#[from] common::blocking::Error),
    /// Local persistence or transport failed.
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// A required daemon subsystem stopped unexpectedly.
    #[error("Agent daemon subsystem failed: {0}")]
    Daemon(String),
    /// A JSON protocol document was invalid.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A YAML manifest was invalid.
    #[error("invalid YAML: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    /// The agent would be able to read files that may hold real keys, which the request did not
    /// name as agreed to. A client asks the person about them and names them in the request again.
    #[error("{}", exposed::refusal(.0))]
    ExposedFiles(Vec<exposed::ExposedFile>),
    /// The Control API returned an error. agentd's message is shown as it is.
    #[error("{0}")]
    Rpc(#[from] control_api::ResponseError),
}
