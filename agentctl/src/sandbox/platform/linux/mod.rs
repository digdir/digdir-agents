//! Idempotent agent setup for Linux sandboxes.

mod command;
mod home;
mod kvm;
mod podman;
mod preamble;
mod sources;
pub(crate) mod ssh;
mod systemd;
pub(crate) mod vnc;

use ::sandbox::{LocalFuture, Platform, SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::{Error, control_plane, harness};

use super::{
    super::PlatformAdapter,
    files::{read_existing, write_if_changed},
};
pub(crate) use command::run_checked;
use command::{checked_output, path_exists};
use home::{archive_home, sync_home};
use kvm::grant_kvm;
use podman::{configure_podman, point_podman_at_socket};
use sources::{read_instructions, read_skills};
use systemd::{SYSTEMCTL, SYSTEMD_RUNNING, systemd_available, wait_for_systemd};

/// The platform-owned sandbox user every session, Execution and SSH login runs as.
pub(crate) const USER: &str = "agent";
pub(crate) const HOME: &str = "/home/agent";
pub(crate) const WORKING_DIRECTORY: &str = "/home/agent/code";
/// Holds the Platform API base URL reachable from the sandbox, read by harness hooks on every
/// report. `agentd` binds a new host port on every start; agent setup rewrites the file first
/// thing on every pass, so harnesses that outlive a daemon restart follow it as soon as the
/// restarted daemon starts the sandbox's network.
///
/// Its presence is also how `agentctl` knows it runs inside an agent, in a session, an Execution or
/// an SSH login alike: [`crate::harness::inside_agent`] checks for this path, so moving the file
/// or writing it anywhere but in an agent's sandbox changes what harness logins with `agentctl`
/// do there.
pub(crate) const PLATFORM_API_URL_FILE: &str = "/etc/agent-platform-api-url";
/// Locale every sandbox process runs with; the image ships it, so UTF-8 output
/// renders regardless of the host's locale.
pub(crate) const UTF8_LOCALE: &str = "C.UTF-8";
/// Terminal type every sandbox terminal runs with. Host-specific TERM names are
/// not necessarily installed in the sandbox image; this baseline is.
pub(crate) const PORTABLE_TERMINAL: &str = "xterm-256color";

/// Agent setup for Linux sandboxes.
pub struct Linux {
    platform_api_url: String,
}

impl Linux {
    /// Creates the adapter that points harnesses at `platform_api_url`, the Platform API base
    /// URL reachable from the sandbox.
    #[must_use]
    pub fn new(platform_api_url: impl Into<String>) -> Self {
        Self {
            platform_api_url: platform_api_url.into(),
        }
    }
}

/// Points harness hooks in the sandbox at `platform_api_url`.
async fn install_platform_api_url(sandbox: &SandboxHandle, platform_api_url: &str) -> Result<(), Error> {
    write_if_changed(
        sandbox,
        PLATFORM_API_URL_FILE,
        format!("{platform_api_url}\n").as_bytes(),
    )
    .await?;
    Ok(())
}

pub(super) fn execution_spec(command: &[String], terminal: bool) -> Result<ExecutionSpec, Error> {
    let (executable, arguments) = command
        .split_first()
        .ok_or_else(|| Error::Invalid("command is required".into()))?;
    let mut environment = vec![("HOME".into(), HOME.into()), ("LANG".into(), UTF8_LOCALE.into())];
    if terminal {
        // Host-specific TERM names are not necessarily installed in the guest.
        environment.push(("TERM".into(), PORTABLE_TERMINAL.into()));
    }
    Ok(
        ExecutionSpec::command(SandboxPath::new(executable), arguments.iter().cloned())
            .with_working_directory(SandboxPath::new(WORKING_DIRECTORY))
            .with_environment(environment),
    )
}

impl PlatformAdapter for Linux {
    fn supports(&self, platform: &Platform) -> bool {
        platform.os == "linux"
    }

    fn setup<'a>(
        &'a self,
        record: &'a control_plane::AgentRecord,
        sandbox: &'a SandboxHandle,
        harnesses: &'a [crate::Harness],
        steps: &'a ::sandbox::SandboxProgress,
    ) -> LocalFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(self.setup(record, sandbox, harnesses, steps))
    }
}

