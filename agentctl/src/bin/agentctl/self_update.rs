use std::{
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::Duration,
};

use agent::{
    Error,
    control_api::{Client, DaemonInfo, PROTOCOL_VERSION},
    local::home::{ControlPlaneHome, Lock},
    upgrade::{self, InstallMetadata, InstallPaths, Release, UpdateJournal, UpdatePhase},
};

use super::CommandResult;

const DEFAULT_REPOSITORY: &str = "digdir/digdir-agents";
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(65);
const LIFECYCLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(65);
const TARGET_VERIFY_TIMEOUT: Duration = Duration::from_secs(75);
const TARGET_RESTART_INTERVAL: Duration = Duration::from_secs(2);

struct Completion {
    paths: InstallPaths,
    target_release: PathBuf,
    target_version: String,
    previous_release: Option<PathBuf>,
    repository: String,
}

impl Completion {
    fn from_journal(paths: InstallPaths, journal: UpdateJournal) -> Result<Self, Error> {
        let repository = repository(&paths)?;
        Ok(Self {
            paths,
            target_release: journal.target_release,
            target_version: journal.target_version,
            previous_release: journal.previous_release,
            repository,
        })
    }

    fn run_target(&self, home: &ControlPlaneHome) -> Result<(), Error> {
        let mut command = ProcessCommand::new(
            self.target_release
                .join(format!("agentctl{}", std::env::consts::EXE_SUFFIX)),
        );
        command
            .arg("--home")
            .arg(home.path())
            .args(["self", "__complete-update", "--install-root"])
            .arg(self.paths.root())
            .arg("--bin-directory")
            .arg(self.paths.bin())
            .arg("--target-release")
            .arg(&self.target_release)
            .arg("--target-version")
            .arg(&self.target_version)
            .arg("--repository")
            .arg(&self.repository);
        if let Some(previous) = &self.previous_release {
            command.arg("--previous-release").arg(previous);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(Error::Daemon(format!("target updater exited with {status}")));
        }
        Ok(())
    }
}

#[derive(clap::Subcommand)]
pub(super) enum SelfCommand {
    /// Check for or install a released version of agentctl and agentd.
    ///
    /// The agent definitions in your checkouts stay as they are. `agentctl update` brings them up
    /// to date with the version installed.
    Update {
        /// Install this exact release version.
        #[arg(long)]
        version: Option<String>,
        /// Report whether an update is available without downloading it.
        #[arg(long)]
        check: bool,
    },
    /// Complete a target-owned package and state transition.
    #[command(name = "__complete-update", hide = true)]
    CompleteUpdate {
        #[arg(long)]
        install_root: PathBuf,
        #[arg(long)]
        bin_directory: PathBuf,
        #[arg(long)]
        target_release: PathBuf,
        #[arg(long)]
        target_version: String,
        #[arg(long)]
        previous_release: Option<PathBuf>,
        #[arg(long, default_value = DEFAULT_REPOSITORY)]
        repository: String,
    },
    /// Publish an extracted release through the platform install lock.
    #[command(name = "__publish-release", hide = true)]
    PublishRelease {
        #[arg(long)]
        install_root: PathBuf,
        #[arg(long)]
        bin_directory: PathBuf,
        #[arg(long)]
        source_release: PathBuf,
        #[arg(long)]
        target_version: String,
    },
}

pub(super) async fn execute(command: SelfCommand, home: &ControlPlaneHome) -> CommandResult<()> {
    match command {
        SelfCommand::Update { version, check } => update(home, version.as_deref(), check).await,
        SelfCommand::CompleteUpdate {
            install_root,
            bin_directory,
            target_release,
            target_version,
            previous_release,
            repository,
        } => {
            complete(
                Completion {
                    paths: InstallPaths::new(install_root, bin_directory)?,
                    target_release,
                    target_version,
                    previous_release,
                    repository,
                },
                home,
            )
            .await
        }
        SelfCommand::PublishRelease {
            install_root,
            bin_directory,
            source_release,
            target_version,
        } => {
            validate_source_process(&source_release)?;
            upgrade::publish_release(
                &InstallPaths::new(install_root, bin_directory)?,
                &source_release,
                &target_version,
            )
            .await?;
            Ok(())
        }
    }
}

