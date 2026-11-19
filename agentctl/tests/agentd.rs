//! The agentd binary as a process.
#![cfg(unix)]
#![allow(clippy::expect_used)]

use crate::support::TempDirectory;

use std::{
    os::unix::fs::PermissionsExt as _,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Kills agentd when the test ends, however it ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The guest's users are not the host user, so what agentd and its runtimes share with a sandbox must be readable by
/// others. A strict umask, such as the one `install.sh` runs `agentctl self update` under, must not take that away.
#[test]
fn agentd_shares_readable_folders_under_a_strict_umask() {
    let temporary = TempDirectory::new("agentd-umask");
    let home = temporary.path().join("home");
    let daemon = Daemon(
        Command::new("/bin/sh")
            .args(["-c", r#"umask 077 && exec "$0" --home "$1""#])
            .arg(env!("CARGO_BIN_EXE_agentd"))
            .arg(&home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("agentd should start"),
    );

    let microsandbox = home.join("microsandbox");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !microsandbox.is_dir() {
        assert!(
            Instant::now() < deadline,
            "agentd should create {}",
            microsandbox.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(daemon);

    let mode = std::fs::metadata(&microsandbox)
        .expect("the Microsandbox folder should be readable")
        .permissions()
        .mode()
        & 0o777;
    assert!(
        mode & 0o055 == 0o055,
        "agentd under umask 077 created {} as {mode:o}, which others cannot read and search",
        microsandbox.display()
    );
}
