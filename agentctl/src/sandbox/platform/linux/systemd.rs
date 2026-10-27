//! Waiting for and probing the guest's systemd.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::Error;

use super::path_exists;

const SYSTEMD_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const SYSTEMD_READY_POLL: std::time::Duration = std::time::Duration::from_secs(1);
/// `systemctl`, the only supported guest service manager today.
pub(super) const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// Present exactly when systemd is the running init; the marker systemd documents for this purpose.
pub(super) const SYSTEMD_RUNNING: &str = "/run/systemd/system";

/// Waits until systemd is PID 1 and has finished booting the guest.
///
/// Sandbox setup starts as soon as the sandbox accepts Executions, which on an
/// image-init guest is before the image entrypoint has become systemd; `systemctl`
/// then reports "System has not been booted with systemd". `is-system-running
/// --wait` blocks until startup finishes once systemd is up. It runs as root
/// because the guest has no D-Bus system bus and only root reaches systemd's
/// private socket. A `degraded` system counts as ready: a failed optional unit,
/// such as a best-effort workspace clone, must not block the Podman configuration.
pub(super) async fn wait_for_systemd(sandbox: &SandboxHandle) -> Result<(), Error> {
    let deadline = tokio::time::Instant::now() + SYSTEMD_READY_TIMEOUT;
    loop {
        // `--wait` blocks for as long as boot takes, so the deadline bounds the wait itself
        // and a still-running check is killed rather than left behind in the guest.
        let started = sandbox
            .start_execution(::sandbox::execution::StartExecutionRequest::new(
                ExecutionSpec::command(
                    SandboxPath::new("/usr/bin/sudo"),
                    ["-n", "/usr/bin/systemctl", "is-system-running", "--wait"].map(str::to_owned),
                ),
            ))
            .await?;
        let execution_id = started.id.clone();
        let output = match tokio::time::timeout_at(deadline, started.collect()).await {
            Ok(output) => output?,
            Err(_elapsed) => {
                let _ = sandbox.kill_execution(&execution_id).await;
                return Err(Error::SandboxSetup(format!(
                    "systemd did not finish booting within {}s",
                    SYSTEMD_READY_TIMEOUT.as_secs()
                )));
            }
        };
        let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() || matches!(state.as_str(), "running" | "degraded") {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let last = if state.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            } else {
                state
            };
            return Err(Error::SandboxSetup(format!(
                "systemd did not become ready within {}s: {last}",
                SYSTEMD_READY_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(SYSTEMD_READY_POLL).await;
    }
}

/// Whether the guest has `systemctl` and runs systemd as its init.
///
/// # Errors
///
/// Returns an error when either presence check cannot run.
pub(super) async fn systemd_available(sandbox: &SandboxHandle) -> Result<bool, Error> {
    Ok(path_exists(sandbox, "-x", SYSTEMCTL).await? && path_exists(sandbox, "-d", SYSTEMD_RUNNING).await?)
}
