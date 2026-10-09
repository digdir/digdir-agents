#![allow(clippy::expect_used)]

mod support;

use std::{path::PathBuf, rc::Rc};

use agent::{
    AccessSpec, AgentId, Error, FailureKind, ReconcileFailure,
    control_plane::{AgentRecord, AgentStore as _, memory::InMemoryAgentStore},
    local::home::ControlPlaneHome,
    ssh::{self, Access, memory::InMemoryHostKeyStore},
};
use sandbox::{
    EnsureSandboxRequest, Platform, SandboxHandle, SandboxPath, SandboxService,
    execution::{ExecutionEvent, ExecutionSpec, ExitStatus, Program},
    memory,
};
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;

const AGENTCTL: &str = "/usr/local/bin/agentctl";

fn command(spec: &ExecutionSpec) -> Option<(&str, Vec<&str>)> {
    match spec.program() {
        Program::Command { executable, args } => Some((executable.as_str(), args.iter().map(String::as_str).collect())),
        Program::ImageEntrypoint => None,
    }
}

fn is_command(spec: &ExecutionSpec, executable: &str, expected: &[&str]) -> bool {
    command(spec).is_some_and(|(actual, args)| actual == executable && args == expected)
}

fn is_server_check(spec: &ExecutionSpec) -> bool {
    is_command(spec, "/usr/bin/test", &["-x", "/usr/sbin/sshd"])
}

fn is_systemctl_check(spec: &ExecutionSpec) -> bool {
    is_command(spec, "/usr/bin/test", &["-x", "/usr/bin/systemctl"])
}

fn is_environment_policy_check(spec: &ExecutionSpec) -> bool {
    is_command(
        spec,
        "/usr/bin/sudo",
        &[
            "-n",
            "/usr/sbin/sshd",
            "-T",
            "-f",
            "/var/lib/agent/ssh/sshd_config",
            "-C",
            "user=agent,host=localhost,addr=127.0.0.1,laddr=127.0.0.1,lport=2222",
        ],
    )
}

fn is_environment_snapshot(spec: &ExecutionSpec) -> bool {
    is_command(spec, "/usr/bin/env", &["-0"])
}

fn is_systemd_running_check(spec: &ExecutionSpec) -> bool {
    is_command(spec, "/usr/bin/test", &["-d", "/run/systemd/system"])
}

fn is_disable(spec: &ExecutionSpec) -> bool {
    is_command(
        spec,
        "/usr/bin/sudo",
        &["-n", "/usr/bin/systemctl", "disable", "--now", "agent-ssh.service"],
    )
}

fn is_state_check(spec: &ExecutionSpec) -> bool {
    is_command(spec, "/usr/bin/test", &["-e", "/var/lib/agent/ssh"])
}

fn exited(code: i32) -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started { process_id: None },
        ExecutionEvent::Exited(ExitStatus { code }),
    ]
}

fn environment(contents: &'static [u8]) -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started { process_id: None },
        ExecutionEvent::Stdout(contents.into()),
        ExecutionEvent::Exited(ExitStatus { code: 0 }),
    ]
}

fn valid_environment_policy() -> Vec<ExecutionEvent> {
    environment(b"permituserenvironment yes\nusepam no\n")
}

fn queue_valid_environment_policy(backend: &memory::Provider) {
    backend.queue_execution_events_matching(is_environment_policy_check, valid_environment_policy());
}

async fn read_guest_file(sandbox: &SandboxHandle, path: &str) -> Option<Vec<u8>> {
    let mut reader = sandbox.read_file(&SandboxPath::new(path)).await.ok()?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.expect("guest file bytes");
    Some(bytes)
}

fn record(name: &str, id: &str, ssh: bool) -> AgentRecord {
    let mut resource = support::agent(name);
    resource.metadata.generation = 1;
    if ssh {
        resource.spec.access = vec![AccessSpec::Ssh {}];
    }
    AgentRecord {
        id: id.parse::<AgentId>().expect("Agent ID"),
        source_directory: PathBuf::from("/source").join(name),
        manifest_path: None,
        env_file: None,
        agent: resource,
    }
}

