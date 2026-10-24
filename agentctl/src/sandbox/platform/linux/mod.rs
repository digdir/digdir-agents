//! Idempotent Agent setup for Linux Sandboxes.

mod command;
mod home;
mod kvm;
mod podman;
mod sources;
pub(crate) mod ssh;
mod systemd;
pub(crate) mod vnc;

use ::sandbox::{LocalFuture, Platform, SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::{Error, control_plane, harness};

use super::{super::PlatformAdapter, files::write_if_changed};
pub(crate) use command::run_checked;
use command::{checked_output, path_exists};
use home::{archive_home, sync_home};
use kvm::grant_kvm;
use podman::{configure_podman, point_podman_at_socket};
use sources::{read_instructions, read_skills};
use systemd::{SYSTEMCTL, SYSTEMD_RUNNING, systemd_available, wait_for_systemd};

/// The platform-owned Sandbox user every Session, Execution and SSH login runs as.
pub(crate) const USER: &str = "agent";
pub(crate) const HOME: &str = "/home/agent";
pub(crate) const WORKING_DIRECTORY: &str = "/home/agent/code";
/// Holds the Platform API base URL reachable from the Sandbox, read by harness hooks on every
/// report. `agentd` binds a new host port on every start; Agent setup rewrites the file first
/// thing on every pass, so harnesses that outlive a daemon restart follow it as soon as the
/// restarted daemon starts the Sandbox's network.
pub(crate) const PLATFORM_API_URL_FILE: &str = "/etc/agent-platform-api-url";
/// Locale every Sandbox process runs with; the image ships it, so UTF-8 output
/// renders regardless of the host's locale.
pub(crate) const UTF8_LOCALE: &str = "C.UTF-8";
/// Terminal type every Sandbox terminal runs with. Host-specific TERM names are
/// not necessarily installed in the Sandbox image; this baseline is.
pub(crate) const PORTABLE_TERMINAL: &str = "xterm-256color";

/// Agent setup for Linux Sandboxes.
pub struct Linux {
    platform_api_url: String,
    host_instructions: Option<std::path::PathBuf>,
}

impl Linux {
    /// Creates the adapter that points harnesses at `platform_api_url`, the Platform API base
    /// URL reachable from the Sandbox.
    #[must_use]
    pub fn new(platform_api_url: impl Into<String>) -> Self {
        Self {
            platform_api_url: platform_api_url.into(),
            host_instructions: None,
        }
    }

    /// Gives every agent the instructions in `path`, re-read on every pass, after the platform's
    /// and ahead of its own. A missing or empty file adds nothing.
    #[must_use]
    pub fn with_host_instructions(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.host_instructions = Some(path.into());
        self
    }
}

/// Points harness hooks in the Sandbox at `platform_api_url`.
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
        github_account: Option<&'a crate::github::GithubAccount>,
        steps: &'a ::sandbox::SandboxProgress,
    ) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(self.setup(record, sandbox, harnesses, github_account, steps))
    }
}

impl Linux {
    /// Sets up only the harnesses preparation reported installing, so setup and preparation
    /// cannot disagree about an optional installation whose host login was absent.
    async fn setup(
        &self,
        record: &control_plane::AgentRecord,
        sandbox: &SandboxHandle,
        harnesses: &[crate::Harness],
        github_account: Option<&crate::github::GithubAccount>,
        steps: &::sandbox::SandboxProgress,
    ) -> Result<(), Error> {
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
        grant_kvm(sandbox).await?;
        step.complete().await;
        let step = steps.start_step("Sync home").await;
        if let Some(home) = &record.agent.spec.home {
            let archive = archive_home(record.source_directory.clone(), home.source.clone()).await?;
            sync_home(sandbox, archive).await?;
        }
        if podman {
            // After the home sync, so a file of the same name in the Agent's home cannot replace it.
            point_podman_at_socket(sandbox).await?;
        }
        configure_git(sandbox, record.agent.spec.github_owner().is_some(), github_account).await?;
        step.complete().await;
        let about = super::about::instructions(&record.agent.spec);
        let instructions = read_instructions(record, &about, self.host_instructions.as_deref()).await?;
        let skills = read_skills(record).await?;
        for installation in &installations {
            let step = steps
                .start_step(format!(
                    "Install {} instructions and Skills",
                    installation.kind.as_str()
                ))
                .await;
            harness::bootstrap_linux(installation.kind, sandbox, HOME, &instructions, &skills).await?;
            step.complete().await;
        }
        Ok(())
    }
}

