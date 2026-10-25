//! Signing in through the browser with the GitHub CLI, kept apart from the host's own `gh` login.

use std::path::Path;

use zeroize::Zeroizing;

use crate::Error;

/// The part of [`super::SCOPES`] the GitHub CLI does not request by default; its defaults are
/// `repo`, `read:org` and `gist`.
const ADDITIONAL_SCOPES: &str = "workflow,read:project";
/// Environment that would make `gh` use another token or host than the sign-in's.
const GH_ENVIRONMENT: [&str; 5] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
];

/// Signs in through the GitHub CLI `program`'s browser flow and returns the new token.
///
/// `gh` runs against a new configuration directory and without any token in its environment, so
/// the host's own `gh` login is neither read nor changed. The directory, which holds the token,
/// is deleted before this returns, also when Ctrl-C ends the sign-in.
///
/// # Errors
///
/// Returns an error, never containing the token, when `gh` cannot run, the sign-in does not
/// complete, or `gh` gives no token.
pub fn sign_in(program: &Path) -> Result<Zeroizing<String>, Error> {
    let configuration = tempfile::TempDir::new()?;
    let mut login = std::process::Command::new(program);
    login.args([
        "auth",
        "login",
        "--hostname",
        "github.com",
        "--web",
        // The person types the one-time code into the browser, so gh copies it for them.
        "--clipboard",
        // With `https`, gh offers to make itself Git's credential helper, a question about a
        // login that is about to be deleted. The protocol choice is deleted with it.
        "--git-protocol",
        "ssh",
        "--skip-ssh-key",
        // Plain text in the new directory, never the host's credential store.
        "--insecure-storage",
        "--scopes",
        ADDITIONAL_SCOPES,
    ]);
    isolate(&mut login, configuration.path());
    login.stderr(std::process::Stdio::piped());
    let status = interruptible(|| {
        let mut child = login
            .spawn()
            .map_err(|error| Error::Invalid(format!("could not run `gh auth login`: {error}")))?;
        let relay = child.stderr.take().map(|mut output| {
            std::thread::spawn(move || {
                let mut relay = Relay::default();
                let mut chunk = [0_u8; 4_096];
                while let Ok(read @ 1..) = std::io::Read::read(&mut output, &mut chunk) {
                    let mut terminal = std::io::stderr();
                    let _ = std::io::Write::write_all(&mut terminal, &relay.feed(&chunk[..read]));
                    let _ = std::io::Write::flush(&mut terminal);
                }
                relay.held
            })
        });
        let status = child.wait()?;
        Ok((status, relay.and_then(|relay| relay.join().ok()).unwrap_or_default()))
    });
    let (status, held) = status?;
    if !status.success() {
        // What gh said after the browser step may explain the failure, so it is shown after all.
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &held);
        return Err(Error::Invalid("the GitHub sign-in did not complete".into()));
    }
    token(program, configuration.path())
}

/// Runs `run` with Ctrl-C left to the child it starts: the terminal interrupts agentctl too,
/// which carries on, so the sign-in's directory is still deleted. Ctrl-C ends agentctl again
/// once `run` returns.
#[cfg(unix)]
fn interruptible<T>(run: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    use std::sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    };

    static INTERRUPT_ENDS: OnceLock<Option<Arc<AtomicBool>>> = OnceLock::new();
    let interrupt_ends = INTERRUPT_ENDS.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(true));
        signal_hook::flag::register_conditional_default(signal_hook::consts::SIGINT, Arc::clone(&flag))
            .ok()
            .map(|_| flag)
    });
    let set = |ends: bool| {
        if let Some(flag) = interrupt_ends {
            flag.store(ends, Ordering::SeqCst);
        }
    };
    set(false);
    let result = run();
    set(true);
    result
}

#[cfg(not(unix))]
fn interruptible<T>(run: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    run()
}

/// Passes on what `gh auth login` writes until the browser approved the sign-in: the one-time
/// code, the prompt to open the browser and the confirmation. What follows concerns only the
/// configuration about to be deleted, so it is held back and shown only if the sign-in fails.
#[derive(Default)]
struct Relay {
    shown: Vec<u8>,
    complete: bool,
    held: Vec<u8>,
}

impl Relay {
    const COMPLETE: &'static [u8] = b"Authentication complete";

    /// Takes the next output of `gh`, returning the part to show now.
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.complete {
            self.held.extend_from_slice(chunk);
            return Vec::new();
        }
        let start = self.shown.len();
        self.shown.extend_from_slice(chunk);
        let Some(found) = self
            .shown
            .windows(Self::COMPLETE.len())
            .position(|window| window == Self::COMPLETE)
        else {
            return chunk.to_vec();
        };
        self.complete = true;
        let line_end = self.shown[found..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(self.shown.len(), |offset| found + offset + 1);
        self.held.extend_from_slice(&self.shown[line_end..]);
        self.shown[start.min(line_end)..line_end].to_vec()
    }
}

fn isolate(command: &mut std::process::Command, configuration: &Path) {
    command.env("GH_CONFIG_DIR", configuration);
    // Whatever gh would write to the global Git configuration stays in the same directory.
    command.env("GIT_CONFIG_GLOBAL", configuration.join("gitconfig"));
    for name in GH_ENVIRONMENT {
        command.env_remove(name);
    }
}

