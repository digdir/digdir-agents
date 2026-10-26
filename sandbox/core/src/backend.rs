//! Interfaces implemented by sandbox Providers.
//!
//! Application code normally provisions through [`crate::SandboxService`] and
//! operates the resulting [`crate::SandboxHandle`].

use std::{collections::BTreeMap, future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::feature::SandboxBackendCapabilities;

use crate::{
    Error, Hostname, PendingOperation, Platform, RootFilesystem, SandboxName, SandboxPath, execution, file_transfer,
    image,
    init::InitSystem,
    mount::Mount,
    network,
    resource::{ByteQuantity, CpuQuantity},
    terminal, volume,
};

/// A non-`Send` future executed by a Tokio local runtime.
pub type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Identifies one materialization independently of backend-specific identifiers.
///
/// The sandbox lifecycle service assigns a fresh ID before calling a Backend's
/// create operation. Deleting and recreating the same [`SandboxName`] produces
/// a different ID.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SandboxId(Uuid);

impl SandboxId {
    pub(crate) fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the UUID representation.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl std::fmt::Display for SandboxId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for SandboxId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// The lifecycle state reported by a sandbox backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxState {
    /// The sandbox exists but is not running.
    Stopped,
    /// The sandbox is running.
    Running,
}

/// Host-observed evidence that a running guest is making progress.
///
/// A Backend reports it without a round trip to the guest, so a guest that no
/// longer responds still reports its last heartbeat. Only a change of the
/// sequence is meaningful: it restarts whenever the sandbox starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestHeartbeat(u64);

impl GuestHeartbeat {
    /// Wraps a heartbeat sequence observed by a Backend.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Returns the observed sequence.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// Desired compute and writable root filesystem resources assigned to one sandbox.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxResources {
    cpu: CpuQuantity,
    memory: ByteQuantity,
    root_filesystem: RootFilesystem,
    #[serde(default, skip_serializing_if = "is_false")]
    nested_sandboxes: bool,
}

impl SandboxResources {
    /// Creates a positive, normalized resource assignment.
    #[must_use]
    pub const fn new(cpu: CpuQuantity, memory: ByteQuantity, root_filesystem: RootFilesystem) -> Self {
        Self {
            cpu,
            memory,
            root_filesystem,
            nested_sandboxes: false,
        }
    }

    /// Lets the sandbox run sandboxes of its own, or not. Requires
    /// [`SandboxFeature::NestedSandboxes`](crate::SandboxFeature) when enabled.
    #[must_use]
    pub const fn with_nested_sandboxes(mut self, enabled: bool) -> Self {
        self.nested_sandboxes = enabled;
        self
    }

    /// Returns the desired CPU quantity.
    #[must_use]
    pub const fn cpu(self) -> CpuQuantity {
        self.cpu
    }

    /// Returns the desired sandbox memory quantity.
    #[must_use]
    pub const fn memory(self) -> ByteQuantity {
        self.memory
    }

    /// Returns the desired writable root filesystem capacity.
    #[must_use]
    pub const fn root_filesystem(self) -> RootFilesystem {
        self.root_filesystem
    }

    /// Reports whether the sandbox can run sandboxes of its own.
    #[must_use]
    pub const fn nested_sandboxes(self) -> bool {
        self.nested_sandboxes
    }
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde passes the field by reference")]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Materialized inputs passed to a sandbox backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSandboxRequest {
    /// Backend-neutral identity assigned by the lifecycle owner.
    pub id: SandboxId,
    /// The resolved image to run.
    pub image: image::ResolvedImage,
    /// The stable caller-provided name.
    pub name: SandboxName,
    /// Hostname reported inside the sandbox, resolved by the lifecycle owner.
    pub hostname: Hostname,
    /// Desired mutable compute and writable root filesystem resources.
    pub resources: SandboxResources,
    /// Process responsible for initializing the sandbox after backend setup.
    pub init_system: InitSystem,
    /// Filesystem attachments materialized when the sandbox is created.
    pub mounts: Vec<Mount>,
    /// Non-secret environment inherited by image init and sandbox Executions.
    pub environment: BTreeMap<String, String>,
    /// Immutable Network attachment selected by the caller.
    pub network: Option<network::NetworkAttachment>,
}

/// A backend-neutral view of a materialized sandbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sandbox {
    /// The image used to create this sandbox.
    pub image: image::ResolvedImage,
    /// The stable backend-neutral identifier.
    pub id: SandboxId,
    /// The caller-provided name.
    pub name: SandboxName,
    /// Hostname the sandbox reports to its guest.
    pub hostname: Hostname,
    /// Current desired compute and writable root filesystem resources.
    pub resources: SandboxResources,
    /// Process responsible for initializing the sandbox after backend setup.
    pub init_system: InitSystem,
    /// The current lifecycle state.
    pub state: SandboxState,
    /// The latest guest heartbeat, when the Backend observes one for a running
    /// sandbox. Absent while the guest boots and when the Backend cannot tell.
    pub guest_heartbeat: Option<GuestHeartbeat>,
    /// Filesystem attachments materialized in the sandbox.
    pub mounts: Vec<Mount>,
    /// Non-secret environment inherited by image init and sandbox Executions.
    pub environment: BTreeMap<String, String>,
    /// Immutable Network attachment materialized with the sandbox.
    pub network: Option<network::NetworkAttachment>,
}

