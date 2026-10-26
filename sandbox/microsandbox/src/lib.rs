//! Microsandbox implementation of the backend-neutral Sandbox SDK.
//!
//! The integration deliberately contains no agent automation. It uses an
//! explicit Microsandbox home, materializes stopped sandboxes in adapter-owned
//! state, and boots the underlying microVM only when the generic Backend is
//! started.

mod backend;
mod client;
mod encoding;
mod error;
mod execution;
mod files;
mod guest_tcp;
mod heartbeat;
mod image;
mod image_cache;
mod network_backend;
mod network_endpoint;
mod platform;
mod state;
mod volumes;

pub use backend::{MicrosandboxProvider, MicrosandboxProviderBuilder};

/// Checks whether this host can let a sandbox run sandboxes of its own.
///
/// [`SandboxFeature::NestedSandboxes`](sandbox::SandboxFeature) reports the same. A Microsandbox
/// sandbox does so with hardware virtualization: the guest gets `/dev/kvm`.
///
/// # Errors
///
/// Returns why the host cannot.
pub fn nested_sandbox_support() -> Result<(), String> {
    microsandbox::runtime::nested_virt_host_support()
}

/// `RUST_LOG` directives that keep the Microsandbox runtime's helper processes quiet at the
/// default level. The runtime's agent client logs every relay connection at INFO.
pub const LOG_DIRECTIVES: &str = "microsandbox_agent_client=warn";
pub use guest_tcp::{GuestTcpDialer, GuestTcpStream};
pub use network_backend::{MicrosandboxNetworkBackend, SecretBinding};