struct Fixture {
    _directory: TempDir,
    home: ControlPlaneHome,
    store: Rc<InMemoryAgentStore>,
    keys: Rc<InMemoryHostKeyStore>,
    access: Access,
    backend: Rc<memory::Provider>,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let home = ControlPlaneHome::resolve(Some(&directory.path().join("agent-home"))).expect("home");
        home.prepare().expect("prepare home");
        let store = Rc::new(InMemoryAgentStore::new());
        let keys = Rc::new(InMemoryHostKeyStore::new());
        let access = Access::new(&home, PathBuf::from(AGENTCTL), keys.clone(), store.clone()).with_user_home(None);
        Self {
            _directory: directory,
            home,
            store,
            keys,
            access,
            backend: Rc::new(memory::Provider::new()),
        }
    }

    async fn store(&self, record: &AgentRecord, expected_generation: u64) {
        self.store
            .put(record.clone(), expected_generation)
            .await
            .expect("store record");
    }

    async fn sandbox(&self, record: &AgentRecord) -> SandboxHandle {
        let service = SandboxService::new(self.backend.clone());
        let spec = record
            .agent
            .spec
            .sandbox
            .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
        service
            .ensure(&EnsureSandboxRequest::new(
                record.sandbox_name().expect("Sandbox name"),
                spec,
            ))
            .await
            .expect("Sandbox")
    }

    fn ssh_home(&self) -> ssh::SshHome {
        ssh::SshHome::new(&self.home)
    }

    fn known_hosts(&self) -> String {
        std::fs::read_to_string(self.ssh_home().known_hosts_path()).unwrap_or_default()
    }

    /// What OpenSSH makes of the generated configuration for `agent`'s alias, when it dials the
    /// Agent through `agentctl`.
    #[cfg(unix)]
    fn resolved(&self, agent: &str) -> Option<String> {
        let output = std::process::Command::new("ssh")
            .arg("-F")
            .arg(self.ssh_home().config_path())
            .arg("-G")
            .arg(ssh::alias(agent))
            .output()
            .expect("the OpenSSH client resolves the generated configuration");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let resolved = String::from_utf8(output.stdout).expect("UTF-8 configuration");
        ssh::resolves_through_agentctl(&resolved, agent).then_some(resolved)
    }
}

