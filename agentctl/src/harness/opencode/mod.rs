//! `OpenCode` (`@opencode/cli`, 2.x) harness adapter.
//!
//! The adapter knows nothing about providers, gateways or models: an Agent configures those
//! itself, through `OpenCode`'s own `~/.config/opencode/opencode.json` (its home overlay or image),
//! environment variables and `spec.secrets`. The platform contributes only what every `OpenCode`
//! user needs: the activity plugin, the manifest's instructions in `~/.config/opencode/AGENTS.md`,
//! skills and the launch.

use std::fmt::Write as _;

use crate::{
    Error,
    harness::{LaunchRequest, ModelSelection, ProcessLaunch, shell_single_quoted},
};

mod bootstrap;
mod plugin;
pub(super) mod transcript;

/// The configuration layer the launch hands `OpenCode`. It ranks above the Agent's global and
/// project configuration, which it adds to rather than replaces, so it only loads the activity
/// plugin and selects the Session's model.
const CONFIG_CONTENT_ENVIRONMENT: &str = "OPENCODE_CONFIG_CONTENT";

/// Where bootstrap installs what the platform adds to `OpenCode`, away from `~/.config/opencode/`,
/// which belongs to the Agent: only the instructions are written there, and `OpenCode` does not
/// discover anything here on its own. The launch loads the plugin.
fn platform_directory(home: &str) -> String {
    format!("{home}/.local/share/agent-platform/opencode")
}

/// Where bootstrap installs skills: the Agent Skills directory Codex and Pi install into, which
/// `OpenCode` also reads, so an Agent with several harnesses loads each skill once.
fn skills_directory(home: &str) -> String {
    format!("{home}/.agents/skills")
}

/// The activity plugin: `OpenCode` loads a plugin from a directory holding `index.ts`.
fn plugin_directory(home: &str) -> String {
    format!("{}/activity", platform_directory(home))
}

fn plugin_path(home: &str) -> String {
    format!("{}/index.ts", plugin_directory(home))
}

/// `OpenCode`'s global configuration directory, which the Agent owns.
fn configuration_directory(home: &str) -> String {
    format!("{home}/.config/opencode")
}

/// The manifest's instructions, in `OpenCode`'s global rules file, as `~/.codex/AGENTS.md` holds
/// them for Codex.
fn instructions_path(home: &str) -> String {
    format!("{}/AGENTS.md", configuration_directory(home))
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
    let output = super::version_output(sandbox, "opencode").await?;
    if !output.status.success() {
        let message = format!("`opencode --version` exited with code {}", output.status.code);
        // 126/127 mean the image does not provide the harness; retrying cannot change that.
        // Any other failure this early in the guest's life may be transient.
        return Err(if matches!(output.status.code, 126 | 127) {
            Error::Invalid(format!("OpenCode is missing: {message}"))
        } else {
            Error::SandboxSetup(message)
        });
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::SandboxSetup("`opencode --version` returned non-UTF-8 output".into()))?;
    let installed = installed_version(stdout)
        .ok_or_else(|| Error::SandboxSetup("`opencode --version` returned no version".into()))?;
    if let Some(expected) = expected_version.filter(|expected| *expected != installed) {
        return Err(Error::Invalid(format!(
            "declared OpenCode version {expected:?} does not match installed version {installed:?}"
        )));
    }
    Ok(())
}

/// The version in `opencode --version` output, which reads `opencode v2.0.32`.
fn installed_version(output: &str) -> Option<&str> {
    let last = output.split_whitespace().last()?;
    Some(last.strip_prefix('v').unwrap_or(last))
}

pub(super) fn manages_environment(name: &str) -> bool {
    name == CONFIG_CONTENT_ENVIRONMENT
}

/// `OpenCode` spells an effort as a variant of a model, `<provider>/<model>#<variant>`, so an
/// effort alone has nothing to attach to.
pub(super) fn validate_model_selection(selection: &ModelSelection) -> Result<(), Error> {
    match (&selection.model, &selection.effort) {
        (None, Some(effort)) => Err(Error::Invalid(format!(
            "OpenCode selects effort {:?} as a variant of a model, so it needs a model too: pass --model, \
             or set defaults.model on the Agent's opencode installation",
            effort.as_str()
        ))),
        _ => Ok(()),
    }
}

/// Recognizes `OpenCode`'s prompt before it reports anything: the activity plugin first hears of a
/// conversation when a prompt starts it. `OpenCode` hides the cursor while it starts. Once ready,
/// the cursor sits in its prompt box, whose left border is `┃`, and the pane title is `OpenCode`
/// on its home screen, or `OC | <session title>` in a resumed conversation.
pub(super) fn input_ready_without_report(cursor_line: &str, title: &str) -> bool {
    cursor_line.trim_start().starts_with('┃') && (title == "OpenCode" || title.starts_with("OC | "))
}

