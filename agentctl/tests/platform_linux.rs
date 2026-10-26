#![allow(clippy::expect_used)]

use crate::support;

use std::{io::Cursor, path::PathBuf, rc::Rc};

use agent::{
    AgentId,
    control_plane::AgentRecord,
    sandbox::{PlatformAdapter as _, platform::Linux},
};
use sandbox::{
    EnsureSandboxRequest, Platform, SandboxPath, SandboxService,
    execution::{ExecutionEvent, ExitStatus, Program},
    memory,
};
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;

const PLATFORM_API_URL: &str = "http://host.microsandbox.internal:4100";

/// Setup steps whose progress is discarded.
fn setup_phase() -> sandbox::SandboxProgress {
    sandbox::ProgressReporter::from_callback(|_| {}).steps()
}

fn is_claude_version(spec: &sandbox::execution::ExecutionSpec) -> bool {
    matches!(
        spec.program(),
        Program::Command { executable, args }
            if executable.as_str() == "/usr/bin/env" && args == &["claude", "--version"]
    )
}

fn is_codex_version(spec: &sandbox::execution::ExecutionSpec) -> bool {
    matches!(
        spec.program(),
        Program::Command { executable, args }
            if executable.as_str() == "/usr/bin/env" && args == &["codex", "--version"]
    )
}

fn is_podman_presence_check(spec: &sandbox::execution::ExecutionSpec) -> bool {
    matches!(
        spec.program(),
        Program::Command { executable, args }
            if executable.as_str() == "/usr/bin/test" && args == &["-x", "/usr/bin/podman"]
    )
}

fn is_systemd_readiness_check(spec: &sandbox::execution::ExecutionSpec) -> bool {
    matches!(
        spec.program(),
        Program::Command { executable, args }
            if executable.as_str() == "/usr/bin/sudo" && args == &["-n", "/usr/bin/systemctl", "is-system-running", "--wait"]
    )
}

fn completed(code: i32) -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started { process_id: None },
        ExecutionEvent::Exited(ExitStatus { code }),
    ]
}

async fn read_file(sandbox: &sandbox::SandboxHandle, path: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    sandbox
        .read_file(&SandboxPath::new(path))
        .await
        .expect("read file")
        .read_to_end(&mut bytes)
        .await
        .expect("read file bytes");
    bytes
}

/// Every harness the agent declares, as preparation would report when all host logins are present.
fn declared(record: &AgentRecord) -> Vec<agent::Harness> {
    record
        .agent
        .spec
        .harnesses
        .iter()
        .map(|installation| installation.kind)
        .collect()
}

