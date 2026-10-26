//! Hands addresses and editor sessions to applications on the machine running `agentctl`.
//!
//! Everything here is best effort: no portable viewer or editor exists. A
//! launcher that fails at once is reported; one that fails later, after it
//! has handed over to the application, cannot be observed.

mod browser;
mod host;

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub(crate) use browser::{as_browser, open_argument, sign_in};
pub(crate) use host::{launch_blocked, open_url};

/// How long a launcher gets to fail before it is taken to have launched.
const LAUNCH_GRACE: Duration = Duration::from_secs(2);

/// Windows `CREATE_NO_WINDOW`: a console launcher such as `code.cmd` opens no console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An editor that opens an agent's working directory over SSH.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Editor {
    VsCode,
    Zed,
}

impl Editor {
    pub(crate) const ALL: [Self; 2] = [Self::VsCode, Self::Zed];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::Zed => "Zed",
        }
    }

    /// Executable names the editor's command-line launcher is installed as, most common first.
    /// Some distribution packages install Zed's as `zeditor`.
    const fn executables(self) -> &'static [&'static str] {
        match self {
            Self::VsCode if cfg!(windows) => &["code.cmd"],
            Self::VsCode => &["code"],
            Self::Zed if cfg!(windows) => &["zed.exe"],
            Self::Zed => &["zed", "zeditor"],
        }
    }

    /// Finds the editor's launcher on `path`, a `PATH`-style variable.
    pub(crate) fn locate(self, path: Option<&OsStr>) -> Option<PathBuf> {
        host::find_executable(path, self.executables())
    }

    /// How to open `directory` in the agent reached as the OpenSSH `alias`, given the launcher
    /// found on `PATH`. VS Code falls back to its URL handler; Zed has none to fall back to.
    pub(crate) fn launch(self, launcher: Option<&Path>, alias: &str, directory: &str) -> Option<Launch> {
        match (self, launcher) {
            (Self::VsCode, Some(program)) => Some(Launch::Command {
                program: program.to_path_buf(),
                arguments: vec!["--remote".into(), format!("ssh-remote+{alias}"), directory.to_owned()],
            }),
            (Self::VsCode, None) => Some(Launch::Url(format!(
                "vscode://vscode-remote/ssh-remote+{alias}{directory}"
            ))),
            (Self::Zed, Some(program)) => Some(Launch::Command {
                program: program.to_path_buf(),
                arguments: vec![format!("ssh://{alias}{directory}")],
            }),
            (Self::Zed, None) => None,
        }
    }

    /// Why the editor cannot be launched without its command-line launcher, if it needs one.
    pub(crate) fn missing_launcher(self) -> Option<String> {
        match self {
            Self::VsCode => None,
            Self::Zed => Some(format!("not found on PATH ({})", self.executables().join(", "))),
        }
    }
}

/// One way to hand something to a local application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Launch {
    /// Runs a program with arguments, detached from the terminal.
    Command { program: PathBuf, arguments: Vec<String> },
    /// Opens an address with whichever application handles its scheme.
    Url(String),
}

impl Launch {
    /// Starts the launch, detached from the caller's terminal, and waits
    /// briefly for it to fail.
    ///
    /// # Errors
    ///
    /// Returns why the program could not be started or failed at once, such as
    /// an opener finding no application for the address.
    pub(crate) async fn start(&self) -> Result<(), String> {
        match self {
            Self::Command { program, arguments } => run_detached(program.as_os_str(), arguments, true).await,
            Self::Url(url) => open_url(url).await.map(|_| ()),
        }
    }
}

/// Runs a launcher outside the caller's terminal: its output is discarded,
/// and it gets a process group of its own, so closing the terminal does not
/// close the application. One still running after [`LAUNCH_GRACE`] is taken
/// to have launched and is left running; Tokio reaps it when it exits. With
/// `exit_tells` unset, an exit status is not taken as a failure.
async fn run_detached(program: &OsStr, arguments: &[String], exit_tells: bool) -> Result<(), String> {
    let name = program.to_string_lossy();
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run {name}: {error}"))?;
    match tokio::time::timeout(LAUNCH_GRACE, child.wait()).await {
        Ok(Ok(status)) if exit_tells && !status.success() => Err(format!("{name} failed ({status})")),
        Ok(Err(error)) => Err(format!("could not wait for {name}: {error}")),
        Ok(Ok(_)) | Err(_) => Ok(()),
    }
}

/// The address that opens a forward listening at `local` to `guest_port`: a
/// VNC client for the desktop's RFB port, a browser for anything else.
pub(crate) fn forward_url(local: impl std::fmt::Display, guest_port: u16) -> String {
    if guest_port == agent::vnc::GUEST_PORT {
        format!("vnc://{local}")
    } else {
        format!("http://{local}/")
    }
}