pub(super) fn resume_pending_before_command(home: &ControlPlaneHome) -> CommandResult<()> {
    let paths = InstallPaths::resolve()?;
    let Some(journal) = UpdateJournal::read_unfinished(&paths)? else {
        return Ok(());
    };
    let target_version = journal.target_version.clone();
    Completion::from_journal(paths, journal)?.run_target(home)?;
    if !same_version(agent::build_version(), &target_version)? {
        return Err(Error::Daemon(format!(
            "Agent update to {target_version} completed; rerun this command with the current agentctl"
        ))
        .into());
    }
    Ok(())
}

async fn update(home: &ControlPlaneHome, version: Option<&str>, check: bool) -> CommandResult<()> {
    let paths = InstallPaths::resolve()?;
    if let Some(journal) = UpdateJournal::read_unfinished(&paths)? {
        Completion::from_journal(paths, journal)?.run_target(home)?;
        return Ok(());
    }
    if version.is_none() && agent::release_version().is_none() {
        return Err(Error::Invalid("this development build needs an explicit self update --version".into()).into());
    }
    let repository = repository(&paths)?;
    println!("Resolve release");
    let release = Release::resolve(version, repository).await?;
    let previous = upgrade::current_release(&paths)?;
    let current_version = previous
        .as_deref()
        .map(upgrade::managed_release_version)
        .transpose()?
        .unwrap_or_else(|| agent::build_version().to_owned());
    compare_versions(&current_version, &release.version)?;
    if previous.is_some() && same_version(&current_version, &release.version)? {
        println!("Agent {} is already installed", release.version);
        return Ok(());
    }
    if check {
        println!("Agent {} is available (current {})", release.version, current_version);
        return Ok(());
    }

    println!("Download and verify package");
    let staged = upgrade::stage_release(&paths, release).await?;
    Completion {
        paths,
        target_release: staged.path,
        target_version: staged.release.version,
        previous_release: previous,
        repository: staged.release.repository,
    }
    .run_target(home)
    .map_err(Into::into)
}

async fn complete(completion: Completion, home: &ControlPlaneHome) -> CommandResult<()> {
    // Release ordering belongs to `self update`. Installers enter here directly
    // so a local development build may replace a higher published preview.
    let Completion {
        paths,
        target_release,
        target_version,
        previous_release,
        repository,
    } = completion;
    let _install_lock = paths.lock().await?;
    validate_target_process(&paths, &target_release, &target_version)?;
    let (mut journal, journal_is_new) = if let Some(journal) = UpdateJournal::read_unfinished(&paths)? {
        if journal.target_release != target_release
            || journal.target_version != target_version
            || journal.previous_release != previous_release
        {
            return Err(Error::Invalid("arguments do not match the unfinished agentctl update".into()).into());
        }
        (journal, false)
    } else {
        (
            UpdateJournal::new(previous_release.clone(), target_release.clone(), target_version.clone()),
            true,
        )
    };
    let client = Client::for_path(home.socket_path());
    if journal.phase < UpdatePhase::Migrated {
        println!("Check agent activity");
        match tokio::time::timeout(Duration::from_secs(2), client.health()).await {
            Ok(Ok(info)) => {
                if is_preview_1(&info) {
                    return Err(Error::Daemon(preview_stop_instruction().into()).into());
                }
                println!("Stop agentd");
                let warnings = tokio::time::timeout(LIFECYCLE_REQUEST_TIMEOUT, client.shutdown_for_upgrade())
                    .await
                    .map_err(|_| Error::Daemon("timed out waiting for agentd to prepare for upgrade".into()))??;
                for warning in warnings {
                    eprintln!("Warning: {warning}");
                }
            }
            Err(_) => {
                return Err(Error::Daemon(
                    "agentd did not answer its health check; retry the update or stop agentd".into(),
                )
                .into());
            }
            Ok(Err(_)) => {}
        }
    }
    let home_lock = if journal.phase < UpdatePhase::Activated {
        Some(acquire_home_lock(home, &client).await?)
    } else {
        None
    };
    if journal_is_new {
        journal.advance(&paths, UpdatePhase::Prepared)?;
    }
    if journal.phase < UpdatePhase::Migrated {
        println!("Migrate agent state");
        if let Err(error) = migrate_state(&paths, &journal, &home.path().join("agent.db")) {
            drop(home_lock);
            if let Some(previous_release) = &previous_release {
                let _ = start_daemon(previous_release, home);
            }
            return Err(error.into());
        }
        upgrade::create_session_relaunch_marker(home, &target_version)?;
        journal.advance(&paths, UpdatePhase::Migrated)?;
    }

    if journal.phase < UpdatePhase::Activated {
        println!("Activate target package");
        InstallMetadata::new(repository, paths.bin().to_path_buf()).write(&paths)?;
        upgrade::activate_release(&paths, &target_release)?;
        journal.advance(&paths, UpdatePhase::Activated)?;
    }
    drop(home_lock);

    println!("Start and verify target daemon");
    let start_target = || start_daemon(&target_release, home);
    if !target_ready(&client, home, &target_version).await {
        start_target()?;
    }
    verify_target(&client, home, &target_version, start_target).await?;
    journal.advance(&paths, UpdatePhase::Complete)?;
    upgrade::prune_releases(&paths, previous_release.as_deref())?;
    println!("Agent updated to {target_version}");
    Ok(())
}

