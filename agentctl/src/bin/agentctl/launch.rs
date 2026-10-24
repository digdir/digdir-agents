//! Hands addresses and editor sessions to applications on the machine running `agentctl`.
//!
//! Everything here is best effort: no portable viewer or editor exists. A
//! launcher that fails at once is reported; one that fails later, after it
//! has handed over to the application, cannot be observed.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use agent::harness::{SignIn, UrlOpener};

/// How long a launcher gets to fail before it is taken to have launched.
const LAUNCH_GRACE: Duration = Duration::from_secs(2);

/// The opener under WSL, a Windows program reached through the Windows `PATH`.
const WSL_OPENER: &str = "rundll32.exe";

/// Windows `CREATE_NO_WINDOW`: a console launcher such as `code.cmd` opens no console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An editor that opens an Agent's working directory over SSH.
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
        find_executable(path, self.executables())
    }

    /// How to open `directory` in the Agent reached as the OpenSSH `alias`, given the launcher
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
            Self::Url(url) => open_url(url).await,
        }
    }
}

/// Hands `url` to the program that opens addresses on this operating system.
///
/// # Errors
///
/// Returns why the opener could not be started or failed at once.
pub(crate) async fn open_url(url: &str) -> Result<(), String> {
    let opener = opener();
    let arguments = opener
        .arguments
        .iter()
        .copied()
        .chain([url])
        .map(str::to_owned)
        .collect::<Vec<_>>();
    run_detached(OsStr::new(opener.program), &arguments, opener.exit_tells).await
}

/// A program that opens the address given after its own `arguments`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Opener {
    pub(crate) program: &'static str,
    arguments: &'static [&'static str],
    /// Whether a failing exit status means the address did not open.
    exit_tells: bool,
}