/// Setup acts on the installed set preparation reported, not on everything the agent declares.
///
/// The two run in the same convergence pass and must agree about an optional installation whose
/// host login was absent. Preparation decides and reports; setup is told. An omitted harness is
/// skipped entirely: not verified, not configured.
#[tokio::test(flavor = "local")]
#[allow(clippy::too_many_lines)]
async fn linux_setup_configures_only_the_harnesses_preparation_reported() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    let agent_id: AgentId = "5c0fd6ac-1d5a-4f8b-9f58-6b0f4de1f6c1".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.harnesses[0].default = true;
    resource.spec.harnesses.push(agent::HarnessSpec {
        kind: agent::Harness::Codex,
        version: None,
        auth: agent::HarnessAuthMode::Mediated,
        optional: true,
        default: false,
        defaults: agent::ModelSelection::default(),
    });
    resource.spec.secrets = vec![
        secret("GITHUB_TOKEN", &["github.com", "api.github.com"]),
        secret("GATEWAY_KEY", &["gateway.example.com"]),
    ];
    resource.spec.environment = vec![agent::EnvironmentSpec {
        name: "GIT_USER_NAME".into(),
        source: None,
    }];
    resource.spec.access = vec![agent::AccessSpec::Ssh {}];
    resource.spec.network.deny = vec!["metadata.google.internal".into()];
    let record = AgentRecord {
        id: agent_id,
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };

    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    // The optional gateway key was not configured, so preparation gave the sandbox no placeholder.
    let sandbox = service
        .ensure(
            &EnsureSandboxRequest::new(record.sandbox_name().expect("Sandbox name"), spec)
                .with_environment([("GITHUB_TOKEN".into(), "placeholder".into())]),
        )
        .await
        .expect("Sandbox");

    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &[agent::Harness::ClaudeCode], &setup_phase())
        .await
        .expect("setup");

    let writes = backend
        .file_writes()
        .into_iter()
        .map(|path| path.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        writes.iter().any(|path| path == "/home/agent/.claude/CLAUDE.md"),
        "the required harness is still configured: {writes:?}"
    );
    // Verifying Codex would fail: nothing answers `codex --version`.
    assert!(
        !writes.iter().any(|path| path.starts_with("/home/agent/.codex/")),
        "the omitted harness must not be configured: {writes:?}"
    );

    let instructions = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    let expected = format!(
        "<!-- agentd: Agent sandbox section, rewritten on every pass -->
# Agent sandbox

Written by agentd {}. Instructions after this section are the agent's own and take precedence.

- **Computer:** agent `worker`, a Linux sandbox VM that is yours to manage. User `agent` with passwordless `sudo`, \
home `/home/agent`, work in `/home/agent/code`.
- **Resources:** cpu 2, memory 1Gi, disk 4Gi; `df -h /` shows how much is free.
- **Harnesses:**
  - Claude Code (default)
  - Codex: not installed until the host logs in to it
- **Secrets:** inert placeholders the host replaces only in requests to their hosts; never search for, print or \
persist real values.
  - `GITHUB_TOKEN`: github.com, api.github.com
  - `GATEWAY_KEY`: not configured
- **Plain environment:** `GIT_USER_NAME`.
- **Network:** all traffic passes through the host, which intercepts TLS with a CA that the system store and \
`NODE_EXTRA_CA_CERTS` trust. Denied: `metadata.google.internal`.
- **Access:** people on the host reach this computer with `agentctl ssh worker`.
- **Nested agents:** available (`/dev/kvm`, no Podman, so only manifests with an image `reference`); see the \
`agentctl` skill.
<!-- agentd: end of Agent sandbox section -->

test instructions
",
        agent::build_version()
    );
    assert_eq!(instructions, expected);
}

/// Without `spec.instructions` the agent's image or home overlay may own the harness's instruction file: the agent
/// sandbox section goes above what the file holds, and a later pass replaces only the section. A skill of the agent's
/// own named `agentctl` replaces the platform's.
#[tokio::test(flavor = "local")]
async fn linux_setup_places_the_section_above_an_instruction_file_the_image_owns() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    let skill = directory.path().join("skills").join("agentctl");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(skill.join("SKILL.md"), "---\nname: agentctl\n---\nour own").expect("skill file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.instructions = Vec::new();
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/agentctl"),
        name: None,
        optional: false,
    }];
    let record = AgentRecord {
        id: "0b6f3c7e-4a8d-4c55-9a3e-2f1d7c9b8e10".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    for _ in 0..2 {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");
    let image_instructions = b"# The image's own\n";
    sandbox
        .write_file(
            &SandboxPath::new("/home/agent/.claude/CLAUDE.md"),
            Box::pin(Cursor::new(image_instructions.to_vec())),
        )
        .await
        .expect("image instruction file");
    let platform = Linux::new(PLATFORM_API_URL);

    platform
        .setup(&record, &sandbox, &[agent::Harness::ClaudeCode], &setup_phase())
        .await
        .expect("first setup");
    let first = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    let writes = backend.file_writes().len();
    platform
        .setup(&record, &sandbox, &[agent::Harness::ClaudeCode], &setup_phase())
        .await
        .expect("second setup");

    assert!(first.starts_with("<!-- agentd: Agent sandbox section"), "{first}");
    assert!(
        first.ends_with("<!-- agentd: end of Agent sandbox section -->\n\n# The image's own\n"),
        "{first}"
    );
    let second = read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await;
    assert_eq!(
        second,
        first.as_bytes(),
        "a later pass keeps one section above the image's text"
    );
    assert!(
        !backend
            .file_writes()
            .iter()
            .skip(writes)
            .any(|path| path.as_str() == "/home/agent/.claude/CLAUDE.md"),
        "an unchanged pass does not rewrite the file"
    );
    let skill = read_file(&sandbox, "/home/agent/.claude/skills/agentctl/SKILL.md").await;
    assert_eq!(skill, b"---\nname: agentctl\n---\nour own");
}

/// Claude Code that the platform does not launch, such as T3 Code's, approves Bash requests in bypass mode as sessions
/// do, and asks in the modes a person picks to be asked in. Sessions still report other permission prompts.
#[tokio::test(flavor = "local")]
async fn linux_setup_approves_claude_bash_requests_in_bypass_mode_for_every_claude_process() {
    use std::{io::Write as _, process::Stdio};

    const MANAGED_SETTINGS: &str = "/etc/claude-code/managed-settings.d/10-agent-platform.json";
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions").expect("instruction file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.harnesses[0].version = None;
    let record = AgentRecord {
        id: "5d0c2f7a-93b1-4e8e-8c4f-1a6b2e9d7f30".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    for _ in 0..2 {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");
    let platform = Linux::new(PLATFORM_API_URL);

    platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("first setup");
    let writes = backend.file_writes().len();
    platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("second setup");

    let settings = |bytes: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&bytes).expect("settings JSON");
    let input = serde_json::json!({
        "session": settings(read_file(&sandbox, "/home/agent/.claude/agent-settings.json").await),
        "managed": settings(read_file(&sandbox, MANAGED_SETTINGS).await),
    });
    let mut child = std::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/claude_permission_hooks.mjs"
        ))
        .stdin(Stdio::piped())
        .spawn()
        .expect("Node.js answers permission requests");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.to_string().as_bytes())
        .expect("write settings");
    assert!(child.wait().expect("permission requests answered").success());
    assert!(
        !backend
            .file_writes()
            .iter()
            .skip(writes)
            .any(|path| path.as_str() == MANAGED_SETTINGS),
        "an unchanged pass does not rewrite the managed settings"
    );
}

