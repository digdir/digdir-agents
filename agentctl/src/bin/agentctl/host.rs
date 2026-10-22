//! Programs on this host that agentctl runs, and whether they can run.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Whether Docker, which builds agents' images, can be used here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Docker {
    Running,
    /// Installed, but its engine does not answer.
    Stopped,
    Missing,
}

pub(crate) fn docker() -> Docker {
    let status = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Docker::Running,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Docker::Missing,
        Ok(_) | Err(_) => Docker::Stopped,
    }
}

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

/// How to install Git on this host, for a person who has never done it.
pub(crate) const fn git_install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Install it with `xcode-select --install`"
    } else {
        "Install it with your package manager"
    }
}

/// The first `program` on `PATH`, as the shell would run it.
fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}
