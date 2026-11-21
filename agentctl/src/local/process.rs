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

/// Finds the first of `names` in `path`, a `PATH`-style variable, trying every
/// absolute directory for one name before the next. On Unix, as a shell does, a file only counts
/// when it may be run.
#[must_use]
pub fn find_executable(path: Option<&std::ffi::OsStr>, names: &[&str]) -> Option<std::path::PathBuf> {
    let directories = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.iter().find_map(|name| {
        directories
            .iter()
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(name))
            .find(|candidate| runnable(candidate))
    })
}

#[cfg(unix)]
fn runnable(candidate: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(candidate: &std::path::Path) -> bool {
    candidate.is_file()
}

/// Starts a daemon that runs on after the process starting it has exited.
///
/// The daemon gets only the standard input and output `command` names. Whatever reads this
/// process's output reads to its end, which a daemon holding that output open would delay until
/// the daemon exits.
///
/// # Errors
///
/// Returns the error from starting `command`.
pub fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    configure_detached(command);
    #[cfg(windows)]
    windows::keep_standard_handles();
    command.spawn()
}

/// Configures a child daemon to run independently of the invoking terminal.
#[cfg(windows)]
fn configure_detached(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;

    command.creation_flags(windows::CREATE_NEW_PROCESS_GROUP | windows::CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn configure_detached(command: &mut std::process::Command) {
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

    /// Keeps this process's standard handles from the children it starts.
    ///
    /// Windows gives a child every handle its parent marked inheritable, not only those it is
    /// given, and the standard handles a process gets from its caller are marked so. A child that
    /// [`std::process::Stdio::inherit`] gives them still gets them, since Rust passes it copies.
    #[expect(
        unsafe_code,
        reason = "Windows marks a handle inheritable only through SetHandleInformation, which has no safe wrapper"
    )]
    pub(super) fn keep_standard_handles() {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

        for handle in [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            // SAFETY: SetHandleInformation only changes the flags of `handle`, and fails on a null
            // or invalid one. A standard handle that cannot change has nothing for a child to keep
            // open, such as a console, so failure is ignored.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    /// Which part of [`read_to_end`] a run of the test binary plays.
    const ROLE: &str = "AGENT_TEST_DETACHED_ROLE";
    /// The file the detached child runs while it exists. The test removes it with its temporary
    /// directory, so the child stops when the test ends, also when the test fails.
    const RUNNING: &str = "AGENT_TEST_DETACHED_RUNNING";

    #[cfg(unix)]
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

    /// Scripts and coding agents read agentctl's output to its end, so the agentd it starts must
    /// not hold that output open. The test binary plays agentctl and agentd here, in the ignored
    /// tests below, so that the caller's pipes reach agentctl as they do from a shell.
    #[tokio::test]
    async fn detached_child_leaves_the_callers_output() {
        read_to_end("start", "start_detached").await;
    }

    /// A shell that captures agentctl's output, such as PowerShell running `agentctl get agents |
    /// Out-String`, gives agentctl a pipe of its own, and whatever runs the shell reads the shell's
    /// output to its end. The agentd that agentctl starts must not hold the shell's output open
    /// either, which agentctl inherits besides the standard handles it is given.
    #[tokio::test]
    async fn detached_child_leaves_the_output_of_the_shell_that_runs_the_caller() {
        read_to_end("shell", "capture_starter").await;
    }

    /// Runs the ignored test `name` of this binary as `role`, and reads its output to its end.
    async fn read_to_end(role: &str, name: &str) {
        let directory = tempfile::tempdir().expect("directory");
        let running = directory.path().join("running");
        std::fs::write(&running, "").expect("running file");
        let caller = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                &format!("local::process::tests::{name}"),
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(ROLE, role)
            .env(RUNNING, &running)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("caller");
        let output = tokio::time::timeout(Duration::from_secs(30), caller.wait_with_output())
            .await
            .expect("the detached child held the caller's output open")
            .expect("caller output");
        assert!(
            output.status.success(),
            "caller failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    #[test]
    #[ignore = "run by detached_child_leaves_the_output_of_the_shell_that_runs_the_caller"]
    fn capture_starter() {
        if std::env::var_os(ROLE).is_none_or(|role| role != "shell") {
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "local::process::tests::start_detached",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(ROLE, "start")
            .output()
            .expect("starter");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "starter failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "run by read_to_end"]
    #[expect(
        clippy::zombie_processes,
        reason = "the child outlives this process, as agentd outlives agentctl"
    )]
    fn start_detached() {
        if std::env::var_os(ROLE).is_none_or(|role| role != "start") {
            return;
        }
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["local::process::tests::run_detached", "--exact", "--ignored"])
            .env(ROLE, "run")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        super::spawn_detached(&mut command).expect("detached child");
    }

    #[test]
    #[ignore = "run by read_to_end"]
    fn run_detached() {
        if std::env::var_os(ROLE).is_none_or(|role| role != "run") {
            return;
        }
        let running = PathBuf::from(std::env::var_os(RUNNING).expect("running file"));
        for _ in 0..600 {
            if !running.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