fn secret(environment: &str, hosts: &[&str]) -> agent::SecretSpec {
    agent::SecretSpec {
        environment: environment.into(),
        optional: environment != "GITHUB_TOKEN",
        placeholder: None,
        allowed_hosts: hosts.iter().map(|host| (*host).into()).collect(),
        source: None,
    }
}

#[tokio::test(flavor = "local")]
#[allow(clippy::too_many_lines)]
async fn linux_setup_rewrites_configuration_without_owning_workspace_initialization() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    std::fs::write(
        directory.path().join("environment.md"),
        "# Environment\n\nhas a browser\n",
    )
    .expect("environment file");
    let skill = directory.path().join("skills").join("evidence");
    std::fs::create_dir_all(skill.join("references")).expect("skill directory");
    std::fs::write(skill.join("SKILL.md"), "---\nname: evidence\n---\ncapture").expect("skill file");
    std::fs::write(skill.join("references").join("gif.md"), "palette").expect("skill reference");
    let agent_id: AgentId = "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/evidence"),
        name: None,
        optional: false,
    }];
    resource.spec.instructions.push(agent::InstructionsSpec {
        source: PathBuf::from("environment.md"),
        optional: false,
    });
    resource.spec.harnesses[0].default = true;
    resource.spec.harnesses.push(agent::HarnessSpec {
        kind: agent::Harness::Codex,
        version: Some("0.149.1".into()),
        auth: agent::HarnessAuthMode::Mediated,
        optional: false,
        default: false,
        defaults: agent::ModelSelection::default(),
    });
    let record = AgentRecord {
        id: agent_id,
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };

    let backend = Rc::new(memory::Provider::new());
    for _ in 0..2 {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(
            is_codex_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("codex-cli 0.149.1\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(
            &EnsureSandboxRequest::new(record.sandbox_name().expect("Sandbox name"), spec)
                .with_environment([("AGENT_CODEX_ACCOUNT_ID".into(), "account-test".into())]),
        )
        .await
        .expect("Sandbox");
    let platform = Linux::new(PLATFORM_API_URL);

    platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("first setup");
    let first_pass_writes = backend.file_writes();
    let mutable_state = br#"{"theme":"light","projects":{"/home/agent/code/example":{"hasTrustDialogAccepted":true}}}"#;
    sandbox
        .write_file(
            &SandboxPath::new("/home/agent/.claude/.claude.json"),
            Box::pin(Cursor::new(mutable_state.to_vec())),
        )
        .await
        .expect("write harness-owned state");
    platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("second setup");

    // Harnesses watch their configuration and skills live: a pass that changes nothing must not
    // rewrite them. Only the home archive, consumed by tar and watched by nobody, is re-sent.
    let second_pass_writes = backend
        .file_writes()
        .into_iter()
        .skip(first_pass_writes.len() + 1)
        .map(|path| path.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(second_pass_writes, ["/tmp/agent-home.tar"]);
    assert!(
        first_pass_writes
            .iter()
            .any(|path| path.as_str() == "/home/agent/.claude/skills/evidence/SKILL.md")
    );

    let preserved = read_file(&sandbox, "/home/agent/.claude/.claude.json").await;
    assert_eq!(preserved, mutable_state);
    let instructions = read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await;
    let instructions = String::from_utf8(instructions).expect("UTF-8 instructions");
    assert!(instructions.contains("\n# Agent sandbox\n"), "{instructions}");
    assert!(
        instructions.ends_with("\n\ntest instructions\n\n# Environment\n\nhas a browser\n"),
        "the agent's own instructions follow the agent sandbox section: {instructions}"
    );
    let codex_instructions = read_file(&sandbox, "/home/agent/.codex/AGENTS.md").await;
    assert_eq!(codex_instructions, instructions.as_bytes());
    for root in ["/home/agent/.claude/skills", "/home/agent/.agents/skills"] {
        let skill = read_file(&sandbox, &format!("{root}/agentctl/SKILL.md")).await;
        assert!(skill.starts_with(b"---\nname: agentctl\n"));
        let skill = read_file(&sandbox, &format!("{root}/evidence/SKILL.md")).await;
        assert_eq!(skill, b"---\nname: evidence\n---\ncapture");
        let reference = read_file(&sandbox, &format!("{root}/evidence/references/gif.md")).await;
        assert_eq!(reference, b"palette");
    }
}

#[tokio::test(flavor = "local")]
#[allow(clippy::too_many_lines)]
async fn linux_setup_waits_for_systemd_and_restores_managed_podman_configuration() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions").expect("instruction file");
    let agent_id: AgentId = "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    let record = AgentRecord {
        id: agent_id,
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    for _ in 0..2 {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(0));
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    // The first setup pass races the image init: systemd is not PID 1 yet, then boots degraded.
    backend.queue_execution_events_matching(
        is_systemd_readiness_check,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stderr(
                "System has not been booted with systemd as init system (PID 1). Can't operate.\n".into(),
            ),
            ExecutionEvent::Exited(ExitStatus { code: 1 }),
        ],
    );
    backend.queue_execution_events_matching(
        is_systemd_readiness_check,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("degraded\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 1 }),
        ],
    );
    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("the first setup waits for systemd to boot");
    let managed = "/etc/containers/containers.conf.d/50-agent-ca.conf";
    let written = read_file(&sandbox, managed).await;
    sandbox
        .write_file(&SandboxPath::new(managed), Box::pin(Cursor::new(b"stale\n".to_vec())))
        .await
        .expect("replace managed configuration");
    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("second setup");

    assert_eq!(
        read_file(&sandbox, managed).await,
        written,
        "a replaced managed file is restored"
    );
}

