//! The nested Sandbox runtime's device.

use ::sandbox::SandboxHandle;

use crate::Error;

use super::{path_exists, run_checked, systemd_available, wait_for_systemd};

/// The device a nested Sandbox runtime opens, present only when the Agent has nested Sandboxes.
const KVM: &str = "/dev/kvm";

/// Gives `/dev/kvm` to the `agent` group, as Podman's socket is, so the Agent can run nested
/// Sandboxes without its image granting the device. The guest creates the device anew at every
/// boot, and setup runs after every boot, so the grant is made on every pass. On a guest already
/// running systemd it first waits for boot to finish, so that an image's own tmpfiles or udev
/// rule for the device runs before the grant; setup that starts before the image's entrypoint
/// has become systemd can still be undone by such a rule until the next pass.
///
/// Reports whether the device is present.
pub(super) async fn grant_kvm(sandbox: &SandboxHandle) -> Result<bool, Error> {
    if !path_exists(sandbox, "-c", KVM).await? {
        return Ok(false);
    }
    if systemd_available(sandbox).await? {
        wait_for_systemd(sandbox).await?;
    }
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/bin/chown", "root:agent", KVM]).await?;
    run_checked(sandbox, "/usr/bin/sudo", ["-n", "/bin/chmod", "0660", KVM]).await?;
    Ok(true)
}