/// The token `gh auth token` gives from `configuration`.
fn token(program: &Path, configuration: &Path) -> Result<Zeroizing<String>, Error> {
    let mut command = std::process::Command::new(program);
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    isolate(&mut command, configuration);
    let output = command
        .output()
        .map_err(|error| Error::Invalid(format!("could not run `gh auth token`: {error}")))?;
    let token = Zeroizing::new(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if !output.status.success() || token.is_empty() {
        return Err(Error::Invalid("the GitHub CLI gave no token after signing in".into()));
    }
    Ok(token)
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::expect_used)]

    use tempfile::TempDir;

    use super::*;

    /// A stand-in `gh` that records how it was run beside itself and keeps its token in
    /// `GH_CONFIG_DIR`, as the real one does with `--insecure-storage`.
    fn fake_gh(directory: &TempDir, login_exit: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let program = directory.path().join("gh");
        let log = directory.path().join("log");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n\
                 echo \"$1 $2 config=$GH_CONFIG_DIR git=$GIT_CONFIG_GLOBAL token=$GH_TOKEN args=$*\" >> '{log}'\n\
                 case \"$1 $2\" in\n\
                 'auth login') echo gho_FAKE_isolated > \"$GH_CONFIG_DIR/token\"; exit {login_exit} ;;\n\
                 'auth token') cat \"$GH_CONFIG_DIR/token\" ;;\n\
                 esac\n",
                log = log.display()
            ),
        )
        .expect("fake gh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("executable");
        program
    }

    fn runs(directory: &TempDir) -> Vec<String> {
        let log = std::fs::read_to_string(directory.path().join("log")).expect("log");
        log.lines().map(str::to_owned).collect()
    }

    /// The configuration directory a recorded run used.
    fn config_of(run: &str) -> &str {
        run.split("config=")
            .nth(1)
            .and_then(|rest| rest.split(" git=").next())
            .expect("configuration directory")
    }

    #[test]
    fn the_sign_in_runs_in_its_own_configuration_that_is_deleted_afterwards() {
        let directory = TempDir::new().expect("temporary directory");
        let program = fake_gh(&directory, 0);

        let token = sign_in(&program).expect("sign-in");

        assert_eq!(token.as_str(), "gho_FAKE_isolated");
        let runs = runs(&directory);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert!(runs[0].starts_with("auth login "), "{runs:?}");
        for flag in [
            "--web",
            "--git-protocol ssh",
            "--skip-ssh-key",
            "--insecure-storage",
            "--scopes workflow,read:project",
            "--hostname github.com",
        ] {
            assert!(runs[0].contains(flag), "{flag} in {runs:?}");
        }
        assert!(runs[1].starts_with("auth token "), "{runs:?}");
        let configuration = config_of(&runs[0]);
        let host = std::env::var_os("GH_CONFIG_DIR");
        assert!(
            Path::new(configuration).starts_with(std::env::temp_dir())
                && host.is_none_or(|host| Path::new(&host) != Path::new(configuration)),
            "a new directory, never the host's: {runs:?}"
        );
        for run in &runs {
            assert_eq!(config_of(run), configuration, "both runs share it: {runs:?}");
            assert!(
                run.contains(&format!("git={configuration}/gitconfig token= ")),
                "gh cannot write the host's Git configuration or use a token from the environment: {runs:?}"
            );
        }
        assert!(!Path::new(configuration).exists(), "the token's directory is deleted");
    }

    #[test]
    fn an_abandoned_sign_in_is_an_error_and_its_directory_is_deleted() {
        let directory = TempDir::new().expect("temporary directory");
        let program = fake_gh(&directory, 130);

        let error = sign_in(&program).expect_err("an abandoned sign-in");

        assert!(!error.to_string().contains("gho_"), "{error}");
        let runs = runs(&directory);
        assert_eq!(runs.len(), 1, "no token is asked for: {runs:?}");
        assert!(
            !Path::new(config_of(&runs[0])).exists(),
            "the token's directory is deleted"
        );
        sign_in(&directory.path().join("missing")).expect_err("no GitHub CLI");
    }

    #[test]
    fn only_the_code_prompt_and_confirmation_of_a_sign_in_are_shown() {
        let mut relay = Relay::default();
        let mut shown = Vec::new();
        for chunk in [
            &b"! First copy your one-time code: AB12-CD34\nPress Enter to open https://github.com/login/device..."[..],
            b" \n\xe2\x9c\x93 Authentic",
            b"ation complete.\n- gh config set -h github.com git_protocol ssh\n",
            b"\xe2\x9c\x93 Configured git protocol\n! Authentication credentials saved in plain text\n",
            b"\xe2\x9c\x93 Logged in as digdir-bot\n",
        ] {
            shown.extend(relay.feed(chunk));
        }
        let shown = String::from_utf8(shown).expect("UTF-8");
        assert!(shown.contains("one-time code: AB12-CD34"), "{shown}");
        assert!(shown.contains("Press Enter to open"), "{shown}");
        assert!(shown.ends_with("Authentication complete.\n"), "{shown}");
        let held = String::from_utf8(relay.held).expect("UTF-8");
        assert!(held.starts_with("- gh config set"), "{held}");
        assert!(held.contains("plain text") && held.contains("Logged in as"), "{held}");
    }
}