#[tokio::test(flavor = "local")]
async fn access_is_idempotent_and_only_public_material_enters_the_guest() {
    let fixture = Fixture::new();
    let record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    fixture.store(&record, 0).await;
    let sandbox = fixture.sandbox(&record).await;
    for _ in 0..2 {
        fixture
            .backend
            .queue_execution_events_matching(is_server_check, exited(0));
        queue_valid_environment_policy(&fixture.backend);
        fixture.backend.queue_execution_events_matching(
            is_environment_snapshot,
            environment(
                b"PATH=/home/agent/.cargo/bin:/usr/local/go/bin:/usr/bin\0NODE_EXTRA_CA_CERTS=/.msb/tls/ca.pem\0GIT_USER_NAME=Agent #1 \"Reviewer\"\0TERM=dumb\0HOME=/image-home\0",
            ),
        );
    }

    assert!(fixture.access.reconcile(&record, &sandbox).await.expect("first pass"));
    let host_key = read_guest_file(&sandbox, "/var/lib/agent/ssh/ssh_host_ed25519_key")
        .await
        .expect("host key in guest");
    let authorized = read_guest_file(&sandbox, "/var/lib/agent/ssh/authorized_keys")
        .await
        .expect("authorized_keys in guest");
    let ssh_home = fixture.ssh_home();
    let client_private = std::fs::read_to_string(ssh_home.identity_path(record.id)).expect("client private key");
    let client_public = std::fs::read_to_string(ssh_home.public_identity_path(record.id)).expect("client public key");

    assert!(fixture.keys.contains(record.id));
    assert!(host_key.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(client_private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert_ne!(host_key, client_private.as_bytes(), "host and client keys differ");
    assert_eq!(authorized, client_public.as_bytes());
    #[cfg(unix)]
    {
        let resolved = fixture
            .resolved("worker")
            .expect("the alias dials the Agent through agentctl");
        let setting = |key: &str| {
            resolved
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
                .map(str::to_owned)
        };
        assert_eq!(setting("user").as_deref(), Some("agent"));
        assert_eq!(setting("hostkeyalias"), Some(format!("agent-{}", record.id)));
        assert_eq!(
            setting("identityfile").map(PathBuf::from),
            Some(ssh_home.identity_path(record.id))
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &std::path::Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&ssh_home.identity_path(record.id)), 0o600);
        assert_eq!(mode(&ssh_home.agent_directory(record.id)), 0o700);
        assert_eq!(mode(ssh_home.root()), 0o700);
    }
    let info = fixture.access.describe("worker").await.expect("descriptor");
    assert_eq!(info.alias, "agentctl-worker");
    assert_eq!(info.identity_file, ssh_home.identity_path(record.id));
    assert_eq!(info.proxy_command, format!("{AGENTCTL} ssh-proxy agent/worker"));

    assert!(fixture.access.reconcile(&record, &sandbox).await.expect("second pass"));
    assert_eq!(
        read_guest_file(&sandbox, "/var/lib/agent/ssh/ssh_host_ed25519_key").await,
        Some(host_key),
        "the incarnation keeps its host key"
    );
    assert_eq!(
        std::fs::read_to_string(ssh_home.identity_path(record.id)).expect("client key"),
        client_private,
        "the incarnation keeps its client key"
    );
    let guest_files = [
        "/var/lib/agent/ssh/ssh_host_ed25519_key",
        "/var/lib/agent/ssh/ssh_host_ed25519_key.pub",
        "/var/lib/agent/ssh/authorized_keys",
        "/var/lib/agent/ssh/sshd_config",
        "/etc/systemd/system/agent-ssh.service",
        "/home/agent/.ssh/environment",
    ];
    for path in guest_files {
        let contents = read_guest_file(&sandbox, path).await.expect("guest file");
        assert!(
            !String::from_utf8_lossy(&contents).contains(client_private.trim()),
            "{path} must not carry the client private key"
        );
    }
}

#[tokio::test(flavor = "local")]
async fn an_image_without_a_server_fails_permanently_before_any_key_exists() {
    let fixture = Fixture::new();
    let record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    fixture.store(&record, 0).await;
    let sandbox = fixture.sandbox(&record).await;
    fixture
        .backend
        .queue_execution_events_matching(is_server_check, exited(1));

    let error = fixture
        .access
        .reconcile(&record, &sandbox)
        .await
        .expect_err("missing server");
    assert!(
        matches!(&error, Error::Invalid(message) if message.contains("cannot provide SSH access") && message.contains("/usr/sbin/sshd is missing") && message.contains("re-apply"))
    );
    assert_eq!(ReconcileFailure::classify(&error).kind, FailureKind::Invalid);
    assert!(!fixture.keys.contains(record.id));
    assert!(!fixture.ssh_home().agent_directory(record.id).exists());
    assert!(!fixture.ssh_home().known_hosts_path().exists());
    assert!(
        read_guest_file(&sandbox, "/var/lib/agent/ssh/authorized_keys")
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "local")]
async fn an_image_without_systemd_support_fails_permanently() {
    for (predicate, expected) in [
        (is_systemctl_check as fn(&ExecutionSpec) -> bool, "systemctl is missing"),
        (is_systemd_running_check, "systemd is not the running init"),
    ] {
        let fixture = Fixture::new();
        let record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
        fixture.store(&record, 0).await;
        let sandbox = fixture.sandbox(&record).await;
        fixture.backend.queue_execution_events_matching(predicate, exited(1));

        let error = fixture
            .access
            .reconcile(&record, &sandbox)
            .await
            .expect_err("incomplete image contract");
        assert!(
            matches!(&error, Error::Invalid(message) if message.contains(expected)),
            "{error}"
        );
        assert!(!fixture.keys.contains(record.id));
    }
}

#[tokio::test(flavor = "local")]
async fn an_image_that_blocks_the_managed_environment_fails_permanently() {
    for response in [environment(b"permituserenvironment yes\nusepam yes\n"), exited(1)] {
        let fixture = Fixture::new();
        let record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
        fixture.store(&record, 0).await;
        let sandbox = fixture.sandbox(&record).await;
        fixture
            .backend
            .queue_execution_events_matching(is_environment_policy_check, response);

        let error = fixture
            .access
            .reconcile(&record, &sandbox)
            .await
            .expect_err("environment policy");
        assert!(
            matches!(&error, Error::Invalid(message) if message.contains("cannot provide SSH access")),
            "{error}"
        );
        assert!(
            fixture.keys.contains(record.id),
            "the real host key is retained for retry"
        );
        assert!(
            read_guest_file(&sandbox, "/var/lib/agent/ssh/ssh_host_ed25519_key")
                .await
                .is_some(),
            "the effective policy is evaluated with the real host key"
        );
        assert!(
            read_guest_file(&sandbox, "/var/lib/agent/ssh/authorized_keys")
                .await
                .is_none(),
            "login state is not installed before the policy passes"
        );
    }
}

#[tokio::test(flavor = "local")]
async fn a_failed_server_stop_keeps_the_state_for_the_next_pass() {
    let fixture = Fixture::new();
    let mut record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    fixture.store(&record, 0).await;
    let sandbox = fixture.sandbox(&record).await;
    queue_valid_environment_policy(&fixture.backend);
    assert!(fixture.access.reconcile(&record, &sandbox).await.expect("grant"));

    let known_hosts_before = fixture.known_hosts();
    record.agent.spec.access.clear();
    record.agent.metadata.generation = 2;
    fixture.store(&record, 1).await;
    fixture.backend.queue_execution_events_matching(
        is_disable,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stderr("Failed to stop agent-ssh.service: Connection timed out\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 1 }),
        ],
    );
    let error = fixture
        .access
        .reconcile(&record, &sandbox)
        .await
        .expect_err("a running server is not forgotten");
    assert!(
        matches!(&error, Error::SandboxSetup(message) if message.contains("Connection timed out")),
        "{error}"
    );
    assert!(
        fixture.keys.contains(record.id),
        "the host key stays while the server that holds it may still run"
    );
    assert_eq!(
        fixture.known_hosts(),
        known_hosts_before,
        "known_hosts keeps matching that server"
    );
    assert!(fixture.ssh_home().identity_path(record.id).is_file());
    assert!(
        read_guest_file(&sandbox, "/var/lib/agent/ssh/authorized_keys")
            .await
            .is_some(),
        "guest state stays until the server is confirmed stopped"
    );

    // A unit the image never shipped is the one failure that is not a running server.
    fixture.backend.queue_execution_events_matching(
        is_disable,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stderr("Failed to disable unit: Unit file agent-ssh.service does not exist.\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 1 }),
        ],
    );
    assert!(!fixture.access.reconcile(&record, &sandbox).await.expect("withdraw"));
    assert!(!fixture.keys.contains(record.id), "the withdrawal completes");
}

#[tokio::test(flavor = "local")]
async fn withdrawing_access_removes_guest_and_host_state() {
    let fixture = Fixture::new();
    let mut record = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    fixture.store(&record, 0).await;
    let sandbox = fixture.sandbox(&record).await;
    fixture
        .backend
        .queue_execution_events_matching(is_server_check, exited(0));
    queue_valid_environment_policy(&fixture.backend);
    assert!(fixture.access.reconcile(&record, &sandbox).await.expect("grant"));

    record.agent.spec.access.clear();
    record.agent.metadata.generation = 2;
    fixture.store(&record, 1).await;
    fixture
        .backend
        .queue_execution_events_matching(is_state_check, exited(0));
    assert!(!fixture.access.reconcile(&record, &sandbox).await.expect("withdraw"));

    assert!(!fixture.keys.contains(record.id));
    assert!(!fixture.ssh_home().agent_directory(record.id).exists());
    assert_eq!(fixture.known_hosts(), "");
    #[cfg(unix)]
    assert!(
        fixture.resolved("worker").is_none(),
        "the alias no longer reaches the Agent"
    );

    // A later pass finds no guest state.
    fixture
        .backend
        .queue_execution_events_matching(is_state_check, exited(1));
    assert!(!fixture.access.reconcile(&record, &sandbox).await.expect("steady"));
}

#[tokio::test(flavor = "local")]
async fn deletion_removes_host_material_and_config_lists_only_active_ssh_agents() {
    let fixture = Fixture::new();
    let worker = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    let reviewer = record("reviewer", "5c1f4a1e-0ad5-4a37-9c94-4c0f2e6d7a10", true);
    let plain = record("plain", "9e2d6b5a-3d3a-4a2b-8a3c-1f9d2c3b4a55", false);
    let mut leaving = record("leaving", "0b7e2f31-6a94-4d0e-9d61-3ac7d1a2b3c4", true);
    leaving.agent.metadata.deletion_timestamp = Some(time::OffsetDateTime::now_utc());
    for record in [&worker, &reviewer, &plain, &leaving] {
        fixture.store(record, 0).await;
    }
    let sandbox = fixture.sandbox(&worker).await;
    fixture
        .backend
        .queue_execution_events_matching(is_server_check, exited(0));
    queue_valid_environment_policy(&fixture.backend);
    assert!(fixture.access.reconcile(&worker, &sandbox).await.expect("grant"));

    #[cfg(unix)]
    for (agent, listed) in [
        ("worker", true),
        ("reviewer", true),
        ("plain", false),
        ("leaving", false),
    ] {
        assert_eq!(fixture.resolved(agent).is_some(), listed, "{agent}");
    }

    assert!(matches!(
        fixture.access.describe("plain").await,
        Err(Error::Invalid(message)) if message.contains("does not declare SSH access")
    ));
    assert!(matches!(fixture.access.describe("nobody").await, Err(Error::NotFound)));

    fixture.access.remove(&worker).await.expect("remove on deletion");
    assert!(!fixture.keys.contains(worker.id));
    assert!(!fixture.ssh_home().agent_directory(worker.id).exists());
    assert_eq!(fixture.known_hosts(), "");
    fixture.access.remove(&worker).await.expect("removal is idempotent");
}

#[tokio::test(flavor = "local")]
async fn descriptor_json_is_the_documented_shape() {
    let fixture = Fixture::new();
    let worker = record("worker", "38f41de4-6ff7-4679-ae46-678bc61e4dcb", true);
    fixture.store(&worker, 0).await;
    let info = fixture.access.describe("worker").await.expect("descriptor");
    let value = serde_json::to_value(&info).expect("JSON");
    let object = value.as_object().expect("object");
    let mut expected = [
        "type",
        "agent",
        "agentId",
        "alias",
        "user",
        "identityFile",
        "knownHostsFile",
        "configFile",
        "proxyCommand",
        "workingDirectory",
    ];
    expected.sort_unstable();
    assert_eq!(object.keys().map(String::as_str).collect::<Vec<_>>(), expected);
    assert_eq!(value["type"], "ssh");
    assert_eq!(value["agent"], "worker");
    assert_eq!(value["agentId"], "38f41de4-6ff7-4679-ae46-678bc61e4dcb");
    assert_eq!(value["alias"], "agentctl-worker");
    assert_eq!(value["user"], "agent");
    assert_eq!(value["proxyCommand"], format!("{AGENTCTL} ssh-proxy agent/worker"));
    assert_eq!(value["workingDirectory"], "/home/agent/code");
    let decoded: ssh::AccessInfo = serde_json::from_value(value).expect("round trip");
    assert_eq!(decoded, info);
}
