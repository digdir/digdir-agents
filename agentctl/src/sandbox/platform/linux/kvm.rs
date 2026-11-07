//! The nested sandbox runtime's device.

use ::sandbox::SandboxHandle;

use crate::Error;

use super::{path_exists, run_checked, systemd_available, wait_for_systemd};

/// The device a nested sandbox runtime opens, present only when the agent has nested sandboxes.
const KVM: &str = "/dev/kvm";
/// The guest kernel's transparent huge page policy, present only when the kernel has huge pages.
const HUGE_PAGES: &str = "/sys/kernel/mm/transparent_hugepage/enabled";

/// Prepares the guest to run nested sandboxes when it has `/dev/kvm`, and reports whether it has.
pub(super) async fn prepare_nested_sandboxes(sandbox: &SandboxHandle) -> Result<bool, Error> {
    if !grant_kvm(sandbox).await? {
        return Ok(false);
    }
    enable_huge_pages(sandbox).await?;
    Ok(true)
}

/// Gives `/dev/kvm` to the `agent` group, as Podman's socket is, so the agent can run nested
/// sandboxes without its image granting the device. The guest creates the device anew at every
/// boot, and setup runs after every boot, so the grant is made on every pass. On a guest already
/// running systemd it first waits for boot to finish, so that an image's own tmpfiles or udev
/// rule for the device runs before the grant; setup that starts before the image's entrypoint
/// has become systemd can still be undone by such a rule until the next pass.
///
/// Reports whether the device is present.
async fn grant_kvm(sandbox: &SandboxHandle) -> Result<bool, Error> {
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

/// Lets the guest back the memory of its nested sandboxes with huge pages, by switching the guest kernel's
/// transparent huge page policy to `always`. Nested sandbox runtimes don't ask for huge pages for their guests'
/// memory, and with 4 KiB pages Hypervisor.framework on macOS handles every memory fault of a nested sandbox by
/// searching each mapping it holds, which makes memory-heavy work there hundreds of times slower. The policy lasts
/// until the guest restarts, and changing nested sandboxes restarts it, so setup sets it on every pass that finds
/// the device.
async fn enable_huge_pages(sandbox: &SandboxHandle) -> Result<(), Error> {
    if !path_exists(sandbox, "-f", HUGE_PAGES).await? {
        return Ok(());
    }
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["-n", "/bin/sh", "-c", &format!("echo always > {HUGE_PAGES}")],
    )
    .await
}
