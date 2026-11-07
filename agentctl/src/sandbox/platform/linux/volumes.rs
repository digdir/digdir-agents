//! Writable volume mounts: a writable layer over a volume's copy, which never changes.

use ::sandbox::SandboxHandle;

use crate::{Error, control_plane};

use super::{USER, run_checked};

/// Puts each writable mount's writable layer over its copy, which is mounted at the mount's target, on
/// every pass, as the guest forgets its mounts when it stops. The changes stay on the agent's root
/// filesystem, below `/var/lib/agent-volumes/changes`, so they outlast restarts and go when the agent
/// is recreated. Only a direct root filesystem can keep them, which the manifest checks.
const MOUNT_SCRIPT: &str = r#"set -eu
user=$1
shift
for target in "$@"; do
  # The overlay is mounted over the copy, so the copy's mount is listed there too.
  if findmnt -no FSTYPE --mountpoint "$target" | grep -qx overlay; then continue; fi
  changes=/var/lib/agent-volumes/changes$target
  mkdir -p "$changes/upper" "$changes/work"
  chown "$user:$user" "$changes/upper"
  mount -t overlay overlay -o "lowerdir=$target,upperdir=$changes/upper,workdir=$changes/work" "$target"
done
"#;

/// Mounts the writable layer of each writable volume mount the agent's sandbox has.
pub(super) async fn mount_writable_volumes(
    record: &control_plane::AgentRecord,
    sandbox: &SandboxHandle,
    steps: &::sandbox::SandboxProgress,
) -> Result<(), Error> {
    let targets: Vec<&str> = record
        .agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            crate::MountSpec::Volume {
                target,
                read_only: false,
                ..
            } if record.volume_revisions.get(target).is_some_and(Option::is_some) => Some(target.as_str()),
            _ => None,
        })
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    let step = steps.start_step("Mount writable volumes").await;
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["-n", "/bin/sh", "-c", MOUNT_SCRIPT, "sh", USER]
            .into_iter()
            .chain(targets),
    )
    .await?;
    step.complete().await;
    Ok(())
}