#[tokio::test(flavor = "local")]
async fn linux_setup_accepts_any_installed_version_when_none_is_declared() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions").expect("instruction file");
    let agent_id: AgentId = "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.harnesses[0].version = None;
    let record = AgentRecord {
        id: agent_id,
        source_directory: PathBuf::from(directory.path()),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.258 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup without a declared version");
}

/// A restarted `agentd` listens on a new port; its first setup pass moves harnesses that kept
/// running onto it.
#[tokio::test(flavor = "local")]
async fn linux_setup_points_harness_hooks_at_the_current_platform_api_url() {
    const RESTARTED_URL: &str = "http://host.microsandbox.internal:4200";
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions").expect("instruction file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.harnesses[0].version = None;
    let record = AgentRecord {
        id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
        source_directory: PathBuf::from(directory.path()),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    for _ in 0..2 {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.258 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("first setup");
    assert_eq!(
        read_file(&sandbox, "/etc/agent-platform-api-url")
            .await
            .trim_ascii_end(),
        PLATFORM_API_URL.as_bytes()
    );

    Linux::new(RESTARTED_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup after a restart");
    assert_eq!(
        read_file(&sandbox, "/etc/agent-platform-api-url")
            .await
            .trim_ascii_end(),
        RESTARTED_URL.as_bytes()
    );
}

#[tokio::test(flavor = "local")]
async fn linux_setup_rejects_partial_git_identity() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.instructions.clear();
    let record = AgentRecord {
        id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(
            &EnsureSandboxRequest::new(record.sandbox_name().expect("Sandbox name"), spec)
                .with_environment([("GIT_USER_NAME".into(), "Test User".into())]),
        )
        .await
        .expect("Sandbox");

    let error = Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect_err("partial Git identity");

    assert!(matches!(error, agent::Error::Invalid(message) if message.contains("must both be configured")));
}

#[tokio::test(flavor = "local")]
async fn linux_setup_rejects_a_declared_harness_version_mismatch_before_injection() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    let agent_id: AgentId = "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    let record = AgentRecord {
        id: agent_id,
        source_directory: PathBuf::from(directory.path()),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.240 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    let error = Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect_err("version mismatch");

    assert!(error.to_string().contains("does not match installed version"));
    assert_eq!(
        backend.file_writes(),
        [SandboxPath::new("/etc/agent-platform-api-url")],
        "verification must happen before injection; only the Platform API URL precedes it"
    );
    assert_eq!(
        read_file(&sandbox, "/etc/agent-platform-api-url")
            .await
            .trim_ascii_end(),
        PLATFORM_API_URL.as_bytes(),
        "a pass that fails later still points running harnesses at the current endpoint"
    );
}

#[tokio::test(flavor = "local")]
async fn linux_setup_reads_home_instructions_and_skills_below_the_home_directory_for_a_tilde() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(home.join("tilde-marker"), "home").expect("home file");
    std::fs::write(directory.path().join("instructions.md"), "tilde instructions\n").expect("instruction file");
    let skill = directory.path().join("skills").join("evidence");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(skill.join("SKILL.md"), "capture").expect("skill file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = support::via_home(&home);
    resource.spec.instructions = vec![agent::InstructionsSpec {
        source: support::via_home(&directory.path().join("instructions.md")),
        optional: false,
    }];
    resource.spec.skills = vec![agent::SkillSpec {
        source: support::via_home(&skill),
        name: None,
        optional: false,
    }];
    let record = AgentRecord {
        id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
        source_directory: directory.path().join("manifest"),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend);
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup");

    let archive = read_file(&sandbox, "/tmp/agent-home.tar").await;
    assert!(
        archive
            .windows(b"tilde-marker".len())
            .any(|window| window == b"tilde-marker")
    );
    let instructions = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    assert!(instructions.ends_with("\n\ntilde instructions\n"), "{instructions}");
    assert_eq!(
        read_file(&sandbox, "/home/agent/.claude/skills/evidence/SKILL.md").await,
        b"capture"
    );
}

// The host is what holds the FIFO; Windows has no mkfifo, and the Linux sandbox setup runs the same
// walker on every host, so one Unix host exercising it is enough.
#[cfg(unix)]
#[tokio::test(flavor = "local")]
async fn linux_setup_rejects_a_skill_tree_with_a_fifo_instead_of_blocking() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    let skill = directory.path().join("skills").join("evidence");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(skill.join("SKILL.md"), "capture").expect("skill file");
    let status = std::process::Command::new("mkfifo")
        .arg(skill.join("pipe"))
        .status()
        .expect("mkfifo runs");
    assert!(status.success());
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.instructions.clear();
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/evidence"),
        name: None,
        optional: false,
    }];
    let record = AgentRecord {
        id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(
        is_claude_version,
        vec![
            ExecutionEvent::Started { process_id: None },
            ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
            ExecutionEvent::Exited(ExitStatus { code: 0 }),
        ],
    );
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend);
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    let error = Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect_err("FIFO must be rejected");

    assert!(
        matches!(&error, agent::Error::Invalid(message) if message.contains("non-regular file pipe")),
        "unexpected error: {error:?}"
    );
}