fn migrate_state(paths: &InstallPaths, journal: &UpdateJournal, database: &Path) -> Result<(), Error> {
    match agent::persistence::Database::migrate(database) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(discard_error) = journal.discard_before_migration(paths) {
                return Err(Error::Database(format!(
                    "{error}; failed to discard the pre-migration update journal: {discard_error}"
                )));
            }
            Err(error)
        }
    }
}

fn validate_target_process(paths: &InstallPaths, target: &Path, version: &str) -> Result<(), Error> {
    let target = canonical_target(paths, target)?;
    upgrade::validate_release_directory(&target, version)?;
    validate_release_process(&target, "update completion must run from the target release")
}

fn canonical_target(paths: &InstallPaths, target: &Path) -> Result<PathBuf, Error> {
    if !target.is_absolute() {
        return Err(Error::Invalid("target updater paths are inconsistent".into()));
    }
    let releases = std::fs::canonicalize(paths.releases())?;
    let target = std::fs::canonicalize(target)?;
    if target.parent() != Some(releases.as_path()) {
        return Err(Error::Invalid("target updater paths are inconsistent".into()));
    }
    Ok(target)
}

fn validate_source_process(source: &Path) -> Result<(), Error> {
    validate_release_process(source, "release publication must run from the source package")
}

fn validate_release_process(release: &Path, mismatch: &str) -> Result<(), Error> {
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let expected = std::fs::canonicalize(release.join(format!("agentctl{}", std::env::consts::EXE_SUFFIX)))?;
    if executable != expected {
        return Err(Error::Invalid(mismatch.into()));
    }
    Ok(())
}

/// Takes the control-plane home once agentd has let go of it.
///
/// Any agentctl command that runs meanwhile finds no agentd and starts one from its own release,
/// which is the release being replaced. Queuing for the lock, rather than polling it, takes the
/// home the moment agentd exits, so an agentd started after that gives up. One that got the home
/// first is asked to stop as well.
async fn acquire_home_lock(home: &ControlPlaneHome, client: &Client) -> Result<Lock, Error> {
    let (sender, mut lock) = tokio::sync::oneshot::channel();
    let waiting = home.clone();
    #[allow(
        clippy::disallowed_methods,
        reason = "a lock wait cannot be stopped, and the runtime would wait for a blocking thread on exit if the lock never came"
    )]
    std::thread::spawn(move || {
        let _ = sender.send(waiting.wait_for_lock());
    });
    let deadline = tokio::time::sleep(DAEMON_STOP_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            result = &mut lock => {
                return result.map_err(|_| Error::Daemon("waiting for the control-plane home stopped".into()))?;
            }
            () = &mut deadline => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "control-plane home is already locked",
                )));
            }
            () = stop_restarted_daemon(client) => {}
        }
    }
}

async fn stop_restarted_daemon(client: &Client) {
    tokio::time::sleep(Duration::from_millis(500)).await;
    if !matches!(
        tokio::time::timeout(Duration::from_secs(1), client.health()).await,
        Ok(Ok(_))
    ) {
        return;
    }
    // The agentd stopped first is still finishing, or refuses, so this changes nothing for it.
    if let Ok(Ok(_)) = tokio::time::timeout(LIFECYCLE_REQUEST_TIMEOUT, client.shutdown_for_upgrade()).await {
        println!("Stop agentd started during the update");
    }
}

fn start_daemon(release: &Path, home: &ControlPlaneHome) -> Result<(), Error> {
    let log = home.open_daemon_log()?;
    let mut command = ProcessCommand::new(release.join(format!("agentd{}", std::env::consts::EXE_SUFFIX)));
    command
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    agent::local::process::configure_detached(&mut command);
    agent::local::process::configure_logging(&mut command);
    command.spawn()?;
    Ok(())
}