/// Configures Git for the Agent's user when the image has it: the commit identity from
/// `GIT_USER_NAME` and `GIT_USER_EMAIL`, or else from the GitHub `account`, and for an Agent with
/// a GitHub account, HTTPS access to GitHub through the mediated `GITHUB_TOKEN`.
async fn configure_git(
    sandbox: &SandboxHandle,
    github_account: bool,
    account: Option<&crate::github::GithubAccount>,
) -> Result<(), Error> {
    let environment = &sandbox.snapshot().environment;
    let identity = match (environment.get("GIT_USER_NAME"), environment.get("GIT_USER_EMAIL")) {
        (Some(name), Some(email)) => Some((name.clone(), email.clone())),
        (None, None) => account.map(|account| (account.name.clone(), account.email.clone())),
        _ => {
            return Err(Error::Invalid(
                "GIT_USER_NAME and GIT_USER_EMAIL must both be configured".into(),
            ));
        }
    };
    if identity.is_none() && !github_account {
        return Ok(());
    }
    let present = run_git(sandbox, &["--version"]).await?;
    match present.status.code {
        127 => return Ok(()),
        0 => {}
        code => {
            return Err(Error::SandboxSetup(format!(
                "Git presence check exited with code {code}"
            )));
        }
    }
    if let Some((name, email)) = identity {
        run_git_config(sandbox, "user.name", &name).await?;
        run_git_config(sandbox, "user.email", &email).await?;
    }
    if github_account {
        configure_github_https(sandbox).await?;
    }
    Ok(())
}

/// Hands Git the placeholder as the password; the mediator replaces it in the Basic credentials.
const GITHUB_CREDENTIAL_KEY: &str = "credential.https://github.com.helper";
const GITHUB_CREDENTIAL_HELPER: &str =
    "!f() { test \"$1\" = get || exit 0; echo username=x-access-token; echo \"password=$GITHUB_TOKEN\"; }; f";
const GITHUB_HTTPS_REWRITE: &str = "url.https://github.com/.insteadOf";
const GITHUB_SSH_URLS: [&str; 2] = ["git@github.com:", "ssh://git@github.com/"];

/// Gives Git the mediated `GITHUB_TOKEN` for GitHub over HTTPS, and sends GitHub's SSH URLs over
/// HTTPS, since the Sandbox has no SSH key and mediation covers HTTPS only.
///
/// The helper is added after any the image configured, which Git asks first, so an image's own
/// helper still wins when it answers and ours answers when it does not, such as a generic `cache`
/// or `store`. An image's own rewrite of an SSH URL is kept.
async fn configure_github_https(sandbox: &SandboxHandle) -> Result<(), Error> {
    let helpers = git_lookup(sandbox, &["config", "--global", "--get-all", GITHUB_CREDENTIAL_KEY])
        .await?
        .unwrap_or_default();
    if !helpers.lines().any(|helper| helper == GITHUB_CREDENTIAL_HELPER) {
        add_git_config(sandbox, GITHUB_CREDENTIAL_KEY, GITHUB_CREDENTIAL_HELPER).await?;
    }
    let rewrites = git_lookup(sandbox, &["config", "--get-regexp", r"^url\..*\.insteadof$"])
        .await?
        .unwrap_or_default();
    for ssh in GITHUB_SSH_URLS {
        if !rewrites.lines().any(|line| line.split_whitespace().nth(1) == Some(ssh)) {
            add_git_config(sandbox, GITHUB_HTTPS_REWRITE, ssh).await?;
        }
    }
    Ok(())
}

/// Adds `value` to the user's multi-valued Git setting `key`.
async fn add_git_config(sandbox: &SandboxHandle, key: &str, value: &str) -> Result<(), Error> {
    let args = ["git", "config", "--global", "--add", key, value];
    let output = run_git(sandbox, &args[1..]).await?;
    checked_output("/usr/bin/env", &args, &output)
}

/// Runs a `git config` lookup: its output when set, `None` when Git reports it unset.
async fn git_lookup(sandbox: &SandboxHandle, args: &[&str]) -> Result<Option<String>, Error> {
    let output = run_git(sandbox, args).await?;
    match output.status.code {
        0 => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        }
        1 => Ok(None),
        _ => {
            let mut full = vec!["git"];
            full.extend_from_slice(args);
            checked_output("/usr/bin/env", &full, &output).map(|()| None)
        }
    }
}

async fn run_git(sandbox: &SandboxHandle, args: &[&str]) -> Result<::sandbox::execution::ExecutionOutput, Error> {
    let args = std::iter::once("git").chain(args.iter().copied()).map(str::to_owned);
    Ok(sandbox
        .run_execution(
            ExecutionSpec::command(SandboxPath::new("/usr/bin/env"), args)
                .with_environment([("HOME".into(), HOME.into())]),
        )
        .await?)
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
