//! Host-specific process launch behavior.

/// Default `RUST_LOG` filter for `agentd` and every runtime process it spawns.
///
/// Runtime helpers inherit the daemon's stderr and install their own tracing
/// subscriber, so the filter travels through the environment rather than code.
#[must_use]
pub fn daemon_log_filter() -> String {
    format!("info,{}", crate::sandbox::microsandbox::LOG_DIRECTIVES)
}

/// Gives a child daemon the default log filter unless the caller set `RUST_LOG`.
pub fn configure_logging(command: &mut std::process::Command) {
    if std::env::var_os("RUST_LOG").is_none() {
        command.env("RUST_LOG", daemon_log_filter());
    }
}

/// Most open files macOS lets a process have, whatever its hard limit says.
#[cfg(target_os = "macos")]
const MACOS_OPEN_MAX: u64 = 10_240;

/// Raises this process's soft limit on open files to its hard limit, and
/// returns the soft limit in effect afterwards, `None` for unlimited.
///
/// agentd carries every SSH session and forwarded connection into a sandbox
/// as a stream that holds about two descriptors: the client's connection and
/// agentd's own to the sandbox. Soft limits default to as little as 256 on
/// macOS and 1024 on many Linux systems, far below the hard limit, so a few
/// hundred streams would leave agentd unable to open anything else. A limit
/// that cannot be raised is kept, and the streams agentd accepts follow it.
#[cfg(unix)]
pub fn raise_open_file_limit() -> Option<u64> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let limit = getrlimit(Resource::Nofile);
    // macOS refuses a soft limit above `OPEN_MAX` even when the hard limit is unlimited.
    #[cfg(target_os = "macos")]
    let wanted = Some(limit.maximum.map_or(MACOS_OPEN_MAX, |hard| hard.min(MACOS_OPEN_MAX)));
    #[cfg(not(target_os = "macos"))]
    let wanted = limit.maximum;
    if wanted == limit.current {
        return limit.current;
    }
    match setrlimit(
        Resource::Nofile,
        Rlimit {
            current: wanted,
            maximum: limit.maximum,
        },
    ) {
        Ok(()) => wanted,
        Err(error) => {
            tracing::debug!(%error, ?wanted, "could not raise the open-file limit");
            limit.current
        }
    }
}

/// Windows has no per-process limit on open files to raise.
#[cfg(windows)]
#[must_use]
pub const fn raise_open_file_limit() -> Option<u64> {
    None
}

/// Configures a child daemon to run independently of the invoking terminal.
#[cfg(windows)]
pub fn configure_detached(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;

    command.creation_flags(windows::CREATE_NEW_PROCESS_GROUP | windows::CREATE_NO_WINDOW);
}

#[cfg(unix)]
pub fn configure_detached(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
}

#[cfg(windows)]
pub(super) fn configure_hidden(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;

    command.creation_flags(windows::CREATE_NO_WINDOW);
}

#[cfg(windows)]
mod windows {
    pub(super) const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub(super) const CREATE_NO_WINDOW: u32 = 0x0800_0000;
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn detached_child_owns_its_process_group() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "ps -o pgid= -p $$"])
            .stdout(std::process::Stdio::piped());
        super::configure_detached(&mut command);
        let child = command.spawn().expect("child");
        let pid = child.id();
        let output = child.wait_with_output().expect("child output");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout)
                .expect("UTF-8")
                .trim()
                .parse::<u32>()
                .expect("process group"),
            pid
        );
    }
}