impl Opener {
    const fn new(program: &'static str, arguments: &'static [&'static str], exit_tells: bool) -> Self {
        Self {
            program,
            arguments,
            exit_tells,
        }
    }
}

/// The program that opens addresses on this operating system.
pub(crate) fn opener() -> Opener {
    if cfg!(target_os = "macos") {
        Opener::new("open", &[], true)
    } else if cfg!(target_os = "windows") {
        // Explorer exits with 1 even when it opened the address, so there only
        // a failure to start it tells.
        Opener::new("explorer", &[], false)
    } else if wsl(|name| std::env::var_os(name)) {
        // Explorer started from WSL opens a folder instead of an address with
        // a long query string, such as a sign-in link; the URL protocol
        // handler opens any address in the default browser.
        Opener::new(WSL_OPENER, &["url.dll,FileProtocolHandler"], true)
    } else {
        Opener::new("xdg-open", &[], true)
    }
}

/// Set for a harness that runs agentctl as its browser, so that agentctl only
/// opens the address it is given, with [`opener`].
const AS_BROWSER_ENVIRONMENT: &str = "AGENTCTL_AS_BROWSER";

/// Whether agentctl runs as a harness's browser rather than as itself.
pub(crate) fn as_browser(variable: impl Fn(&str) -> Option<OsString>) -> bool {
    set(&variable, AS_BROWSER_ENVIRONMENT)
}

/// Opens the one address agentctl is given when it runs as a harness's browser.
pub(crate) fn open_argument() -> std::process::ExitCode {
    let opened = address_to_open(std::env::args_os().skip(1)).and_then(|url| {
        tokio::runtime::LocalRuntime::new()
            .map_err(|error| format!("could not start: {error}"))
            .and_then(|runtime| runtime.block_on(open_url(&url)))
    });
    opened.map_or_else(
        |error| {
            eprintln!("agentctl: {error}");
            std::process::ExitCode::FAILURE
        },
        |()| std::process::ExitCode::SUCCESS,
    )
}

/// The address a harness asks its browser to open. Only web addresses are
/// accepted: the openers behind it also open files and start programs.
fn address_to_open(arguments: impl IntoIterator<Item = OsString>) -> Result<String, String> {
    let mut arguments = arguments.into_iter();
    match (arguments.next().map(OsString::into_string), arguments.next()) {
        (Some(Ok(url)), None) if url.starts_with("https://") || url.starts_with("http://") => Ok(url),
        _ => Err("expected one http or https address to open".to_owned()),
    }
}

/// Where a harness login sends the person: a browser here when one would reach
/// them, or else a link to open elsewhere. A `BROWSER` of the person's own is
/// the harness's to use; otherwise, under WSL, where a harness's own opener
/// misses the Windows desktop, `agentctl`, this program, stands in as its
/// browser.
pub(crate) fn sign_in(variable: impl Fn(&str) -> Option<OsString>, agentctl: Option<PathBuf>) -> SignIn {
    if let Some(reason) = launch_blocked(&variable) {
        return SignIn::Elsewhere { reason };
    }
    if set(&variable, "BROWSER") || !wsl(&variable) {
        return SignIn::Browser(None);
    }
    agentctl.map_or_else(
        || SignIn::Elsewhere {
            reason: "agentctl could not find itself to open the Windows browser".into(),
        },
        |program| {
            SignIn::Browser(Some(UrlOpener {
                program,
                environment: (AS_BROWSER_ENVIRONMENT, "1"),
            }))
        },
    )
}

/// Whether this is Linux under WSL, whose Linux openers cannot reach the
/// Windows desktop but whose Windows programs can.
pub(crate) fn wsl(variable: impl Fn(&str) -> Option<OsString>) -> bool {
    cfg!(target_os = "linux") && set(&variable, "WSL_DISTRO_NAME")
}

fn set(variable: &impl Fn(&str) -> Option<OsString>, name: &str) -> bool {
    variable(name).is_some_and(|value| !value.is_empty())
}

/// Finds the first of `names` in `path`, a `PATH`-style variable, trying every
/// absolute directory for one name before the next.
fn find_executable(path: Option<&OsStr>, names: &[&str]) -> Option<PathBuf> {
    let directories = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.iter().find_map(|name| {
        directories
            .iter()
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
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

/// Why an application started here would not appear in front of the person,
/// when it would not. `AGENTCTL_OPEN=launch` or `copy` overrides the guess.
///
/// On Linux a window needs a display; that also covers `ssh -X` and remote
/// shells that are not SSH. macOS, Windows and WSL, which opens on the Windows
/// desktop, need no display variable and open windows on the machine's own
/// screen, so there a terminal reached over SSH is taken to be elsewhere.
/// WSL also needs the Windows `PATH`, which it may be configured not to add.
pub(crate) fn launch_blocked(variable: impl Fn(&str) -> Option<OsString>) -> Option<String> {
    let set = |name: &str| set(&variable, name);
    match variable("AGENTCTL_OPEN").as_deref().and_then(OsStr::to_str) {
        Some("launch") => return None,
        Some("copy") => return Some("AGENTCTL_OPEN=copy asks to copy instead of opening".into()),
        _ => {}
    }
    let wsl = wsl(&variable);
    if cfg!(any(target_os = "macos", windows)) || wsl {
        if set("SSH_CONNECTION") || set("SSH_TTY") {
            return Some("this terminal is reached over SSH, so it would open on that machine".into());
        }
        (wsl && find_executable(variable("PATH").as_deref(), &[WSL_OPENER]).is_none())
            .then(|| format!("{WSL_OPENER} is not on PATH, so nothing here reaches the Windows desktop"))
    } else {
        (!set("DISPLAY") && !set("WAYLAND_DISPLAY")).then(|| "this terminal has no display to open windows on".into())
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

    fn only(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |variable: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == variable)
                .map(|(_, value)| OsString::from(value))
        }
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

    #[test]
    fn the_override_decides_whatever_the_terminal() {
        assert_eq!(launch_blocked(only(&[("AGENTCTL_OPEN", "launch")])), None);
        assert!(
            launch_blocked(only(&[("AGENTCTL_OPEN", "copy"), ("DISPLAY", ":0")]))
                .is_some_and(|reason| reason.contains("AGENTCTL_OPEN=copy"))
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn on_linux_a_window_needs_a_display_whether_or_not_ssh_is_involved() {
        assert_eq!(launch_blocked(only(&[("DISPLAY", ":0")])), None);
        assert_eq!(launch_blocked(only(&[("WAYLAND_DISPLAY", "wayland-0")])), None);
        assert_eq!(
            launch_blocked(only(&[("DISPLAY", "localhost:10.0"), ("SSH_TTY", "/dev/pts/1")])),
            None,
            "ssh -X shows windows on the person's own screen"
        );
        assert!(launch_blocked(only(&[("SSH_TTY", "/dev/pts/1")])).is_some());
        assert!(launch_blocked(only(&[("DISPLAY", "")])).is_some());
    }

    /// `pairs`, with `PATH` holding only `directory`.
    fn with_path(
        directory: &Path,
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

    fn windows_path() -> tempfile::TempDir {
        let windows = tempfile::tempdir().expect("windows directory");
        std::fs::write(windows.path().join(WSL_OPENER), "").expect("opener");
        windows
    }

    const UNDER_WSL: &[(&str, &str)] = &[("WSL_DISTRO_NAME", "Ubuntu")];

    #[cfg(target_os = "linux")]
    #[test]
    fn under_wsl_the_windows_desktop_is_reached_through_its_path_unless_over_ssh() {
        let windows = windows_path();
        assert!(wsl(only(UNDER_WSL)));
        assert!(!wsl(only(&[("WSL_DISTRO_NAME", "")])));
        assert_eq!(launch_blocked(with_path(windows.path(), UNDER_WSL)), None);
        assert!(
            launch_blocked(only(UNDER_WSL)).is_some_and(|reason| reason.contains(WSL_OPENER)),
            "appendWindowsPath=false leaves no Windows program to open with"
        );
        assert!(
            launch_blocked(with_path(
                windows.path(),
                &[
                    ("WSL_DISTRO_NAME", "Ubuntu"),
                    ("DISPLAY", ":0"),
                    ("SSH_TTY", "/dev/pts/1")
                ]
            ))
            .is_some()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_login_opens_through_agentctl_only_under_wsl_without_a_browser_of_the_persons_own() {
        let windows = windows_path();
        let agentctl = PathBuf::from("/opt/agentctl/agentctl");
        let through_agentctl = SignIn::Browser(Some(UrlOpener {
            program: agentctl.clone(),
            environment: (AS_BROWSER_ENVIRONMENT, "1"),
        }));
        let sign_in_with =
            |pairs, agentctl: Option<&PathBuf>| sign_in(with_path(windows.path(), pairs), agentctl.cloned());
        assert_eq!(sign_in_with(UNDER_WSL, Some(&agentctl)), through_agentctl);
        assert_eq!(
            sign_in_with(&[("WSL_DISTRO_NAME", "Ubuntu"), ("BROWSER", "")], Some(&agentctl)),
            through_agentctl
        );
        assert_eq!(
            sign_in_with(
                &[("WSL_DISTRO_NAME", "Ubuntu"), ("BROWSER", "wslview")],
                Some(&agentctl)
            ),
            SignIn::Browser(None)
        );
        assert!(matches!(sign_in_with(UNDER_WSL, None), SignIn::Elsewhere { .. }));
        assert_eq!(
            sign_in_with(&[("DISPLAY", ":0")], Some(&agentctl)),
            SignIn::Browser(None)
        );
        assert!(matches!(
            sign_in_with(&[("SSH_TTY", "/dev/pts/1")], Some(&agentctl)),
            SignIn::Elsewhere { .. }
        ));
    }

    #[test]
    fn only_a_single_web_address_is_opened_as_a_browser() {
        for (arguments, expected) in [
            (
                &["https://claude.com/authorize?a=1&b=%2F"][..],
                Some("https://claude.com/authorize?a=1&b=%2F"),
            ),
            (
                &["http://localhost:1455/callback"],
                Some("http://localhost:1455/callback"),
            ),
            (&["status"], None),
            (&["C:\\Windows\\notepad.exe"], None),
            (&["https://a.example", "https://b.example"], None),
            (&[], None),
        ] {
            let address = address_to_open(arguments.iter().map(OsString::from));
            assert_eq!(address.ok().as_deref(), expected, "{arguments:?}");
        }
        assert!(!as_browser(only(&[("AGENTCTL_AS_BROWSER", "")])));
        assert!(as_browser(only(&[("AGENTCTL_AS_BROWSER", "1")])));
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn elsewhere_a_terminal_reached_over_ssh_is_somewhere_else() {
        assert!(launch_blocked(only(&[("SSH_CONNECTION", "10.0.0.1 5000 10.0.0.2 22")])).is_some());
        assert_eq!(launch_blocked(only(&[])), None);
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
