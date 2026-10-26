//! Claude Code harness adapter.

use std::fmt::Write as _;

use crate::{
    Error,
    harness::{LaunchRequest, MediatedSecret, ProcessLaunch, SignIn, shell_single_quoted},
    persistence,
};
use sandbox::secret_store::SecretReference;

pub(super) mod authentication;
mod bootstrap;
mod hooks;
mod status_line;
pub(super) mod transcript;

const PROVIDER: &str = "claude";
/// Claude Code keeps its settings and skills in `~/.claude`, and reads a repository's from its `.claude`.
pub(super) const FOOTPRINT: super::Footprint = super::Footprint {
    executable: "claude",
    directory: ".claude",
    skills_directory: ".claude/skills",
};
const ACCESS_SECRET: &str = "claude-access-token";
pub(super) const ACCESS_ENVIRONMENT: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const ACCESS_PLACEHOLDER: &str = "sk-ant-oat01-agent-mediated-placeholder-not-a-real-credential";
/// Second binding on the same credential, under a name the harness does not
/// scrub. Claude Code removes `CLAUDE_CODE_OAUTH_TOKEN` from every process it
/// spawns, so a Session cannot read its own placeholder to hand to a nested
/// `agentd`; this name survives, as the Codex one already does.
const NESTED_ENVIRONMENT: &str = "AGENT_CLAUDE_ACCESS_TOKEN";
/// A binding needs its own placeholder, and one placeholder may not contain
/// another, so this is not a spelling of `ACCESS_PLACEHOLDER`. A nested Agent
/// therefore sends this value outward and the outer mediation resolves it to
/// the same stored credential.
const NESTED_PLACEHOLDER: &str = "sk-ant-oat01-agent-mediated-nested-placeholder-not-a-real-credential";
const API_HOST: &str = "api.anthropic.com";
/// Fullscreen Claude owns an alternate-screen viewport whose redraws can corrupt under tmux;
/// normal-screen output remains stable and gives tmux durable scrollback.
const DISABLE_ALTERNATE_SCREEN_ENVIRONMENT: &str = "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN";
/// The model recorded for Sessions that predate recorded selections. The adapter
/// launched every Session on this alias from preview 2 until selections arrived,
/// because the mediated setup token cannot enumerate models and Fable never
/// appeared in the `/model` picker. Preview 1 Sessions ran on Claude Code's own
/// default; recording the alias for them too keeps every earlier conversation on
/// one known model instead of whatever the harness defaults to next. Manifests
/// now declare the default for new Sessions.
pub(super) const MODEL_LAUNCHED_BEFORE_SELECTION: &str = "fable";

/// Claude Code's user instruction file, which holds the Agent's instructions.
pub(super) fn instructions_path(home: &str) -> String {
    format!("{home}/.claude/CLAUDE.md")
}

pub(super) async fn authentication_ready(database: &persistence::Database) -> Result<bool, Error> {
    authentication::is_ready(database).await
}

pub(super) async fn prepare(database: &persistence::Database) -> Result<Vec<MediatedSecret>, Error> {
    if !authentication::is_ready(database).await? {
        return Err(Error::Invalid(
            "Claude Code authentication is not ready; run `agentctl claude login`".into(),
        ));
    }
    Ok(vec![
        MediatedSecret {
            environment: ACCESS_ENVIRONMENT,
            placeholder: ACCESS_PLACEHOLDER.into(),
            reference: SecretReference::from_opaque(ACCESS_SECRET),
            allowed_hosts: vec![authentication::mediated_host().into()],
        },
        MediatedSecret {
            environment: NESTED_ENVIRONMENT,
            placeholder: NESTED_PLACEHOLDER.into(),
            reference: SecretReference::from_opaque(ACCESS_SECRET),
            allowed_hosts: vec![authentication::mediated_host().into()],
        },
    ])
}