/// Waits for the target agentd to start and relaunch sessions.
///
/// An agentd that a command started from the replaced release can take the home before the
/// target does, so that the target gives up. That agentd exits once it reads the session relaunch
/// marker, which names the target, so the target is started again whenever nothing holds the home.
async fn verify_target(
    client: &Client,
    home: &ControlPlaneHome,
    target_version: &str,
    start_target: impl Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    let deadline = tokio::time::Instant::now() + TARGET_VERIFY_TIMEOUT;
    let mut next_start = tokio::time::Instant::now() + TARGET_RESTART_INTERVAL;
    while tokio::time::Instant::now() < deadline {
        if target_ready(client, home, target_version).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= next_start {
            next_start = tokio::time::Instant::now() + TARGET_RESTART_INTERVAL;
            let home_free = home.acquire_lock().is_ok();
            if home_free {
                start_target()?;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error::Daemon(format!(
        "target agentd {target_version:?} did not become healthy and finish session relaunch within {} seconds; {}",
        TARGET_VERIFY_TIMEOUT.as_secs(),
        super::daemon_startup_diagnostics(home)
    )))
}

async fn target_ready(client: &Client, home: &ControlPlaneHome, target_version: &str) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(1), client.health()).await,
        Ok(Ok(info))
            if info.protocol_version.as_deref() == Some(PROTOCOL_VERSION)
                && info.build_version.as_deref() == Some(target_version)
                && !home.pending_session_relaunch_path().exists()
    )
}

fn compare_versions(current: &str, target: &str) -> Result<(), Error> {
    let current = parse_version(current);
    let target = parse_version(target).map_err(|error| Error::Invalid(format!("invalid target version: {error}")))?;
    if let Ok(ref current) = current
        && target < *current
    {
        return Err(Error::Invalid(format!(
            "downgrading agentctl from v{current} to v{target} is not supported"
        )));
    }
    Ok(())
}

fn same_version(current: &str, target: &str) -> Result<bool, Error> {
    let current =
        parse_version(current).map_err(|error| Error::Invalid(format!("invalid current version: {error}")))?;
    let target = parse_version(target).map_err(|error| Error::Invalid(format!("invalid target version: {error}")))?;
    Ok(current == target)
}

fn parse_version(version: &str) -> Result<semver::Version, semver::Error> {
    semver::Version::parse(version.strip_prefix('v').unwrap_or(version))
}

fn repository(paths: &InstallPaths) -> Result<String, Error> {
    match InstallMetadata::read(paths) {
        Ok(metadata) => Ok(metadata.repository().to_owned()),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::env::var("AGENT_GITHUB_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.into()))
        }
        Err(error) => Err(error),
    }
}

const fn preview_stop_instruction() -> &'static str {
    if cfg!(windows) {
        "preview 1 agentd cannot stop itself; finish active turns, run `Stop-Process -Name agentd` in PowerShell, and rerun the installer"
    } else {
        "preview 1 agentd cannot stop itself; finish active turns, run `pkill -x agentd`, and rerun the installer"
    }
}