/// Names a forward to one of the desktop's ports, however it was made.
pub(crate) const fn forward_label(guest_port: u16) -> Option<&'static str> {
    match guest_port {
        agent::vnc::WEB_GUEST_PORT => Some("desktop"),
        agent::vnc::GUEST_PORT => Some("vnc"),
        _ => None,
    }
}

/// Environments for tests of what the person's terminal reaches.
#[cfg(test)]
mod test_support {
    #![allow(clippy::expect_used)]

    use std::ffi::OsString;

    /// An environment holding only `pairs`.
    pub(super) fn only(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |variable: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == variable)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) const UNDER_WSL: &[(&str, &str)] = &[("WSL_DISTRO_NAME", "Ubuntu")];

    /// `pairs`, with `PATH` holding only `directory`.
    #[cfg(target_os = "linux")]
    pub(super) fn with_path(
        directory: &std::path::Path,
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<OsString> {
        let path = directory.as_os_str().to_owned();
        let rest = only(pairs);
        move |variable: &str| {
            if variable == "PATH" {
                Some(path.clone())
            } else {
                rest(variable)
            }
        }
    }

    /// A directory standing in for the Windows `PATH` WSL adds.
    #[cfg(target_os = "linux")]
    pub(super) fn windows_path() -> tempfile::TempDir {
        let windows = tempfile::tempdir().expect("windows directory");
        std::fs::write(windows.path().join(super::host::WSL_OPENER), "").expect("opener");
        windows
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn vs_code_uses_its_launcher_or_else_its_url_handler() {
        let launcher = Path::new("/opt/bin/code");
        assert_eq!(
            Editor::VsCode.launch(Some(launcher), "agentctl-worker", "/srv/work"),
            Some(Launch::Command {
                program: launcher.to_path_buf(),
                arguments: vec![
                    "--remote".into(),
                    "ssh-remote+agentctl-worker".into(),
                    "/srv/work".into()
                ],
            })
        );
        assert_eq!(
            Editor::VsCode.launch(None, "agentctl-worker", "/srv/work"),
            Some(Launch::Url(
                "vscode://vscode-remote/ssh-remote+agentctl-worker/srv/work".into()
            ))
        );
        assert_eq!(Editor::VsCode.missing_launcher(), None);
    }

    #[test]
    fn zed_needs_its_launcher() {
        let launcher = Path::new("/opt/bin/zeditor");
        assert_eq!(
            Editor::Zed.launch(Some(launcher), "agentctl-worker", "/srv/work"),
            Some(Launch::Command {
                program: launcher.to_path_buf(),
                arguments: vec!["ssh://agentctl-worker/srv/work".into()],
            })
        );
        assert_eq!(Editor::Zed.launch(None, "agentctl-worker", "/srv/work"), None);
        let names = if cfg!(windows) { "zed.exe" } else { "zed, zeditor" };
        assert_eq!(
            Editor::Zed.missing_launcher(),
            Some(format!("not found on PATH ({names})"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn launchers_are_found_under_any_of_their_names_in_path_order() {
        let first = tempfile::tempdir().expect("directory");
        let second = tempfile::tempdir().expect("directory");
        std::fs::write(second.path().join("zeditor"), "").expect("zeditor");
        let path = std::env::join_paths([first.path(), Path::new("relative"), second.path()]).expect("PATH");

        assert_eq!(Editor::Zed.locate(Some(&path)), Some(second.path().join("zeditor")));
        std::fs::write(first.path().join("zed"), "").expect("zed");
        assert_eq!(Editor::Zed.locate(Some(&path)), Some(first.path().join("zed")));
        assert_eq!(Editor::VsCode.locate(Some(&path)), None);
        assert_eq!(Editor::VsCode.locate(None), None);
    }

    #[test]
    fn forwards_open_in_the_application_for_their_guest_port() {
        assert_eq!(forward_url("127.0.0.1:53817", 5900), "vnc://127.0.0.1:53817");
        assert_eq!(forward_url("127.0.0.1:53817", 6080), "http://127.0.0.1:53817/");
    }

    #[test]
    fn forwards_to_the_desktop_are_named_by_their_port() {
        assert_eq!(forward_label(6080), Some("desktop"));
        assert_eq!(forward_label(5900), Some("vnc"));
        assert_eq!(forward_label(3000), None);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "local")]
    async fn a_launcher_that_fails_at_once_is_reported_and_one_that_runs_is_launched() {
        assert!(run_detached(OsStr::new("false"), &[], true).await.is_err());
        assert_eq!(
            run_detached(OsStr::new("false"), &[], false).await,
            Ok(()),
            "an opener whose exit status says nothing"
        );
        assert_eq!(run_detached(OsStr::new("true"), &[], true).await, Ok(()));
        assert_eq!(
            run_detached(OsStr::new("sleep"), &["5".into()], true).await,
            Ok(()),
            "still running after the grace period"
        );
        assert!(
            run_detached(OsStr::new("agentctl-no-such-launcher"), &[], true)
                .await
                .is_err_and(|error| error.starts_with("could not run"))
        );
    }
}