/// A sandbox for `record` whose Claude Code version check answers `passes` setups.
async fn claude_sandbox(record: &AgentRecord, passes: usize) -> sandbox::SandboxHandle {
    let backend = Rc::new(memory::Provider::new());
    for _ in 0..passes {
        backend.queue_execution_events_matching(
            is_claude_version,
            vec![
                ExecutionEvent::Started { process_id: None },
                ExecutionEvent::Stdout("2.1.266 (Claude Code)\n".into()),
                ExecutionEvent::Exited(ExitStatus { code: 0 }),
            ],
        );
        backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    }
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture)
        .expect("Sandbox spec");
    SandboxService::new(backend)
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox")
}

/// A record in `directory` with one required instruction file and the given optional sources.
fn optional_sources_record(directory: &std::path::Path, instructions: &str, skill: &str) -> AgentRecord {
    let home = directory.join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.join("instructions.md"), "required\n").expect("instruction file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home.source = home;
    resource.spec.instructions.push(agent::InstructionsSpec {
        source: PathBuf::from(instructions),
        optional: true,
    });
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from(skill),
        name: None,
        optional: true,
    }];
    AgentRecord {
        id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
        source_directory: directory.to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    }
}

/// Optional sources that do not exist are left out and reported, and the next pass installs them
/// once they exist, with no change to the agent.
#[tokio::test(flavor = "local")]
async fn linux_setup_leaves_out_missing_optional_sources_until_they_exist() {
    let directory = TempDir::new().expect("temporary directory");
    let record = optional_sources_record(directory.path(), "personal.md", "skills/review");
    let sandbox = claude_sandbox(&record, 2).await;
    let platform = Linux::new(PLATFORM_API_URL);

    let missing = platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup without the optional sources");

    assert_eq!(
        missing,
        [
            "spec.instructions[1].source personal.md",
            "spec.skills[0].source skills/review",
        ]
    );
    let instructions = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    assert!(instructions.ends_with("\n\nrequired\n"), "{instructions}");

    std::fs::write(directory.path().join("personal.md"), "personal\n").expect("optional instruction file");
    let skill = directory.path().join("skills").join("review");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(skill.join("SKILL.md"), "review").expect("skill file");
    let missing = platform
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup with the optional sources");

    assert!(missing.is_empty(), "{missing:?}");
    let instructions = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    assert!(instructions.ends_with("\n\nrequired\n\npersonal\n"), "{instructions}");
    assert_eq!(
        read_file(&sandbox, "/home/agent/.claude/skills/review/SKILL.md").await,
        b"review"
    );
}