impl Linux {
    /// Sets up only the harnesses preparation reported installing, so setup and preparation
    /// cannot disagree about an optional installation whose host login was absent. Returns the
    /// optional instructions and skills it left out because they do not exist.
    async fn setup(
        &self,
        record: &control_plane::AgentRecord,
        sandbox: &SandboxHandle,
        harnesses: &[crate::Harness],
        steps: &::sandbox::SandboxProgress,
    ) -> Result<Vec<String>, Error> {
        let installations: Vec<&crate::HarnessSpec> = record
            .agent
            .spec
            .harnesses
            .iter()
            .filter(|installation| harnesses.contains(&installation.kind))
            .collect();
        // Before anything that can fail, so a pass that fails later still moves running harnesses.
        // Harnesses still running an earlier release's hook script follow only once a pass reaches
        // harness bootstrap, which replaces that script.
        install_platform_api_url(sandbox, &self.platform_api_url).await?;
        for installation in &installations {
            let step = steps.start_step(format!("Verify {}", installation.kind.as_str())).await;
            harness::verify_linux(installation.kind, sandbox, installation.version.as_deref()).await?;
            step.complete().await;
        }
        let step = steps.start_step("Prepare workspace and Podman").await;
        run_checked(sandbox, "/usr/bin/install", ["-d", "-m", "0755", WORKING_DIRECTORY]).await?;
        let podman = configure_podman(sandbox).await?;
        let kvm = grant_kvm(sandbox).await?;
        step.complete().await;
        let step = steps.start_step("Sync home").await;
        let archive = archive_home(record.source_directory.clone(), record.agent.spec.home.source.clone()).await?;
        sync_home(sandbox, archive).await?;
        if podman {
            // After the home sync, so a file of the same name in the agent's home cannot replace it.
            point_podman_at_socket(sandbox).await?;
        }
        configure_git_identity(sandbox).await?;
        step.complete().await;
        let observed = preamble::Observed {
            harnesses,
            environment: &sandbox.snapshot().environment,
            kvm,
            podman,
        };
        let section = preamble::render(record, &observed);
        let mut missing = Vec::new();
        let own = read_instructions(record, &mut missing).await?;
        let mut skills = read_skills(record, &mut missing).await?;
        let platform_skill = harness::platform_skill();
        // An agent's own skill of the same name replaces the platform's.
        if !skills.iter().any(|skill| skill.name == platform_skill.name) {
            skills.push(platform_skill);
        }
        for installation in &installations {
            let step = steps
                .start_step(format!(
                    "Install {} instructions and Skills",
                    installation.kind.as_str()
                ))
                .await;
            let instructions = if let Some(own) = &own {
                preamble::place(&section, own)
            } else {
                // Without instructions of its own, the agent's image or home overlay may own the file: the section
                // goes above what it holds. The home sync restores an overlay's file on every pass, so for a
                // moment before this rewrite it lacks the section.
                let path = harness::instructions_path(installation.kind, HOME);
                let existing = read_owned_instructions(sandbox, &path).await?;
                preamble::place(&section, preamble::without_section(&existing))
            };
            harness::bootstrap_linux(installation.kind, sandbox, HOME, &instructions, &skills).await?;
            step.complete().await;
        }
        Ok(missing)
    }
}

/// The most of an instruction file the image or home overlay owns that setup reads back.
const MAX_OWNED_INSTRUCTIONS: usize = 1024 * 1024;

/// What the image or home overlay put in a harness's instruction file: nothing when it does not
/// exist. The sandbox user can replace the file, so it must be a regular file and is read only up
/// to a bound; anything else fails setup rather than losing the file's text.
async fn read_owned_instructions(sandbox: &SandboxHandle, path: &str) -> Result<Vec<u8>, Error> {
    if !path_exists(sandbox, "-e", path).await? {
        return Ok(Vec::new());
    }
    if !path_exists(sandbox, "-f", path).await? {
        return Err(Error::SandboxSetup(format!(
            "{path} is not a regular file, so the agent sandbox section cannot be placed above it"
        )));
    }
    read_existing(sandbox, path, MAX_OWNED_INSTRUCTIONS).await.ok_or_else(|| {
        Error::SandboxSetup(format!(
            "{path} is unreadable or larger than {MAX_OWNED_INSTRUCTIONS} bytes, so the agent sandbox section cannot be \
             placed above it"
        ))
    })
}

async fn configure_git_identity(sandbox: &SandboxHandle) -> Result<(), Error> {
    let environment = &sandbox.snapshot().environment;
    let (Some(name), Some(email)) = (environment.get("GIT_USER_NAME"), environment.get("GIT_USER_EMAIL")) else {
        if environment.contains_key("GIT_USER_NAME") || environment.contains_key("GIT_USER_EMAIL") {
            return Err(Error::Invalid(
                "GIT_USER_NAME and GIT_USER_EMAIL must both be configured".into(),
            ));
        }
        return Ok(());
    };
    let present = sandbox
        .run_execution(ExecutionSpec::command(
            SandboxPath::new("/usr/bin/env"),
            ["git".into(), "--version".into()],
        ))
        .await?;
    match present.status.code {
        127 => return Ok(()),
        0 => {}
        code => {
            return Err(Error::SandboxSetup(format!(
                "Git presence check exited with code {code}"
            )));
        }
    }
    run_git_config(sandbox, "user.name", name).await?;
    run_git_config(sandbox, "user.email", email).await
}

async fn run_git_config(sandbox: &SandboxHandle, key: &str, value: &str) -> Result<(), Error> {
    let args = ["git", "config", "--global", key, value];
    let output = sandbox
        .run_execution(
            ExecutionSpec::command(SandboxPath::new("/usr/bin/env"), args.into_iter().map(str::to_owned))
                .with_environment([("HOME".into(), HOME.into())]),
        )
        .await?;
    checked_output("/usr/bin/env", &args, &output)
}
