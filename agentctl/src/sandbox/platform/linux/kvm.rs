//! The nested sandbox runtime's device.

use ::sandbox::SandboxHandle;

use crate::Error;

use super::{path_exists, run_checked, systemd_available, wait_for_systemd};

/// The device a nested sandbox runtime opens, present only when the agent has nested sandboxes.
const KVM: &str = "/dev/kvm";
/// The guest kernel's transparent huge page controls, present only when the kernel has huge pages.
const HUGE_PAGES: &str = "/sys/kernel/mm/transparent_hugepage";

/// Prepares the guest to run nested sandboxes when it has `/dev/kvm`, and reports whether it has. With
/// `huge_pages`, it also backs their memory with huge pages, as [`enable_huge_pages`] describes.
pub(super) async fn prepare_nested_sandboxes(sandbox: &SandboxHandle, huge_pages: bool) -> Result<bool, Error> {
    if !grant_kvm(sandbox).await? {
        return Ok(false);
    }
    if huge_pages {
        enable_huge_pages(sandbox).await;
    }
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
/// searching each mapping it holds, which makes memory-heavy work there hundreds of times slower. Other hosts'
/// hypervisors don't have that cost, so there it only spends memory.
///
/// Defragmentation is set to `always` too, so that a fault compacts memory for a huge page when the page cache has
/// taken the free ones. Huge pages the kernel assembles later instead, by collapsing or compacting in the background,
/// move memory a nested sandbox has mapped, and Hypervisor.framework answers each move by dropping all of that
/// sandbox's mappings, which stalls it.
///
/// Memory a nested sandbox touched before the switch stays in 4 KiB pages, so it has to happen before the agent
/// starts nested sandboxes, as setup does at every start. The policy lasts until the guest restarts, and changing
/// nested sandboxes restarts it. A guest that refuses the switch only runs its nested sandboxes slower, so a
/// failure is logged rather than failing setup.
async fn enable_huge_pages(sandbox: &SandboxHandle) {
    let result = async {
        if !path_exists(sandbox, "-f", &format!("{HUGE_PAGES}/enabled")).await? {
            return Ok(());
        }
        run_checked(
            sandbox,
            "/usr/bin/sudo",
            [
                "-n",
                "/bin/sh",
                "-c",
                &format!("echo always > {HUGE_PAGES}/enabled && echo always > {HUGE_PAGES}/defrag"),
            ],
        )
        .await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "could not back nested sandboxes with huge pages");
    }
}
