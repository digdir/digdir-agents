//! Programs on this host that agentctl runs, and whether they can run.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Whether Git can run without asking anything. On macOS, Git without the Command Line Tools is
/// Apple's stub, which opens an installer dialog instead of running, and Homebrew's Git needs
/// the Command Line Tools too.
pub(crate) fn git_available() -> bool {
    if on_path("git").is_none() {
        return false;
    }
    !cfg!(target_os = "macos")
        || Command::new("xcode-select")
            .arg("-p")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// The first `program` on `PATH`, as the shell would run it.
fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}