/// The nested placeholder a Session in an Agent with Claude Code holds.
pub(super) fn agent_credential(
    environment: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<zeroize::Zeroizing<String>, Error> {
    let token = environment(NESTED_ENVIRONMENT)
        .and_then(|token| token.into_string().ok())
        .map(|token| zeroize::Zeroizing::new(token.trim().to_owned()))
        .filter(|token| !token.is_empty());
    token.ok_or_else(|| {
        super::no_agent_login(
            super::Harness::ClaudeCode,
            &format!("`{NESTED_ENVIRONMENT}` is not set here"),
        )
    })
}

pub(super) fn conflicts_with_managed_secret(name: &str, placeholder: Option<&str>) -> bool {
    matches!(name, ACCESS_ENVIRONMENT | NESTED_ENVIRONMENT)
        || matches!(placeholder, Some(ACCESS_PLACEHOLDER | NESTED_PLACEHOLDER))
}

pub(super) fn manages_environment(name: &str) -> bool {
    matches!(
        name,
        ACCESS_ENVIRONMENT
            | NESTED_ENVIRONMENT
            | "CLAUDE_CONFIG_DIR"
            | DISABLE_ALTERNATE_SCREEN_ENVIRONMENT
            | "DISABLE_AUTOUPDATER"
    )
}

/// Long-lived Claude setup tokens carry this prefix.
const SETUP_TOKEN_PREFIX: &str = "sk-ant-oat";
/// How long a browser sign-in may take, including signing in to the account.
/// Its hidden interface takes Ctrl+C as a keypress, so this is what ends a
/// sign-in whose browser never opened or never reached the localhost callback.
const BROWSER_SIGN_IN_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(5);
/// A pasted token wraps over a few lines at most; anything longer is not one.
const PASTED_TOKEN_LINES: usize = 8;

/// Mints a long-lived Claude token on the host with `claude setup-token`.
///
/// With a browser here, sign-in completes through the ephemeral localhost
/// callback it starts, so nothing needs typing: its interface is hidden and
/// the token read from its output. Otherwise its interface keeps the
/// terminal, the only place it redraws in place, to show the sign-in link and
/// code prompt, and the person pastes the token it prints.
///
/// # Errors
///
/// Returns an error when the harness CLI is missing, fails, times out, or
/// yields no token.
pub(super) fn acquire_host_token(sign_in: SignIn) -> Result<zeroize::Zeroizing<String>, Error> {
    use std::io::Write as _;

    eprintln!("Minting a long-lived Claude token with `claude setup-token`.");
    let mut command = std::process::Command::new("claude");
    // The token must stay in the terminal's scrollback after it exits, for the
    // person to copy.
    command
        .arg("setup-token")
        .env(DISABLE_ALTERNATE_SCREEN_ENVIRONMENT, "1");
    match sign_in {
        SignIn::Browser(opener) => {
            if let Some(opener) = opener {
                let (name, value) = opener.environment;
                command.env("BROWSER", opener.program).env(name, value);
            }
            eprintln!("A browser window will open — approve the request, then return here.");
            eprintln!(
                "If none opens, this gives up after {} minutes; then run again with AGENTCTL_OPEN=copy.",
                BROWSER_SIGN_IN_TIMEOUT.as_secs() / 60
            );
            let output = run_hidden(&mut command)?;
            find_setup_token(&output)
                .map(|token| zeroize::Zeroizing::new(token.to_owned()))
                .ok_or_else(|| {
                    Error::Invalid(
                        "`claude setup-token` printed no token; run again with AGENTCTL_OPEN=copy to sign in by hand"
                            .into(),
                    )
                })
        }
        SignIn::Elsewhere { reason } => {
            eprintln!("No browser here would reach you: {reason}.");
            eprintln!("Open the sign-in link it shows in any browser, then paste the token it prints.");
            completed(command.status().map_err(|error| not_started(&error))?)?;
            eprint!("Paste the Claude token shown above, then press Enter on an empty line: ");
            std::io::stderr().flush().ok();
            read_pasted_token(&mut std::io::stdin().lock())
        }
    }
}

/// Runs the sign-in with its interface hidden and its output captured, until
/// it exits or [`BROWSER_SIGN_IN_TIMEOUT`] passes.
fn run_hidden(command: &mut std::process::Command) -> Result<zeroize::Zeroizing<Vec<u8>>, Error> {
    // The sign-in switches the terminal to raw mode, and once killed cannot
    // switch it back. Switching first records the person's mode, which
    // disabling then restores whatever the sign-in left behind.
    let restore = crossterm::terminal::enable_raw_mode().is_ok();
    let captured = capture(command);
    if restore {
        crossterm::terminal::disable_raw_mode().ok();
    }
    captured
}

fn capture(command: &mut std::process::Command) -> Result<zeroize::Zeroizing<Vec<u8>>, Error> {
    use std::{io::Read as _, process::Stdio};

    let mut child = command
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| not_started(&error))?;
    let Some(mut stdout) = child.stdout.take() else {
        child.kill().ok();
        return Err(Error::Invalid("`claude setup-token` has no output to read".into()));
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    // Reading ends when the sign-in exits and closes its output.
    std::thread::spawn(move || {
        let mut output = zeroize::Zeroizing::new(Vec::new());
        sender.send(stdout.read_to_end(&mut output).map(|_| output)).ok();
    });
    let output = receiver.recv_timeout(BROWSER_SIGN_IN_TIMEOUT).map_or_else(
        |_| {
            child.kill().ok();
            Err(Error::Invalid(format!(
                "no browser sign-in completed within {} minutes; run again with AGENTCTL_OPEN=copy to sign in by hand",
                BROWSER_SIGN_IN_TIMEOUT.as_secs() / 60
            )))
        },
        |read| read.map_err(|error| Error::Invalid(format!("could not read `claude setup-token` output: {error}"))),
    );
    let status = child.wait().map_err(|error| not_started(&error))?;
    let output = output?;
    completed(status)?;
    Ok(output)
}

fn not_started(error: &std::io::Error) -> Error {
    Error::Invalid(format!(
        "could not run `claude setup-token` (is Claude Code installed on this host?): {error}"
    ))
}

fn completed(status: std::process::ExitStatus) -> Result<(), Error> {
    if status.success() {
        Ok(())
    } else {
        Err(Error::Invalid("`claude setup-token` did not complete".into()))
    }
}

const fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// Finds the last setup token in terminal output, which interleaves it with
/// ANSI escape sequences rather than only whitespace. Hidden output repeats
/// every frame, including whatever was typed into its prompt, so the token it
/// printed last is the one it minted.
fn find_setup_token(output: &[u8]) -> Option<&str> {
    let prefix = SETUP_TOKEN_PREFIX.as_bytes();
    let start = output.windows(prefix.len()).rposition(|window| window == prefix)?;
    let rest = &output[start..];
    let end = rest.iter().position(|&byte| !token_byte(byte)).unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok()
}

/// Reads a pasted token through the empty line that ends it. The interface
/// wraps a long token at the terminal's width, so it may arrive over several
/// lines; reading them all also keeps its tail from reaching the shell once
/// agentctl exits.
fn read_pasted_token(input: &mut impl std::io::BufRead) -> Result<zeroize::Zeroizing<String>, Error> {
    let mut token = zeroize::Zeroizing::new(String::new());
    let mut line = zeroize::Zeroizing::new(String::new());
    for _ in 0..PASTED_TOKEN_LINES {
        line.clear();
        let read = input
            .read_line(&mut line)
            .map_err(|error| Error::Invalid(format!("could not read the pasted token: {error}")))?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        token.extend(line.chars().filter(|character| !character.is_whitespace()));
    }
    if token.is_empty() {
        return Err(Error::Invalid("no Claude token was provided".into()));
    }
    if !token.starts_with(SETUP_TOKEN_PREFIX) || !token.bytes().all(token_byte) {
        return Err(Error::Invalid(
            "the pasted text is not a Claude setup token; paste the token `claude setup-token` printed".into(),
        ));
    }
    Ok(token)
}

pub(super) async fn bootstrap_linux(
    sandbox: &sandbox::SandboxHandle,
    home: &str,
    instructions: &[u8],
    skills: &[crate::harness::Skill],
) -> Result<(), Error> {
    bootstrap::configure_linux(sandbox, home, instructions, skills).await
}

pub(super) async fn verify_linux(
    sandbox: &sandbox::SandboxHandle,
    expected_version: Option<&str>,
) -> Result<(), Error> {
    let output = super::version_output(sandbox, "claude").await?;
    if !output.status.success() {
        let message = format!("`claude --version` exited with code {}", output.status.code);
        // 126/127 mean the image does not provide the harness; retrying cannot change that.
        // Any other failure this early in the guest's life may be transient.
        return Err(if matches!(output.status.code, 126 | 127) {
            Error::Invalid(format!("Claude Code is missing: {message}"))
        } else {
            Error::SandboxSetup(message)
        });
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::SandboxSetup("`claude --version` returned non-UTF-8 output".into()))?;
    let installed = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::SandboxSetup("`claude --version` returned no version".into()))?;
    if let Some(expected) = expected_version.filter(|expected| *expected != installed) {
        return Err(Error::Invalid(format!(
            "declared Claude Code version {expected:?} does not match installed version {installed:?}"
        )));
    }
    Ok(())
}