/// The environment defaults the platform gives `OpenCode`. Each applies only when the Session's
/// environment does not set the name, so an Agent's `spec.environment` decides: the launch
/// command spells them with `${NAME-default}`.
///
/// `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS` is left alone: `OpenCode` also reads `~/.claude/skills`,
/// where the Claude Code adapter installs the same skills, but it keeps one skill per name, the
/// `~/.agents/skills` one, and the switch would also hide a project's own `.claude/skills`.
const ENVIRONMENT_DEFAULTS: [(&str, &str); 2] = [
    // The image pins the version; an update would also change it behind the manifest's back.
    ("OPENCODE_DISABLE_AUTOUPDATE", "1"),
    // OpenCode runs on Bun, which honors this; the bundle holds the mediation CA.
    ("NODE_EXTRA_CA_CERTS", "/etc/ssl/certs/ca-certificates.crt"),
];

/// The configuration layer for one launch: the activity plugin, told the Session's selection so
/// it can refuse a model the Agent's configuration lacks, and that selection as `OpenCode`'s model.
fn config_content(home: &str, selection: &ModelSelection) -> String {
    let mut options = serde_json::Map::new();
    if let Some(model) = selection.model_str() {
        options.insert("model".into(), model.into());
    }
    if let Some(effort) = selection.effort_str() {
        options.insert("effort".into(), effort.into());
    }
    let mut content = serde_json::json!({
        "plugins": [{ "package": plugin_directory(home), "options": options }],
    });
    // Effort without a model is refused before a Session records it.
    if let Some(model) = selection.model_str() {
        let model = selection
            .effort_str()
            .map_or_else(|| model.to_owned(), |effort| format!("{model}#{effort}"));
        content["model"] = model.into();
    }
    content.to_string()
}

pub(super) fn launch_linux(request: &LaunchRequest<'_>) -> ProcessLaunch {
    let mut command = String::new();
    for (name, value) in ENVIRONMENT_DEFAULTS {
        let _infallible = write!(command, "export {name}=\"${{{name}-{value}}}\"; ");
    }
    let _infallible = write!(
        command,
        "export {CONFIG_CONTENT_ENVIRONMENT}={}; ",
        shell_single_quoted(&config_content(request.home, request.model_selection))
    );
    // `--standalone` runs OpenCode's server as this TUI's own child, with the Session's
    // environment and plugins; without it every TUI attaches to one shared per-user service that
    // keeps the environment of whichever launch started it. `--auto` approves what the
    // configuration does not deny; the Sandbox is the boundary, as it is for the other harnesses.
    let mut arguments = String::from("opencode --standalone --auto");
    match request.resume {
        // OpenCode opens the conversation with its history, and creates one by that ID when it
        // has none, as after a Sandbox that stopped before the first prompt was stored.
        Some(native) => {
            let _infallible = write!(arguments, " --session {}", shell_single_quoted(native));
        }
        // The initial prompt submits once the model is ready. It must be one `--prompt=` word: as
        // a separate argument, a prompt that begins with `-` is read as an option.
        None => {
            if let Some(message) = request.initial_prompt {
                let _infallible = write!(arguments, " {}", shell_single_quoted(&format!("--prompt={message}")));
            }
        }
    }
    let _infallible = write!(command, "exec {arguments} \"$@\"");
    ProcessLaunch {
        command,
        environment: Vec::new(),
    }
}

