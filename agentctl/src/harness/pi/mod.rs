//! Pi (`@earendil-works/pi-coding-agent`) harness adapter.
//!
//! The adapter knows nothing about providers, gateways or models: an Agent configures those
//! itself, through Pi's own files under `~/.pi/agent/` (its home overlay or image), environment
//! variables and `spec.secrets`. The platform contributes only what every Pi user needs: the
//! activity extension, the manifest's instructions in `~/.pi/agent/AGENTS.md`, skills and the launch.

use std::fmt::Write as _;

use crate::{
    Error,
    harness::{LaunchRequest, ProcessLaunch, shell_single_quoted},
};

mod bootstrap;
mod extension;
pub(super) mod transcript;

/// Where bootstrap installs what the platform adds to Pi, away from `~/.pi/agent/`, which
/// belongs to the Agent: nothing there is written, and Pi does not discover anything here on
/// its own. The launch loads the extension.
fn platform_directory(home: &str) -> String {
    format!("{home}/.local/share/agent-platform/pi")
}

/// Where bootstrap installs skills: the Agent Skills directory Codex installs into, which Pi also
/// reads, so an Agent with both harnesses loads each skill once.
fn skills_directory(home: &str) -> String {
    format!("{home}/.agents/skills")
}

/// The activity extension, loaded with `-e`.
fn extension_path(home: &str) -> String {
    format!("{}/activity.ts", platform_directory(home))
}

/// The manifest's instructions, in Pi's global context file, as `~/.codex/AGENTS.md` holds them
/// for Codex.
fn instructions_path(home: &str) -> String {
    format!("{home}/.pi/agent/AGENTS.md")
}

pub(super) async fn bootstrap_linux(
    sandbox: &sandbox::SandboxHandle,
    home: &str,
    instructions: Option<&[u8]>,
    skills: &[crate::harness::Skill],
) -> Result<(), Error> {
    bootstrap::configure_linux(sandbox, home, instructions, skills).await
}

pub(super) async fn verify_linux(
    sandbox: &sandbox::SandboxHandle,
    expected_version: Option<&str>,
) -> Result<(), Error> {
    let output = super::version_output(sandbox, "pi").await?;
    if !output.status.success() {
        let message = format!("`pi --version` exited with code {}", output.status.code);
        // 126/127 mean the image does not provide the harness; retrying cannot change that.
        // Any other failure this early in the guest's life may be transient.
        return Err(if matches!(output.status.code, 126 | 127) {
            Error::Invalid(format!("Pi is missing: {message}"))
        } else {
            Error::SandboxSetup(message)
        });
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::SandboxSetup("`pi --version` returned non-UTF-8 output".into()))?;
    let installed = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::SandboxSetup("`pi --version` returned no version".into()))?;
    if let Some(expected) = expected_version.filter(|expected| *expected != installed) {
        return Err(Error::Invalid(format!(
            "declared Pi version {expected:?} does not match installed version {installed:?}"
        )));
    }
    Ok(())
}

/// The environment defaults the platform gives Pi. Each applies only when the Session's
/// environment does not set the name, so an Agent's `spec.environment` decides: the launch
/// command spells them with `${NAME-default}`.
const ENVIRONMENT_DEFAULTS: [(&str, &str); 3] = [
    // Pi runs on Node, which ignores the system trust store; this bundle holds the mediation CA.
    ("NODE_EXTRA_CA_CERTS", "/etc/ssl/certs/ca-certificates.crt"),
    ("PI_TELEMETRY", "0"),
    ("PI_SKIP_VERSION_CHECK", "1"),
];

