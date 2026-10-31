//! A Docker Engine for agents whose image is built from a Dockerfile. agentctl builds through the
//! local Docker Engine; an agent on a published image needs none. Setup only checks that one
//! answers and says what to install otherwise: installing one needs administrator rights and
//! differs between systems.

use std::process::Stdio;

use agent::Error;

/// What to install, for this system, when no Docker Engine answers. Docker Desktop is not named:
/// its license does not cover larger organisations.
const ADVICE: &str = if cfg!(target_os = "macos") {
    "Install Colima, a Docker Engine in a small Linux VM, with `brew install colima docker`, start it with \
     `colima start --cpu 4 --memory 8`, and run setup again."
} else if cfg!(windows) {
    "Install Docker Engine in WSL 2 and set DOCKER_HOST to it, then run setup again."
} else {
    "Install Docker Engine from your distribution (for example `sudo apt install docker.io`, or `sudo dnf install \
     moby-engine`), add yourself to the docker group, log in again, and run setup again."
};

/// Checks that a Docker Engine answers, which building the agent's image needs.
pub(super) async fn ensure() -> Result<(), Error> {
    let answers = tokio::process::Command::new("docker")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success());
    if answers {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "this agent's image is built from a Dockerfile, which needs a Docker Engine, and none answers. {ADVICE}"
        )))
    }
}
