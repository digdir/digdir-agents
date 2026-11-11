//! The clone of the repositories an image lists into the workspace, which the image starts at boot.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::{Error, sandbox::WorkspaceClone};

use super::{SYSTEMCTL, checked_output, run_checked, systemd_available, wait_for_systemd};

/// The base image's unit that clones the repositories its `/etc/agent-workspace.d` lists.
const UNIT: &str = "agent-workspace-init.service";
/// The properties of [`UNIT`] that [`parse`] reads.
const PROPERTIES: &str = "LoadState,ActiveState,Result,ConditionResult";

/// How far the image's workspace clone got; nothing when the image has no such clone or lists
/// no repositories. With `retry`, a clone that failed is started again, without waiting for it.
///
/// # Errors
///
/// Returns an error when systemd cannot be asked or the clone cannot be started again.
pub(super) async fn observe(sandbox: &SandboxHandle, retry: bool) -> Result<Option<WorkspaceClone>, Error> {
    if !systemd_available(sandbox).await? {
        return Ok(None);
    }
    // The clone starts during boot, so until boot ends it may not have started yet.
    wait_for_systemd(sandbox).await?;
    let args = ["-n", SYSTEMCTL, "show", "-p", PROPERTIES, UNIT];
    let output = sandbox
        .run_execution(ExecutionSpec::command(
            SandboxPath::new("/usr/bin/sudo"),
            args.map(str::to_owned),
        ))
        .await?;
    checked_output("/usr/bin/sudo", &args, &output)?;
    let clone = parse(&String::from_utf8_lossy(&output.stdout));
    if retry && clone == Some(WorkspaceClone::Failed) {
        run_checked(sandbox, "/usr/bin/sudo", ["-n", SYSTEMCTL, "reset-failed", UNIT]).await?;
        run_checked(sandbox, "/usr/bin/sudo", ["-n", SYSTEMCTL, "start", "--no-block", UNIT]).await?;
        return Ok(Some(WorkspaceClone::Cloning));
    }
    Ok(clone)
}

/// Reads `systemctl show` output for [`UNIT`]. A unit whose condition was not met, as when the
/// image lists no repositories, ran nothing.
fn parse(show: &str) -> Option<WorkspaceClone> {
    let value = |key: &str| {
        show.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if value("LoadState") != Some("loaded") {
        return None;
    }
    match value("ActiveState")? {
        "failed" => Some(WorkspaceClone::Failed),
        "activating" | "active" | "reloading" | "deactivating" => Some(WorkspaceClone::Cloning),
        "inactive" if value("ConditionResult") == Some("yes") && value("Result") == Some("success") => {
            Some(WorkspaceClone::Cloned)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_how_far_the_clone_got() {
        let show = |active: &str, result: &str, condition: &str| {
            parse(&format!(
                "LoadState=loaded\nActiveState={active}\nResult={result}\nConditionResult={condition}\n"
            ))
        };
        assert_eq!(show("failed", "exit-code", "yes"), Some(WorkspaceClone::Failed));
        assert_eq!(show("active", "success", "yes"), Some(WorkspaceClone::Cloning));
        assert_eq!(show("inactive", "success", "yes"), Some(WorkspaceClone::Cloned));
        // An image that lists no repositories, whose unit systemd skipped.
        assert_eq!(show("inactive", "success", "no"), None);
        assert_eq!(
            parse("LoadState=not-found\nActiveState=inactive\nResult=success\nConditionResult=no\n"),
            None
        );
        assert_eq!(parse(""), None);
    }
}