pub(super) fn launch_linux(request: &LaunchRequest<'_>) -> ProcessLaunch {
    let config = format!("{}/.claude", request.home);
    let mut base = format!("claude --dangerously-skip-permissions --settings {config}/agent-settings.json");
    // Claude Code takes a model alias (`fable`, `opus`) or full model name, and one of its own
    // effort levels. Both are opaque here and apply to fresh and resumed conversations alike.
    if let Some(model) = &request.model_selection.model {
        let _infallible = write!(base, " --model {}", shell_single_quoted(model.as_str()));
    }
    if let Some(effort) = &request.model_selection.effort {
        let _infallible = write!(base, " --effort {}", shell_single_quoted(effort.as_str()));
    }
    // A fresh conversation may start on a positional prompt; `--` keeps a prompt
    // that begins with `-` from being read as an option.
    let fresh = request.initial_prompt.map_or_else(
        || base.clone(),
        |message| format!("{base} -- {}", shell_single_quoted(message)),
    );
    // Claude Code currently reports UUID conversation IDs. Keep that
    // harness-specific constraint out of the generic Session reconciler.
    let resume = request.resume.and_then(|native| native.parse::<uuid::Uuid>().ok());
    let command = match resume {
        // SessionStart can report an ID before Claude creates its JSONL. Treat
        // the harness-owned transcript as the authority for resumability so
        // an untouched Session can still wake from Idle as a fresh Session.
        Some(native) => format!(
            "if /usr/bin/find {config}/projects -type f -name '{native}.jsonl' -print -quit 2>/dev/null \
             | /usr/bin/grep -q .; then exec {base} --resume {native}; else exec {fresh}; fi"
        ),
        None => fresh,
    };
    ProcessLaunch {
        command,
        // Launch-only overrides keep the tmux session non-interactive and its
        // conversation in tmux history without depending on image ENV.
        environment: vec![
            ("CLAUDE_CONFIG_DIR".into(), config),
            (DISABLE_ALTERNATE_SCREEN_ENVIRONMENT.into(), "1".into()),
            ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ],
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_manifest_cannot_redeclare_either_claude_binding() {
        for name in [super::ACCESS_ENVIRONMENT, super::NESTED_ENVIRONMENT] {
            assert!(super::manages_environment(name));
            assert!(super::conflicts_with_managed_secret(name, None));
        }
        for placeholder in [super::ACCESS_PLACEHOLDER, super::NESTED_PLACEHOLDER] {
            assert!(super::conflicts_with_managed_secret("UNRELATED", Some(placeholder)));
        }
        assert!(super::manages_environment(super::DISABLE_ALTERNATE_SCREEN_ENVIRONMENT));
    }

    const TOKEN: &str = "sk-ant-oat01-Ab_c-9";

    #[test]
    fn the_last_setup_token_is_found_between_terminal_escapes() {
        for (output, expected) in [
            (format!("Your token:\n{TOKEN}\n"), Some(TOKEN)),
            (format!("\x1b[1m{TOKEN}\x1b[22m"), Some(TOKEN)),
            (format!("> sk-ant-oat01-typed\x1b[2K\r{TOKEN}\n"), Some(TOKEN)),
            ("Paste code here if prompted >".to_owned(), None),
        ] {
            assert_eq!(super::find_setup_token(output.as_bytes()), expected, "{output:?}");
        }
    }

    #[test]
    fn a_pasted_token_may_wrap_over_lines_up_to_an_empty_one() {
        let (head, tail) = TOKEN.split_at(10);
        for (input, expected) in [
            (format!("{TOKEN}\n\n"), Some(TOKEN)),
            (format!("{head}\r\n  {tail}\n\nlater"), Some(TOKEN)),
            (TOKEN.to_owned(), Some(TOKEN)),
            ("\n".to_owned(), None),
            ("not-a-token\n\n".to_owned(), None),
            (format!("{TOKEN}; ls\n\n"), None),
        ] {
            let token = super::read_pasted_token(&mut std::io::Cursor::new(input.as_bytes()));
            assert_eq!(token.ok().as_deref().map(String::as_str), expected, "{input:?}");
        }
    }
}

/// Launches run under `sh` with a stub `claude` that records its arguments, in a temporary home
/// standing in for the guest's.
#[cfg(test)]
#[cfg(unix)]
mod launch_tests {
    use tempfile::TempDir;

    use crate::harness::{Effort, LaunchRequest, Model, ModelSelection, has_pair, test_launch_request};

    const NATIVE: &str = "160cdb4b-5997-464c-9d22-602786eb45d4";

    /// A Claude Code home whose project directory holds a transcript named `transcript`, if any.
    fn home_with_transcript(transcript: Option<&str>) -> TempDir {
        let home = TempDir::new().expect("temporary home");
        let project = home.path().join(".claude/projects/-home-agent-code");
        std::fs::create_dir_all(&project).expect("project directory");
        if let Some(transcript) = transcript {
            std::fs::write(project.join(transcript), "{}\n").expect("transcript");
        }
        home
    }

    /// The arguments Claude Code starts with when `request` launches it in `home`.
    fn launched(home: &TempDir, request: &LaunchRequest<'_>) -> Vec<String> {
        let home = home.path().to_str().expect("UTF-8 home");
        let launch = super::launch_linux(&LaunchRequest { home, ..*request });
        crate::harness::run_launch("claude", &launch).expect("Claude Code started")
    }

    #[test]
    fn every_launch_skips_permission_prompts_and_reads_the_agent_settings() {
        let home = home_with_transcript(None);
        let arguments = launched(&home, &test_launch_request(None, None));

        assert_eq!(
            arguments.first().map(String::as_str),
            Some("--dangerously-skip-permissions")
        );
        let settings = home.path().join(".claude/agent-settings.json");
        assert!(has_pair(&arguments, "--settings", settings.to_str().expect("UTF-8")));
    }

    #[test]
    fn resume_launch_requires_a_native_transcript() {
        let transcript = format!("{NATIVE}.jsonl");
        let resumed = launched(
            &home_with_transcript(Some(&transcript)),
            &test_launch_request(Some(NATIVE), None),
        );
        assert!(
            resumed.ends_with(&["--resume".to_owned(), NATIVE.to_owned()]),
            "{resumed:?}"
        );

        for transcript in [None, Some(format!("{NATIVE}.jsonl.bak"))] {
            let fresh = launched(
                &home_with_transcript(transcript.as_deref()),
                &test_launch_request(Some(NATIVE), None),
            );
            assert!(!fresh.contains(&"--resume".to_owned()), "{transcript:?}: {fresh:?}");
        }
    }

    #[test]
    fn non_uuid_native_id_is_not_a_claude_resume_target() {
        let home = home_with_transcript(Some("opaque-harness-id.jsonl"));
        let arguments = launched(&home, &test_launch_request(Some("opaque-harness-id"), None));

        assert!(!arguments.contains(&"--resume".to_owned()), "{arguments:?}");
    }

    #[test]
    fn a_fresh_launch_passes_the_first_prompt_as_one_argument() {
        // `--` keeps a prompt that starts with `-` or names a subcommand positional.
        let arguments = launched(
            &home_with_transcript(None),
            &test_launch_request(None, Some("fix it's\nbroken")),
        );

        assert!(
            arguments.ends_with(&["--".to_owned(), "fix it's\nbroken".to_owned()]),
            "{arguments:?}"
        );
        assert!(!arguments.contains(&"--resume".to_owned()));
    }

    #[test]
    fn launches_select_no_model_or_effort_unless_the_session_carries_them() {
        let arguments = launched(&home_with_transcript(None), &test_launch_request(None, None));

        assert!(!arguments.contains(&"--model".to_owned()), "{arguments:?}");
        assert!(!arguments.contains(&"--effort".to_owned()), "{arguments:?}");
    }

    #[test]
    fn model_and_effort_apply_to_fresh_and_resumed_conversations() {
        let selection = ModelSelection {
            model: Some(Model::new("fable").expect("model")),
            effort: Some(Effort::new("xhigh").expect("effort")),
        };
        let selected = LaunchRequest {
            model_selection: &selection,
            ..test_launch_request(Some(NATIVE), Some("go"))
        };
        let transcript = format!("{NATIVE}.jsonl");

        let resumed = launched(&home_with_transcript(Some(&transcript)), &selected);
        let fresh = launched(&home_with_transcript(None), &selected);
        for arguments in [&resumed, &fresh] {
            assert!(has_pair(arguments, "--model", "fable"), "{arguments:?}");
            assert!(has_pair(arguments, "--effort", "xhigh"), "{arguments:?}");
        }
        assert!(
            resumed.ends_with(&["--resume".to_owned(), NATIVE.to_owned()]),
            "{resumed:?}"
        );
        assert!(fresh.ends_with(&["--".to_owned(), "go".to_owned()]), "{fresh:?}");
    }
}