/// `optional` forgives only a source that does not exist: one that exists but cannot be used still
/// fails setup, and so does a missing source that is required.
#[tokio::test(flavor = "local")]
async fn linux_setup_fails_on_optional_sources_that_exist_but_cannot_be_used() {
    let directory = TempDir::new().expect("temporary directory");
    std::fs::create_dir_all(directory.path().join("skills").join("review")).expect("skill without SKILL.md");
    std::fs::create_dir_all(directory.path().join("folder.md")).expect("directory as instruction file");
    let cases = [
        (
            "folder.md",
            "skills/absent",
            "spec.instructions[1].source must identify a file",
        ),
        (
            "absent.md",
            "skills/review",
            "spec.skills[0].source must contain SKILL.md",
        ),
    ];
    for (instructions, skill, expected) in cases {
        let record = optional_sources_record(directory.path(), instructions, skill);
        let sandbox = claude_sandbox(&record, 1).await;

        let error = Linux::new(PLATFORM_API_URL)
            .setup(&record, &sandbox, &declared(&record), &setup_phase())
            .await
            .expect_err(expected);

        assert!(
            matches!(&error, agent::Error::Invalid(message) if message == expected),
            "unexpected error: {error:?}"
        );
    }

    let mut record = optional_sources_record(directory.path(), "absent.md", "skills/absent");
    record.agent.spec.instructions[1].optional = false;
    let sandbox = claude_sandbox(&record, 1).await;
    Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect_err("a required instruction file must exist");
}