fn is_preview_1(info: &DaemonInfo) -> bool {
    info.protocol_version.as_deref() == Some("v1") && info.build_version.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_rejects_downgrade() {
        assert!(compare_versions("v2.0.0", "v1.0.0").is_err());
        assert!(
            compare_versions("v0.1.0-preview.2", "v0.0.1-dev.20260914000000").is_err(),
            "the released-update policy also rejects a lower development build"
        );
        assert!(same_version("1.0.0", "v1.0.0").expect("version"));
    }

    #[test]
    fn failed_migration_discards_the_prepared_journal() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let paths =
            InstallPaths::new(temporary.path().join("install"), temporary.path().join("bin")).expect("install paths");
        let target = paths.releases().join(format!("v2.0.0-{}", upgrade::package_target()));
        let mut journal = UpdateJournal::new(None, target, "v2.0.0".into());
        journal
            .advance(&paths, UpdatePhase::Prepared)
            .expect("prepared journal");
        let database = temporary.path().join("agent.db");
        rusqlite::Connection::open(&database)
            .expect("database")
            .execute_batch("PRAGMA user_version = 999;")
            .expect("future schema");

        let error = migrate_state(&paths, &journal, &database).expect_err("migration failure");

        assert!(error.to_string().contains("newer than the supported schema"), "{error}");
        assert!(
            !paths.journal().exists(),
            "failed migration must not poison later commands"
        );
    }

    #[test]
    fn only_preview_1_requires_manual_daemon_shutdown() {
        assert!(is_preview_1(&DaemonInfo {
            protocol_version: Some("v1".into()),
            build_version: None,
        }));
        assert!(!is_preview_1(&DaemonInfo {
            protocol_version: Some("v1".into()),
            build_version: Some("v0.2.0".into()),
        }));
        assert!(!is_preview_1(&DaemonInfo {
            protocol_version: Some("v2".into()),
            build_version: Some("v0.3.0".into()),
        }));
    }

    /// An agentd that holds the control-plane home until it is asked to stop.
    struct Daemon {
        home_lock: std::rc::Rc<std::cell::RefCell<Option<Lock>>>,
        shutdowns: std::rc::Rc<std::cell::Cell<usize>>,
        version: &'static str,
    }

    impl agent::control_api::Connector for Daemon {
        fn connect(&self) -> sandbox::LocalFuture<'_, Result<Box<dyn agent::control_api::Connection>, Error>> {
            Box::pin(async move {
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                if self.home_lock.borrow().is_none() {
                    return Err(Error::Io(std::io::ErrorKind::ConnectionRefused.into()));
                }
                let (client, server) = tokio::io::duplex(4096);
                let home_lock = std::rc::Rc::clone(&self.home_lock);
                let shutdowns = std::rc::Rc::clone(&self.shutdowns);
                let version = self.version;
                tokio::task::spawn_local(async move {
                    let mut server = tokio::io::BufReader::new(server);
                    let mut request = String::new();
                    server.read_line(&mut request).await.expect("request");
                    let request: serde_json::Value = serde_json::from_str(&request).expect("RPC");
                    let result = match request["method"].as_str() {
                        Some("control.v1.health") => serde_json::json!({
                            "protocolVersion": PROTOCOL_VERSION,
                            "buildVersion": version,
                        }),
                        Some("control.v1.shutdown") => {
                            shutdowns.set(shutdowns.get() + 1);
                            home_lock.borrow_mut().take();
                            serde_json::json!({})
                        }
                        method => panic!("unexpected method {method:?}"),
                    };
                    let response = serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
                    server
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .expect("response");
                });
                Ok(Box::new(client) as Box<dyn agent::control_api::Connection>)
            })
        }
    }

    #[tokio::test(flavor = "local")]
    async fn an_agentd_started_during_the_update_is_stopped_again() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let home = ControlPlaneHome::resolve(Some(temporary.path())).expect("home");
        let daemon = Daemon {
            home_lock: std::rc::Rc::new(std::cell::RefCell::new(Some(home.acquire_lock().expect("home lock")))),
            shutdowns: std::rc::Rc::default(),
            version: "v1.0.0",
        };
        let shutdowns = std::rc::Rc::clone(&daemon.shutdowns);
        let client = Client::new(std::rc::Rc::new(daemon));

        let lock = tokio::time::timeout(Duration::from_secs(10), acquire_home_lock(&home, &client))
            .await
            .expect("the home within seconds")
            .expect("home lock");

        assert_eq!(shutdowns.get(), 1);
        assert!(home.acquire_lock().is_err(), "the updater holds the home");
        drop(lock);
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn the_target_is_started_again_once_an_agentd_that_took_its_home_exits() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let home = ControlPlaneHome::resolve(Some(temporary.path())).expect("home");
        let replaced = home.acquire_lock().expect("home taken by the replaced release");
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            drop(replaced);
        });
        let target = Daemon {
            home_lock: std::rc::Rc::default(),
            shutdowns: std::rc::Rc::default(),
            version: "v2.0.0",
        };
        let target_home = std::rc::Rc::clone(&target.home_lock);
        let client = Client::new(std::rc::Rc::new(target));
        let starts = std::cell::Cell::new(0);

        verify_target(&client, &home, "v2.0.0", || {
            starts.set(starts.get() + 1);
            *target_home.borrow_mut() = Some(home.acquire_lock()?);
            Ok(())
        })
        .await
        .expect("target verified");

        assert_eq!(starts.get(), 1, "started once the home was free, and not before");
    }

    #[cfg(unix)]
    #[test]
    fn target_validation_accepts_an_installation_alias() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let real = temporary.path().join("real");
        let release = real.join("releases/v0.2.0-linux-x86_64");
        std::fs::create_dir_all(&release).expect("release");
        let alias = temporary.path().join("alias");
        symlink(&real, &alias).expect("alias");
        let paths = InstallPaths::new(real, temporary.path().join("bin")).expect("paths");

        assert_eq!(
            canonical_target(&paths, &alias.join("releases/v0.2.0-linux-x86_64")).expect("target"),
            std::fs::canonicalize(release).expect("canonical release")
        );
    }
}
