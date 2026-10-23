#![allow(clippy::expect_used)]

mod support;

use std::{io::Cursor, path::PathBuf, rc::Rc};

use agent::{
    AgentId, HomeSpec,
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

/// Every harness the Agent declares, as preparation would report when all host logins are present.
fn declared(record: &AgentRecord) -> Vec<agent::Harness> {
    record
        .agent
        .spec
        .harnesses
        .iter()
        .map(|installation| installation.kind)
        .collect()
}

/// Setup acts on the installed set preparation reported, not on everything the Agent declares.
///
/// The two run in the same convergence pass and must agree about an optional installation whose
/// host login was absent. Preparation decides and reports; setup is told. An omitted harness is
/// skipped entirely: not verified, not configured.
#[tokio::test(flavor = "local")]
async fn linux_setup_configures_only_the_harnesses_preparation_reported() {
    let directory = TempDir::new().expect("temporary directory");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    let agent_id: AgentId = "5c0fd6ac-1d5a-4f8b-9f58-6b0f4de1f6c1".parse().expect("Agent ID");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home = Some(HomeSpec { source: home });
    resource.spec.harnesses[0].default = true;
    resource.spec.harnesses.push(agent::HarnessSpec {
        kind: agent::Harness::Codex,
        version: None,
        auth: agent::HarnessAuthMode::Mediated,
        optional: true,
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
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
}

/// The arguments of a `git` command run through `/usr/bin/env`, without the leading `git`.
fn git_args(spec: &sandbox::execution::ExecutionSpec) -> Option<Vec<String>> {
    match spec.program() {
        Program::Command { executable, args }
            if executable.as_str() == "/usr/bin/env" && args.first().map(String::as_str) == Some("git") =>
        {
            Some(args[1..].to_vec())
        }
        _ => None,
    }
}

fn answers(stdout: &str, code: i32) -> Vec<ExecutionEvent> {
    vec![
        ExecutionEvent::Started { process_id: None },
        ExecutionEvent::Stdout(stdout.to_owned().into()),
        ExecutionEvent::Exited(ExitStatus { code }),
    ]
}

/// Runs Linux setup for a Claude Code Agent with or without the GitHub account, with `configured`
/// answering Git's lookups as the image's own configuration would, and returns every `git`
/// command it ran.
async fn git_commands_during_setup(github_account: bool, configured: Option<(&str, &str)>) -> Vec<Vec<String>> {
    let directory = TempDir::new().expect("temporary directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home = None;
    if github_account {
        resource.spec.accounts = vec![agent::AccountSpec::Github { owner: None }];
    }
    let record = AgentRecord {
        id: "2f6c3a1e-7d4b-4c8e-9a0f-1b2c3d4e5f60".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(is_claude_version, answers("2.1.266 (Claude Code)\n", 0));
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let (helper, rewrites) = configured.map_or((("", 1), ("", 1)), |(helper, rewrites)| ((helper, 0), (rewrites, 0)));
    backend.queue_execution_events_matching(
        |spec| git_args(spec).is_some_and(|args| args.iter().any(|arg| arg == "--get-urlmatch")),
        answers(helper.0, helper.1),
    );
    backend.queue_execution_events_matching(
        |spec| git_args(spec).is_some_and(|args| args.iter().any(|arg| arg == "--get-regexp")),
        answers(rewrites.0, rewrites.1),
    );
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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

    backend.execution_specs().iter().filter_map(git_args).collect()
}

fn writes(commands: &[Vec<String>]) -> Vec<Vec<String>> {
    commands
        .iter()
        .filter(|args| args.len() > 2 && args[0] == "config" && args[1] == "--global")
        .cloned()
        .collect()
}

/// Git over HTTPS reaches GitHub with the mediated token even when the image configured nothing,
/// and GitHub's SSH URLs, which the Sandbox has no key for, go over HTTPS instead.
#[tokio::test(flavor = "local")]
async fn linux_setup_gives_git_the_github_account_over_https() {
    let commands = git_commands_during_setup(true, None).await;

    let writes = writes(&commands);
    assert_eq!(writes.len(), 3, "{writes:?}");
    assert_eq!(writes[0][2], "credential.https://github.com.helper", "{writes:?}");
    assert!(writes[0][3].contains("password=$GITHUB_TOKEN"), "{writes:?}");
    assert!(
        !writes[0][3].contains("github_pat"),
        "only the variable, never a value: {writes:?}"
    );
    assert_eq!(
        writes[1..],
        [
            [
                "config",
                "--global",
                "--add",
                "url.https://github.com/.insteadOf",
                "git@github.com:"
            ]
            .map(String::from),
            [
                "config",
                "--global",
                "--add",
                "url.https://github.com/.insteadOf",
                "ssh://git@github.com/"
            ]
            .map(String::from),
        ]
    );
}

/// An image that set up GitHub credentials and URL rewrites keeps them.
#[tokio::test(flavor = "local")]
async fn linux_setup_keeps_the_images_own_github_configuration() {
    let commands = git_commands_during_setup(
        true,
        Some((
            "!/usr/local/bin/gh auth git-credential",
            "url.https://github.com/.insteadof git@github.com:\nurl.https://github.com/.insteadof ssh://git@github.com/",
        )),
    )
    .await;

    assert!(writes(&commands).is_empty(), "{commands:?}");
}

/// Without the GitHub account or a Git identity, setup leaves Git alone.
#[tokio::test(flavor = "local")]
async fn linux_setup_without_the_github_account_runs_no_git() {
    assert!(git_commands_during_setup(false, None).await.is_empty());
}

/// Runs Linux setup for an agent with `instructions` as its own instructions file, if any, on a
/// host whose instructions file holds `host`, if it exists, and returns the `CLAUDE.md` written.
async fn claude_instructions_after_setup(host: Option<&str>, instructions: Option<&str>) -> Option<String> {
    let directory = TempDir::new().expect("temporary directory");
    let host_path = directory.path().join("host-instructions.md");
    if let Some(host) = host {
        std::fs::write(&host_path, host).expect("host instructions");
    }
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home = None;
    resource.spec.instructions.clear();
    if let Some(instructions) = instructions {
        std::fs::write(directory.path().join("instructions.md"), instructions).expect("instructions");
        resource.spec.instructions.push(agent::InstructionsSpec {
            source: PathBuf::from("instructions.md"),
        });
    }
    let record = AgentRecord {
        id: "7b1e2c3d-4f5a-4b6c-8d7e-9f0a1b2c3d4e".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(is_claude_version, answers("2.1.266 (Claude Code)\n", 0));
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");

    Linux::new(PLATFORM_API_URL)
        .with_host_instructions(&host_path)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await
        .expect("setup");

    let written = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    after_the_platforms(&written).map(str::to_owned)
}

/// The last line of the platform's instructions, which every agent gets first.
const PLATFORM_END: &str = "or another folder or repository, ask the person to add it.\n";

/// What `instructions` hold after the platform's own, or `None` when they do not start with them.
fn after_the_platforms(instructions: &str) -> Option<&str> {
    let rest = instructions
        .strip_prefix("# About this sandbox\n")?
        .split_once(PLATFORM_END)?
        .1;
    Some(rest.strip_prefix('\n').unwrap_or(rest))
}

/// The host's instructions reach every agent, ahead of the agent's own.
#[tokio::test(flavor = "local")]
async fn linux_setup_puts_the_host_instructions_ahead_of_the_agents_own() {
    assert_eq!(
        claude_instructions_after_setup(Some("Host rules\n"), Some("Repository rules\n")).await,
        Some("Host rules\n\nRepository rules\n".to_owned())
    );
    assert_eq!(
        claude_instructions_after_setup(Some("Host rules\n"), None).await,
        Some("Host rules\n".to_owned()),
        "an agent without instructions of its own still gets the host's"
    );
    assert_eq!(
        claude_instructions_after_setup(None, Some("Repository rules\n")).await,
        Some("Repository rules\n".to_owned()),
        "no host file changes nothing"
    );
}

/// Emptying the host's file clears what it gave agents, and an agent without any instructions still
/// gets the platform's.
#[tokio::test(flavor = "local")]
async fn an_empty_host_file_clears_its_instructions_and_the_platforms_remain() {
    assert_eq!(
        claude_instructions_after_setup(Some(""), None).await,
        Some(String::new())
    );
    assert_eq!(claude_instructions_after_setup(None, None).await, Some(String::new()));
}

/// Runs Linux setup for a Claude Code agent whose Claude Code state file holds `existing`, if
/// any, and returns the state file afterwards and whether setup wrote it.
/// An Agent without `spec.home` copies nothing into the guest home, but is otherwise set up.
#[tokio::test(flavor = "local")]
async fn linux_setup_without_a_home_copies_nothing_into_the_guest_home() {
    let directory = TempDir::new().expect("temporary directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home = None;
    let record = AgentRecord {
        id: "0d9b5f5e-8a8f-4d0e-9a51-5b1f6f0c2a37".parse().expect("Agent ID"),
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
        .expect("setup without a home");

    let writes = backend
        .file_writes()
        .into_iter()
        .map(|path| path.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !writes.iter().any(|path| path == "/tmp/agent-home.tar"),
        "no home archive is written: {writes:?}"
    );
    assert!(
        writes.iter().any(|path| path == "/home/agent/.claude/CLAUDE.md"),
        "the harness is still configured: {writes:?}"
    );
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
    resource.spec.home = Some(HomeSpec { source: home });
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/evidence"),
        name: None,
    }];
    resource.spec.instructions.push(agent::InstructionsSpec {
        source: PathBuf::from("environment.md"),
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
    assert_eq!(
        after_the_platforms(std::str::from_utf8(&instructions).expect("UTF-8")),
        Some("test instructions\n\n# Environment\n\nhas a browser\n")
    );
    let codex_instructions = read_file(&sandbox, "/home/agent/.codex/AGENTS.md").await;
    assert_eq!(codex_instructions, instructions);
    for root in ["/home/agent/.claude/skills", "/home/agent/.agents/skills"] {
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
    resource.spec.home = Some(HomeSpec { source: home });
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
    resource.spec.home = Some(HomeSpec { source: home });
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
    resource.spec.home = Some(HomeSpec { source: home });
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
    resource.spec.home = Some(HomeSpec { source: home });
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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
    resource.spec.home = Some(HomeSpec { source: home });
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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

// The host is what holds the FIFO; Windows has no mkfifo, and the Linux Sandbox setup runs the same
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
    resource.spec.home = Some(HomeSpec { source: home });
    resource.spec.instructions.clear();
    resource.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/evidence"),
        name: None,
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
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
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

fn command(spec: &sandbox::execution::ExecutionSpec) -> Option<Vec<String>> {
    match spec.program() {
        Program::Command { executable, args } => Some(
            std::iter::once(executable.as_str().to_owned())
                .chain(args.iter().cloned())
                .collect(),
        ),
        Program::ImageEntrypoint => None,
    }
}

/// Runs setup for an agent whose folder lists `repositories`, on an image whose workspace script is
/// `present`, returning the outcome and every command it ran.
async fn setup_with_repositories(
    repositories: Option<&str>,
    present: bool,
) -> (Result<(), agent::Error>, Rc<memory::Provider>, sandbox::SandboxHandle) {
    let directory = TempDir::new().expect("temporary directory");
    std::fs::write(directory.path().join("instructions.md"), "test instructions\n").expect("instruction file");
    if let Some(repositories) = repositories {
        std::fs::write(directory.path().join("repositories.yaml"), repositories).expect("repositories file");
    }
    let mut resource = support::agent("worker");
    resource.metadata.generation = 1;
    resource.spec.home = None;
    let record = AgentRecord {
        id: "7d1e2c3b-4a5f-4e6d-8c7b-9a0f1e2d3c4b".parse().expect("Agent ID"),
        source_directory: directory.path().to_path_buf(),
        manifest_path: None,
        env_file: None,
        agent: resource,
    };
    let backend = Rc::new(memory::Provider::new());
    backend.queue_execution_events_matching(is_claude_version, answers("2.1.266 (Claude Code)\n", 0));
    backend.queue_execution_events_matching(is_podman_presence_check, completed(1));
    if !present {
        backend.queue_execution_events_matching(
            |spec| {
                command(spec)
                    .is_some_and(|args| args == ["/usr/bin/test", "-x", "/usr/local/libexec/agent-workspace-init"])
            },
            completed(1),
        );
    }
    let service = SandboxService::new(backend.clone());
    let spec = record
        .agent
        .spec
        .sandbox
        .resolve_from(&record.source_directory, &Platform::native("linux").architecture);
    let sandbox = service
        .ensure(&EnsureSandboxRequest::new(
            record.sandbox_name().expect("Sandbox name"),
            spec,
        ))
        .await
        .expect("Sandbox");
    let outcome = Linux::new(PLATFORM_API_URL)
        .setup(&record, &sandbox, &declared(&record), &setup_phase())
        .await;
    (outcome, backend, sandbox)
}

const START_WORKSPACE_UNIT: [&str; 6] = [
    "/usr/bin/sudo",
    "-n",
    "/usr/bin/systemctl",
    "start",
    "--no-block",
    "agent-workspace-init.service",
];

/// The folder's repositories reach the base image's workspace unit, which is started on every pass
/// so a repository added later is cloned without recreating the agent.
#[tokio::test(flavor = "local")]
async fn linux_setup_hands_the_folders_repositories_to_the_workspace_unit() {
    let (outcome, backend, sandbox) = setup_with_repositories(
        Some("- repo: digdir/app\n- repo: digdir/microsandbox\n  path: forks/microsandbox\n  about: The fork.\n"),
        true,
    )
    .await;
    outcome.expect("setup");

    let list = read_file(&sandbox, "/etc/agent-workspace.d/agentctl.list").await;
    assert_eq!(list, b"digdir/app app\ndigdir/microsandbox forks/microsandbox\n");
    let commands: Vec<Vec<String>> = backend.execution_specs().iter().filter_map(command).collect();
    assert!(
        commands.iter().any(|args| args == &START_WORKSPACE_UNIT),
        "{commands:?}"
    );
    let instructions = String::from_utf8(read_file(&sandbox, "/home/agent/.claude/CLAUDE.md").await).expect("UTF-8");
    assert!(
        instructions.contains("`digdir/microsandbox` in `/home/agent/code/forks/microsandbox`: The fork."),
        "the agent is told what was cloned for it: {instructions}"
    );
}

#[tokio::test(flavor = "local")]
async fn linux_setup_without_repositories_starts_no_clone() {
    let (outcome, backend, _sandbox) = setup_with_repositories(None, true).await;
    outcome.expect("setup");

    let commands: Vec<Vec<String>> = backend.execution_specs().iter().filter_map(command).collect();
    assert!(
        !commands.iter().any(|args| args == &START_WORKSPACE_UNIT),
        "{commands:?}"
    );
    assert!(
        commands.iter().any(|args| args
            == &[
                "/usr/bin/sudo",
                "-n",
                "/bin/rm",
                "-f",
                "/etc/agent-workspace.d/agentctl.list"
            ]),
        "a list left from an earlier pass is removed: {commands:?}"
    );
}

#[tokio::test(flavor = "local")]
async fn linux_setup_refuses_repositories_an_image_cannot_clone() {
    let (outcome, _backend, _sandbox) = setup_with_repositories(Some("- repo: digdir/app\n"), false).await;
    let error = outcome.expect_err("an image without the workspace unit");
    assert!(error.to_string().contains("repositories.yaml"), "{error}");
}

#[tokio::test(flavor = "local")]
async fn linux_setup_rejects_an_invalid_repositories_file() {
    let (outcome, _backend, _sandbox) = setup_with_repositories(Some("- repo: ../app\n"), true).await;
    let error = outcome.expect_err("an invalid repository");
    assert!(error.to_string().contains("owner/name"), "{error}");
}