pub(super) fn launch_linux(request: &LaunchRequest<'_>) -> ProcessLaunch {
    let mut command = String::new();
    for (name, value) in ENVIRONMENT_DEFAULTS {
        let _infallible = write!(command, "export {name}=\"${{{name}-{value}}}\"; ");
    }
    // `--approve` trusts the working directory's own `.pi` resources without a dialog that
    // nothing could answer; the Sandbox is the boundary, as it is for the other harnesses.
    // `--tui-mode regular` keeps the conversation in tmux's scrollback instead of an
    // alternate screen.
    let mut base = format!(
        "pi --approve --tui-mode regular -e {}",
        shell_single_quoted(&extension_path(request.home))
    );
    // Both are opaque here: Pi takes `<provider>/<id>` and its own thinking levels, which the
    // Agent's configuration defines. They apply to fresh and resumed conversations alike.
    if let Some(model) = &request.model_selection.model {
        let _infallible = write!(base, " --model {}", shell_single_quoted(model.as_str()));
    }
    if let Some(effort) = &request.model_selection.effort {
        let _infallible = write!(base, " --thinking {}", shell_single_quoted(effort.as_str()));
    }
    // A fresh conversation may start on a positional prompt; `--` keeps a prompt that begins
    // with `-` from being read as an option. Pi reads a positional argument that begins with `@`
    // as a file to include, even after `--`, and exits when there is none, so such a prompt
    // gets a leading space.
    let fresh = request.initial_prompt.map_or_else(
        || base.clone(),
        |message| {
            let guard = if message.starts_with('@') { " " } else { "" };
            format!("{base} -- {}", shell_single_quoted(&format!("{guard}{message}")))
        },
    );
    // The transcript the extension reported is the authority for resumability, wherever the
    // Agent's Pi configuration (`sessionDir`, `PI_CODING_AGENT_DIR`, ...) keeps its sessions:
    // SessionStart reports a conversation before Pi writes it, so an untouched Session can still
    // wake from Idle as a fresh one. Pi cannot open an empty file, which a Sandbox that stopped
    // abruptly leaves behind, so that case starts fresh too. `--session` takes the path.
    match request.resume.and(request.resume_transcript) {
        Some(transcript) => {
            let path = shell_single_quoted(transcript);
            let _infallible = write!(
                command,
                "if [ -s {path} ]; then exec {base} --session {path}; else exec {fresh}; fi"
            );
        }
        None => {
            let _infallible = write!(command, "exec {fresh}");
        }
    }
    ProcessLaunch {
        command,
        environment: Vec::new(),
    }
}

