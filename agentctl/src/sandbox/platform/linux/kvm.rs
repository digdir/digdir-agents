//! The nested Sandbox runtime's device.

use ::sandbox::SandboxHandle;

use crate::Error;

use super::{path_exists, run_checked};

/// The device a nested Sandbox runtime opens, present only when the Agent has nested Sandboxes.
const KVM: &str = "/dev/kvm";

/// Gives `/dev/kvm` to the `agent` group, as Podman's socket is, so the Agent can run nested
/// Sandboxes without its image granting the device. The guest creates the device anew at every
/// boot, and setup runs after every boot, so the grant is made on every pass.
pub(super) async fn grant_kvm(sandbox: &SandboxHandle) -> Result<(), Error> {
    if !path_exists(sandbox, "-c", KVM).await? {
        return Ok(());
    }
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/bin/chown", "root:agent", KVM]).await?;
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/bin/chmod", "0660", KVM]).await
}
