//! Starting `agentd` for a client, so that every client that ships beside it, such as `agentctl` or a GUI, reaches the
//! same daemon the same way.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use crate::{Error, control_api::Client, local::home::ControlPlaneHome, upgrade::InstallPaths};

/// How often a starting daemon is asked whether it is ready.
const READY_POLL: Duration = Duration::from_millis(100);
/// How many times a starting daemon is asked before giving up, which with [`READY_POLL`] is 10 seconds.
const READY_ATTEMPTS: u32 = 100;

/// Makes sure the daemon for `home` runs and comes from this build, starting `executable` when none answers.
///
/// A daemon from another build is refused rather than replaced, because it may be running agents. The caller decides
/// what to do then, such as `agentctl self update`.
///
/// # Errors
///
/// Returns an error when the running daemon is from another build, when `home`'s socket path is too long for one to
/// listen on, or when a started one exits or does not become ready within 10 seconds. The message points at the
/// daemon's log.
pub async fn ensure(home: &ControlPlaneHome, client: &Client, executable: &Path) -> Result<(), Error> {
    if let Ok(daemon) = client.health().await {
        return daemon.require_compatible();
    }
    let mut daemon = spawn(home, executable)?;
    for _ in 0..READY_ATTEMPTS {
        tokio::time::sleep(READY_POLL).await;
        if let Ok(daemon) = client.health().await {
            return daemon.require_compatible();
        }
        if let Some(status) = daemon.try_wait()? {
            return Err(startup_failure(
                home,
                &format!("automatic startup exited with {status}"),
            ));
        }
    }
    Err(startup_failure(
        home,
        "automatic startup did not become ready within 10 seconds",
    ))
}

/// An update stops agentd and keeps it stopped until it starts the new one, so a client that tries to start agentd
/// meanwhile fails. The update need not be for this client's home, so the usual diagnostics stay.
fn startup_failure(home: &ControlPlaneHome, failure: &str) -> Error {
    let diagnostics = startup_diagnostics(home);
    if InstallPaths::resolve()
        .and_then(|paths| paths.update_running())
        .unwrap_or(false)
    {
        return Error::Daemon(format!(
            "{failure}, likely because an agentctl update is running; rerun this command once it has finished, or {diagnostics}"
        ));
    }
    Error::Daemon(format!("{failure}; {diagnostics}"))
}

/// Where to look when the daemon did not start: its log, and a session relaunch an upgrade left pending.
#[must_use]
pub fn startup_diagnostics(home: &ControlPlaneHome) -> String {
    let log = home.daemon_log_path();
    let marker = home.pending_session_relaunch_path();
    if marker.exists() {
        format!(
            "see {}; pending post-upgrade session relaunch: {}",
            log.display(),
            marker.display()
        )
    } else {
        format!("see {}", log.display())
    }
}

/// The `agentd` that ships beside `client`, the executable of a client in the same release.
#[must_use]
pub fn beside(client: &Path) -> PathBuf {
    client.with_file_name(format!("agentd{}", std::env::consts::EXE_SUFFIX))
}

fn spawn(home: &ControlPlaneHome, executable: &Path) -> Result<Child, Error> {
    home.check_socket_path()?;
    home.prepare()?;
    let log = home.open_daemon_log()?;
    let mut command = Command::new(executable);
    command
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    super::process::configure_detached(&mut command);
    super::process::configure_logging(&mut command);
    command.spawn().map_err(Error::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    use super::{beside, ensure, startup_diagnostics};
    use crate::{control_api::Client, local::home::ControlPlaneHome};

    #[tokio::test(flavor = "local")]
    async fn a_home_too_long_for_its_socket_is_reported_without_starting_agentd() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let home = ControlPlaneHome::resolve(Some(&directory.path().join("a".repeat(120)))).expect("home");
        let client = Client::for_path(home.socket_path());

        let error = ensure(&home, &client, &directory.path().join("agentd"))
            .await
            .expect_err("no agentd can listen on a socket path this long");

        let message = error.to_string();
        assert!(
            message.contains(&home.socket_path().display().to_string()) && message.contains("too long"),
            "the error names the socket path and the problem: {message}"
        );
        assert!(
            !home.path().exists(),
            "neither the home, which holds agentd's log, was created nor agentd started"
        );
    }

    #[test]
    fn startup_diagnostics_identify_a_pending_session_relaunch() {
        let directory = tempfile::TempDir::new().expect("temporary home");
        let home = ControlPlaneHome::resolve(Some(directory.path())).expect("home");
        assert_eq!(
            startup_diagnostics(&home),
            format!("see {}", home.daemon_log_path().display())
        );

        std::fs::write(home.pending_session_relaunch_path(), "pending").expect("marker");
        let diagnostic = startup_diagnostics(&home);

        assert!(diagnostic.contains(&home.daemon_log_path().display().to_string()));
        assert!(diagnostic.contains(&home.pending_session_relaunch_path().display().to_string()));
    }

    #[test]
    fn the_daemon_is_the_one_beside_the_client() {
        let client = Path::new("/opt/agentctl/bin").join(format!("agent-gui{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(
            beside(&client),
            Path::new("/opt/agentctl/bin").join(format!("agentd{}", std::env::consts::EXE_SUFFIX))
        );
    }
}