/// Provides core sandbox lifecycle, execution, runtime file transfer, storage,
/// and mount behavior.
///
/// Network enforcement and agent automation intentionally remain outside this
/// interface. A Backend reports and opens data-plane endpoints that an
/// independently implemented [`network::NetworkBackend`] can consume.
pub trait SandboxBackend {
    /// Reports functionality implemented for a supported Platform.
    ///
    /// An offered endpoint must be the exclusive path for sandbox egress. The
    /// Backend blocks traffic not represented by the selected endpoint rather
    /// than allowing it to bypass the Network Backend.
    fn capabilities<'a>(&'a self, platform: &'a Platform)
    -> LocalFuture<'a, Result<SandboxBackendCapabilities, Error>>;

    /// Creates a stopped sandbox.
    fn create(&self, request: CreateSandboxRequest) -> PendingOperation<'_, Sandbox>;

    /// Reconciles the mutable resource assignment of an existing sandbox.
    fn update_resources<'a>(&'a self, id: &'a SandboxId, resources: SandboxResources) -> PendingOperation<'a, Sandbox>;

    /// Replaces the non-secret environment of a stopped sandbox.
    fn update_environment<'a>(
        &'a self,
        id: &'a SandboxId,
        environment: BTreeMap<String, String>,
    ) -> PendingOperation<'a, Sandbox>;

    /// Finds a sandbox by its stable caller-provided name.
    fn find<'a>(&'a self, name: &'a SandboxName) -> LocalFuture<'a, Result<Sandbox, Error>>;

    /// Inspects a sandbox by identifier.
    fn inspect<'a>(&'a self, id: &'a SandboxId) -> LocalFuture<'a, Result<Sandbox, Error>>;

    /// Starts a sandbox.
    fn start<'a>(&'a self, id: &'a SandboxId) -> PendingOperation<'a, ()>;

    /// Stops a sandbox.
    fn stop<'a>(&'a self, id: &'a SandboxId) -> LocalFuture<'a, Result<(), Error>>;

    /// Ends a running sandbox at once, without asking the guest to shut down. A backend
    /// without a faster way stops it as [`Self::stop`] does, and says so.
    fn kill<'a>(&'a self, id: &'a SandboxId) -> LocalFuture<'a, Result<(), Error>>;

    /// Deletes a sandbox.
    fn delete<'a>(&'a self, id: &'a SandboxId) -> LocalFuture<'a, Result<(), Error>>;

    /// Opens the data-plane endpoint for the sandbox's immutable Network attachment.
    fn open_network_endpoint<'a>(
        &'a self,
        id: &'a SandboxId,
    ) -> LocalFuture<'a, Result<network::NetworkEndpoint, Error>>;

    /// Starts an Execution with its SDK-assigned identity and opens its transient event stream.
    fn start_execution<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        request: execution::StartExecutionRequest,
    ) -> LocalFuture<'a, Result<execution::StartedExecution, Error>>;

    /// Starts a terminal Execution with its SDK-assigned identity and bidirectional input and output.
    fn start_terminal_execution<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        request: terminal::StartTerminalExecutionRequest,
    ) -> LocalFuture<'a, Result<terminal::StartedTerminalExecution, Error>>;

    /// Attaches the caller's terminal to an interactive Execution.
    fn attach_terminal<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        request: terminal::AttachTerminalRequest,
    ) -> LocalFuture<'a, Result<terminal::TerminalAttachOutcome, Error>>;

    /// Requests graceful termination of a live Execution.
    fn terminate_execution<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        execution_id: &'a execution::ExecutionId,
    ) -> LocalFuture<'a, Result<(), Error>>;

    /// Forces a live Execution to stop.
    fn kill_execution<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        execution_id: &'a execution::ExecutionId,
    ) -> LocalFuture<'a, Result<(), Error>>;

    /// Opens one regular file in a running sandbox for streamed reading.
    fn read_file<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        path: &'a SandboxPath,
    ) -> LocalFuture<'a, Result<file_transfer::ByteReader, Error>>;

    /// Creates or replaces one regular file in a running sandbox from a byte stream.
    ///
    /// The replacement is atomic: a concurrent reader in the sandbox observes either the previous
    /// file or the complete new one, never a truncated or partially written file. A replaced
    /// regular file keeps its mode and ownership.
    fn write_file<'a>(
        &'a self,
        sandbox_id: &'a SandboxId,
        path: &'a SandboxPath,
        contents: file_transfer::ByteReader,
    ) -> LocalFuture<'a, Result<(), Error>>;

    /// Creates or returns a named Volume, using the SDK-assigned identity when materializing it.
    fn ensure_volume(&self, request: volume::EnsureVolumeRequest) -> LocalFuture<'_, Result<volume::Volume, Error>>;

    /// Finds a Volume by name.
    fn find_volume<'a>(&'a self, name: &'a volume::VolumeName) -> LocalFuture<'a, Result<volume::Volume, Error>>;

    /// Deletes a Volume.
    fn delete_volume<'a>(&'a self, id: &'a volume::VolumeId) -> LocalFuture<'a, Result<(), Error>>;
}