/// Launches run under `sh` with a stub `opencode`, in a temporary home standing in for the guest's.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::{collections::BTreeMap, process::Command};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::harness::{Effort, LaunchRequest, Model, ModelSelection, has_pair, test_launch_request};

    const NATIVE: &str = "ses_eb1b130e7ffeNYLUV9Id9zawbR";

    fn home() -> TempDir {
        TempDir::new().expect("temporary home")
    }

    fn selection(model: Option<&str>, effort: Option<&str>) -> ModelSelection {
        ModelSelection {
            model: model.map(|model| Model::new(model).expect("model")),
            effort: effort.map(|effort| Effort::new(effort).expect("effort")),
        }
    }

    /// The arguments `OpenCode` starts with when `request` launches it in `home`.
    fn launched(home: &TempDir, request: &LaunchRequest<'_>) -> Vec<String> {
        let home = home.path().to_str().expect("UTF-8 home");
        let launch = super::launch_linux(&LaunchRequest { home, ..*request });
        crate::harness::run_launch("opencode", &launch).expect("OpenCode started")
    }

    /// The environment `OpenCode` starts with when `request` launches it under `sh` with
    /// `inherited` set.
    fn launched_environment(request: &LaunchRequest<'_>, inherited: &[(&str, &str)]) -> BTreeMap<String, String> {
        let launch = super::launch_linux(request);
        let directory = TempDir::new().expect("temporary directory");
        let stub = directory.path().join("opencode");
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

    /// The configuration layer `OpenCode` starts with when `request` launches it.
    fn config_content(request: &LaunchRequest<'_>) -> Value {
        let environment = launched_environment(request, &[]);
        let content = environment.get("OPENCODE_CONFIG_CONTENT").expect("configuration layer");
        serde_json::from_str(content).expect("JSON configuration layer")
    }

    #[test]
    fn every_launch_runs_its_own_server_and_approves_what_the_configuration_allows() {
        let arguments = launched(&home(), &test_launch_request(None, None));

        assert_eq!(arguments, ["--standalone", "--auto"]);
    }

    #[test]
    fn the_configuration_layer_loads_the_activity_plugin_from_the_platform_directory() {
        let content = config_content(&test_launch_request(None, None));

        assert_eq!(
            content,
            json!({"plugins": [{
                "package": "/home/agent/.local/share/agent-platform/opencode/activity",
                "options": {},
            }]})
        );
    }

    #[test]
    fn bootstrap_writes_only_the_instructions_among_the_agents_opencode_files() {
        let home = "/home/agent";
        assert_eq!(super::instructions_path(home), "/home/agent/.config/opencode/AGENTS.md");
        assert_eq!(
            super::plugin_path(home),
            "/home/agent/.local/share/agent-platform/opencode/activity/index.ts"
        );
        for path in [
            super::platform_directory(home),
            super::plugin_directory(home),
            super::plugin_path(home),
            super::skills_directory(home),
        ] {
            assert!(!path.contains("/.config/opencode"), "{path}");
        }
    }

    #[test]
    fn a_selected_model_and_effort_become_the_model_variant_and_the_plugin_options() {
        let selected = selection(Some("aivar/sigma2:Qwen3.8-27B"), Some("high"));
        let content = config_content(&LaunchRequest {
            model_selection: &selected,
            ..test_launch_request(None, None)
        });

        assert_eq!(content["model"], "aivar/sigma2:Qwen3.8-27B#high");
        assert_eq!(
            content["plugins"][0]["options"],
            json!({"model": "aivar/sigma2:Qwen3.8-27B", "effort": "high"})
        );

        let model_only = selection(Some("aivar/aivar:borealis"), None);
        let content = config_content(&LaunchRequest {
            model_selection: &model_only,
            ..test_launch_request(None, None)
        });
        assert_eq!(content["model"], "aivar/aivar:borealis");
        assert_eq!(
            content["plugins"][0]["options"],
            json!({"model": "aivar/aivar:borealis"})
        );
    }

    #[test]
    fn launch_environment_defaults_apply_only_when_the_agent_leaves_the_names_unset() {
        let request = test_launch_request(None, None);
        let defaults = launched_environment(&request, &[]);
        assert_eq!(
            defaults.get("OPENCODE_DISABLE_AUTOUPDATE").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            defaults.get("NODE_EXTRA_CA_CERTS").map(String::as_str),
            Some("/etc/ssl/certs/ca-certificates.crt")
        );
        // The Agent's database and configuration are OpenCode's own defaults; the platform never
        // moves them.
        // Nor does it hide skills, which would also hide a project's own `.claude/skills`.
        for name in [
            "OPENCODE_DB",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "XDG_DATA_HOME",
            "OPENCODE_DISABLE_CLAUDE_CODE",
            "OPENCODE_DISABLE_CLAUDE_CODE_SKILLS",
        ] {
            assert!(!defaults.contains_key(name), "{name}");
        }

        let overridden = launched_environment(
            &request,
            &[
                ("OPENCODE_DISABLE_AUTOUPDATE", ""),
                ("NODE_EXTRA_CA_CERTS", "/etc/own-ca.pem"),
            ],
        );
        assert_eq!(
            overridden.get("OPENCODE_DISABLE_AUTOUPDATE").map(String::as_str),
            Some("")
        );
        assert_eq!(
            overridden.get("NODE_EXTRA_CA_CERTS").map(String::as_str),
            Some("/etc/own-ca.pem")
        );
    }

    #[test]
    fn the_platform_owns_only_the_configuration_layer_among_opencode_environment_names() {
        assert!(crate::harness::manages_environment(
            crate::Harness::OpenCode,
            "OPENCODE_CONFIG_CONTENT"
        ));
        // A secret by that name would be overwritten by the launch too.
        assert!(crate::harness::conflicts_with_managed_secret(
            crate::Harness::OpenCode,
            "OPENCODE_CONFIG_CONTENT",
            None
        ));
        for name in [
            "OPENCODE_DB",
            "OPENCODE_CONFIG",
            "OPENCODE_DISABLE_AUTOUPDATE",
            "OPENCODE_DISABLE_CLAUDE_CODE_SKILLS",
            "NODE_EXTRA_CA_CERTS",
            "MY_GATEWAY_KEY",
        ] {
            assert!(
                !crate::harness::manages_environment(crate::Harness::OpenCode, name),
                "{name}"
            );
            assert!(
                !crate::harness::conflicts_with_managed_secret(crate::Harness::OpenCode, name, None),
                "{name}"
            );
        }
    }

    #[test]
    fn an_inherited_configuration_layer_is_replaced() {
        let environment = launched_environment(
            &test_launch_request(None, None),
            &[("OPENCODE_CONFIG_CONTENT", r#"{"model":"elsewhere/model"}"#)],
        );
        let content: Value =
            serde_json::from_str(environment.get("OPENCODE_CONFIG_CONTENT").expect("layer")).expect("JSON");
        assert!(content.get("model").is_none(), "{content}");
    }

    #[test]
    fn resume_opens_the_reported_conversation_and_does_not_prompt_again() {
        let resumed = launched(&home(), &test_launch_request(Some(NATIVE), Some("ignored")));

        assert!(has_pair(&resumed, "--session", NATIVE), "{resumed:?}");
        assert!(
            resumed.iter().all(|argument| !argument.starts_with("--prompt")),
            "{resumed:?}"
        );
    }

    #[test]
    fn a_fresh_launch_passes_the_first_prompt_as_one_argument() {
        for prompt in [
            "fix it's\nbroken",
            "-n starts like an option",
            "@foo check this",
            "\"quoted\" $HOME `date`",
        ] {
            let arguments = launched(&home(), &test_launch_request(None, Some(prompt)));

            assert_eq!(arguments.last(), Some(&format!("--prompt={prompt}")), "{arguments:?}");
            assert!(!arguments.contains(&"--session".to_owned()));
        }
    }

    #[test]
    fn a_selection_applies_to_fresh_and_resumed_conversations() {
        let selected = selection(Some("provider/some-model:1"), Some("high"));
        for resume in [None, Some(NATIVE)] {
            let content = config_content(&LaunchRequest {
                model_selection: &selected,
                ..test_launch_request(resume, Some("go"))
            });
            assert_eq!(content["model"], "provider/some-model:1#high", "{resume:?}");
        }
    }

    #[test]
    fn an_effort_needs_a_model_to_be_its_variant() {
        super::validate_model_selection(&selection(None, None)).expect("no selection");
        super::validate_model_selection(&selection(Some("p/m"), None)).expect("model alone");
        super::validate_model_selection(&selection(Some("p/m"), Some("high"))).expect("model and effort");
        let error = super::validate_model_selection(&selection(None, Some("high"))).expect_err("effort alone");
        assert!(error.to_string().contains("--model"), "{error}");
        assert!(error.to_string().contains("defaults.model"), "{error}");
        // Other harnesses take an effort on its own.
        for harness in [crate::Harness::ClaudeCode, crate::Harness::Codex, crate::Harness::Pi] {
            crate::harness::validate_model_selection(harness, &selection(None, Some("high"))).expect("effort");
        }
    }

    #[test]
    fn the_version_is_the_last_word_without_its_v() {
        assert_eq!(super::installed_version("opencode v2.0.32\n"), Some("2.0.32"));
        assert_eq!(super::installed_version("2.0.32"), Some("2.0.32"));
        assert_eq!(super::installed_version("  \n"), None);
    }

    #[test]
    fn the_prompt_is_ready_once_the_cursor_is_in_the_prompt_box_of_a_titled_pane() {
        let input = "  ┃  ";
        assert!(super::input_ready_without_report(input, "OpenCode"));
        assert!(super::input_ready_without_report(
            input,
            "OC | Simple confirmation request"
        ));
        // Starting: the title is not OpenCode's yet.
        assert!(!super::input_ready_without_report(input, "opencode"));
        assert!(!super::input_ready_without_report(input, "sh"));
        // Not the prompt box.
        assert!(!super::input_ready_without_report("  Build auto · model", "OpenCode"));
        assert!(!super::input_ready_without_report("", "OpenCode"));
    }
}