/// Launches run under `sh` with a stub `pi`, in a temporary home standing in for the guest's.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::{collections::BTreeMap, process::Command};

    use tempfile::TempDir;

    use crate::harness::{Effort, LaunchRequest, Model, ModelSelection, has_pair, test_launch_request};

    const NATIVE: &str = "01a149c1-7423-76be-a455-8368e91d25b7";

    fn home() -> TempDir {
        TempDir::new().expect("temporary home")
    }

    /// A transcript file with `content`, in a session directory the Agent's own Pi configuration
    /// could have put anywhere.
    fn transcript(content: &str) -> (TempDir, String) {
        let directory = TempDir::new().expect("session directory");
        let path = directory
            .path()
            .join(format!("2026-10-17T12-06-17-379Z_{NATIVE}.jsonl"));
        std::fs::write(&path, content).expect("session");
        let path = path.to_str().expect("UTF-8 path").to_owned();
        (directory, path)
    }

    /// The arguments Pi starts with when `request` launches it in `home`.
    fn launched(home: &TempDir, request: &LaunchRequest<'_>) -> Vec<String> {
        let home = home.path().to_str().expect("UTF-8 home");
        let launch = super::launch_linux(&LaunchRequest { home, ..*request });
        crate::harness::run_launch("pi", &launch).expect("Pi started")
    }

    /// The environment Pi starts with when `command` runs under `sh` with `inherited` set.
    fn launched_environment(inherited: &[(&str, &str)]) -> BTreeMap<String, String> {
        let launch = super::launch_linux(&test_launch_request(None, None));
        let directory = TempDir::new().expect("temporary directory");
        let stub = directory.path().join("pi");
        std::fs::write(&stub, "#!/bin/sh\nenv\n").expect("stub");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("permissions");
        }
        let path = std::env::var("PATH").unwrap_or_default();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&launch.command)
            .env_clear()
            .env("PATH", format!("{}:{path}", directory.path().display()));
        command.envs(inherited.iter().copied());
        let output = loop {
            // A stub another test thread is still writing can be briefly busy.
            let output = command.output().expect("sh");
            if !String::from_utf8_lossy(&output.stderr).contains("busy") {
                break output;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout)
            .expect("UTF-8")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn every_launch_trusts_the_project_and_loads_the_activity_extension_from_the_platform_directory() {
        let home = home();
        let arguments = launched(&home, &test_launch_request(None, None));

        assert_eq!(arguments.first().map(String::as_str), Some("--approve"));
        assert!(has_pair(&arguments, "--tui-mode", "regular"), "{arguments:?}");
        let extension = home.path().join(".local/share/agent-platform/pi/activity.ts");
        assert!(has_pair(&arguments, "-e", extension.to_str().expect("UTF-8")));
        // Nothing the launch names lives where Pi or the Agent keeps its own files.
        assert!(
            arguments.iter().all(|argument| !argument.contains(".pi/agent")),
            "{arguments:?}"
        );
    }

    #[test]
    fn bootstrap_writes_only_the_instructions_among_the_agents_pi_files() {
        let home = "/home/agent";
        assert_eq!(super::instructions_path(home), "/home/agent/.pi/agent/AGENTS.md");
        for path in [
            super::platform_directory(home),
            super::extension_path(home),
            super::skills_directory(home),
        ] {
            assert!(!path.contains("/.pi"), "{path}");
        }
    }

    #[test]
    fn the_launch_leaves_the_system_prompt_to_pi_and_the_agent() {
        // `--append-system-prompt` would make Pi skip the Agent's own `APPEND_SYSTEM.md`.
        let arguments = launched(&home(), &test_launch_request(None, None));
        assert!(
            arguments.iter().all(|argument| !argument.contains("system-prompt")),
            "{arguments:?}"
        );
    }

    #[test]
    fn launch_environment_defaults_apply_only_when_the_agent_leaves_the_names_unset() {
        let defaults = launched_environment(&[]);
        assert_eq!(
            defaults.get("NODE_EXTRA_CA_CERTS").map(String::as_str),
            Some("/etc/ssl/certs/ca-certificates.crt")
        );
        assert_eq!(defaults.get("PI_TELEMETRY").map(String::as_str), Some("0"));
        assert_eq!(defaults.get("PI_SKIP_VERSION_CHECK").map(String::as_str), Some("1"));
        // The Agent's directory is Pi's own default; the platform never moves it, and it does not
        // switch Pi offline, which would also stop its model catalog refresh.
        assert!(!defaults.contains_key("PI_CODING_AGENT_DIR"));
        assert!(!defaults.contains_key("PI_OFFLINE"));

        let overridden = launched_environment(&[
            ("PI_TELEMETRY", "1"),
            ("NODE_EXTRA_CA_CERTS", "/etc/own-ca.pem"),
            ("PI_SKIP_VERSION_CHECK", ""),
        ]);
        assert_eq!(overridden.get("PI_TELEMETRY").map(String::as_str), Some("1"));
        assert_eq!(
            overridden.get("NODE_EXTRA_CA_CERTS").map(String::as_str),
            Some("/etc/own-ca.pem")
        );
        assert_eq!(overridden.get("PI_SKIP_VERSION_CHECK").map(String::as_str), Some(""));
    }

    #[test]
    fn a_manifest_may_set_any_pi_environment_name() {
        for name in [
            "PI_OFFLINE",
            "PI_CODING_AGENT_DIR",
            "PI_TELEMETRY",
            "NODE_EXTRA_CA_CERTS",
        ] {
            assert!(!crate::harness::manages_environment(crate::Harness::Pi, name), "{name}");
        }
        assert!(!crate::harness::conflicts_with_managed_secret(
            crate::Harness::Pi,
            "MY_GATEWAY_KEY",
            None
        ));
    }

    #[test]
    fn resume_opens_the_reported_transcript_wherever_it_lives() {
        let (_directory, path) = transcript("{}\n");
        let resumed = launched(
            &home(),
            &LaunchRequest {
                resume_transcript: Some(&path),
                ..test_launch_request(Some(NATIVE), None)
            },
        );
        assert!(has_pair(&resumed, "--session", &path), "{resumed:?}");
    }

    #[test]
    fn resume_starts_fresh_without_a_usable_transcript() {
        let (_empty_directory, empty) = transcript("");
        for (resume, path) in [
            (Some(NATIVE), None),
            (Some(NATIVE), Some("/nonexistent/session.jsonl")),
            (Some(NATIVE), Some(empty.as_str())),
            // A transcript only matters for a conversation being resumed.
            (None, Some(empty.as_str())),
        ] {
            let fresh = launched(
                &home(),
                &LaunchRequest {
                    resume_transcript: path,
                    ..test_launch_request(resume, None)
                },
            );
            assert!(
                !fresh.contains(&"--session".to_owned()),
                "{resume:?} {path:?}: {fresh:?}"
            );
        }
    }

    #[test]
    fn a_transcript_path_with_quotes_and_spaces_reaches_pi_intact() {
        let directory = TempDir::new().expect("directory");
        let nested = directory.path().join("it's a dir");
        std::fs::create_dir(&nested).expect("nested");
        let path = nested.join("s.jsonl");
        std::fs::write(&path, "{}\n").expect("session");
        let path = path.to_str().expect("UTF-8");
        let resumed = launched(
            &home(),
            &LaunchRequest {
                resume_transcript: Some(path),
                ..test_launch_request(Some(NATIVE), None)
            },
        );
        assert!(has_pair(&resumed, "--session", path), "{resumed:?}");
    }

    #[test]
    fn a_fresh_launch_passes_the_first_prompt_as_one_argument() {
        // `--` keeps a prompt that starts with `-` positional.
        let arguments = launched(&home(), &test_launch_request(None, Some("fix it's\nbroken")));

        assert!(
            arguments.ends_with(&["--".to_owned(), "fix it's\nbroken".to_owned()]),
            "{arguments:?}"
        );
        assert!(!arguments.contains(&"--session".to_owned()));
    }

    #[test]
    fn a_first_prompt_starting_with_an_at_sign_is_not_a_file_reference() {
        // Pi opens a positional `@name` as a file even after `--`, and exits when it is missing.
        let arguments = launched(&home(), &test_launch_request(None, Some("@foo check this")));

        let prompt = arguments.last().expect("prompt");
        assert!(!prompt.starts_with('@'), "{prompt:?}");
        assert_eq!(prompt.trim_start(), "@foo check this");
    }

    #[test]
    fn launches_select_no_model_or_thinking_unless_the_session_carries_them() {
        let arguments = launched(&home(), &test_launch_request(None, None));

        assert!(!arguments.contains(&"--model".to_owned()), "{arguments:?}");
        assert!(!arguments.contains(&"--thinking".to_owned()), "{arguments:?}");
    }

    #[test]
    fn model_and_effort_apply_to_fresh_and_resumed_conversations() {
        let selection = ModelSelection {
            model: Some(Model::new("provider/some-model:1").expect("model")),
            effort: Some(Effort::new("high").expect("effort")),
        };
        let selected = LaunchRequest {
            model_selection: &selection,
            ..test_launch_request(Some(NATIVE), Some("go"))
        };
        let (_directory, path) = transcript("{}\n");
        let selected = LaunchRequest {
            resume_transcript: Some(&path),
            ..selected
        };

        let resumed = launched(&home(), &selected);
        let fresh = launched(
            &home(),
            &LaunchRequest {
                resume: None,
                resume_transcript: None,
                ..selected
            },
        );
        for arguments in [&resumed, &fresh] {
            assert!(has_pair(arguments, "--model", "provider/some-model:1"), "{arguments:?}");
            assert!(has_pair(arguments, "--thinking", "high"), "{arguments:?}");
        }
        assert!(has_pair(&resumed, "--session", &path), "{resumed:?}");
        assert!(fresh.ends_with(&["--".to_owned(), "go".to_owned()]), "{fresh:?}");
    }
}
