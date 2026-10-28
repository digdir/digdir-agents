//! Whether an application started here reaches the person, and the program
//! that opens addresses on this operating system.

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

/// The opener under WSL, a Windows program reached through the Windows `PATH`.
pub(super) const WSL_OPENER: &str = "rundll32.exe";

/// Hands `url` to the program that opens addresses on this operating system,
/// and names that program.
///
/// # Errors
///
/// Returns why the opener could not be started or failed at once.
pub(crate) async fn open_url(url: &str) -> Result<&'static str, String> {
    let opener = opener(|name| std::env::var_os(name));
    let arguments = opener
        .arguments
        .iter()
        .copied()
        .chain([url])
        .map(str::to_owned)
        .collect::<Vec<_>>();
    super::run_detached(OsStr::new(opener.program), &arguments, opener.exit_tells)
        .await
        .map(|()| opener.program)
}

/// Puts `text` on this machine's clipboard, and names the program that did. A terminal reached
/// over SSH has no clipboard here that the person uses, nor has a Linux session without a
/// display, so nothing is copied there.
///
/// # Errors
///
/// Returns why nothing was copied.
pub(crate) async fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    use tokio::io::AsyncWriteExt as _;

    let variable = |name: &str| std::env::var_os(name);
    if set(&variable, "SSH_CONNECTION") || set(&variable, "SSH_TTY") {
        return Err("this terminal is reached over SSH, so the clipboard here is not yours".into());
    }
    let path = variable("PATH");
    let (program, arguments) = copiers(variable)
        .into_iter()
        .find(|(program, _)| find_executable(path.as_deref(), &[program]).is_some())
        .ok_or_else(|| "no clipboard program for this session is installed".to_owned())?;
    let mut child = tokio::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|error| format!("could not write to {program}: {error}"))?;
    }
    // Some copiers stay behind to serve the clipboard, once their first process has exited.
    match tokio::time::timeout(super::LAUNCH_GRACE, child.wait()).await {
        Ok(Ok(status)) if !status.success() => Err(format!("{program} failed ({status})")),
        Ok(Err(error)) => Err(format!("could not wait for {program}: {error}")),
        Ok(Ok(_)) | Err(_) => Ok(program),
    }
}

/// The programs that copy their input to the clipboard of this session, with their arguments, the
/// first installed one to be used. On Linux that depends on the display the session has: a
/// Wayland copier fails without a Wayland display, and an X one without an X display.
fn copiers(variable: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", &[])];
    }
    if cfg!(target_os = "windows") || wsl(&variable) {
        // Under WSL, the Windows clipboard is the person's.
        return vec![("clip.exe", &[])];
    }
    let mut copiers: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if set(&variable, "WAYLAND_DISPLAY") {
        copiers.push(("wl-copy", &[]));
    }
    if set(&variable, "DISPLAY") {
        copiers.extend([
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ]);
    }
    copiers
}

/// A program that opens the address given after its own `arguments`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Opener {
    program: &'static str,
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
fn opener(variable: impl Fn(&str) -> Option<OsString>) -> Opener {
    if cfg!(target_os = "macos") {
        Opener::new("open", &[], true)
    } else if cfg!(target_os = "windows") {
        // Explorer exits with 1 even when it opened the address, so there only
        // a failure to start it tells.
        Opener::new("explorer", &[], false)
    } else if wsl(variable) {
        // Explorer started from WSL opens a folder instead of an address with
        // a long query string, such as a sign-in link; the URL protocol
        // handler opens any address in the default browser.
        Opener::new(WSL_OPENER, &["url.dll,FileProtocolHandler"], true)
    } else {
        Opener::new("xdg-open", &[], true)
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

/// Whether this is Linux under WSL, whose Linux openers cannot reach the
/// Windows desktop but whose Windows programs can.
pub(super) fn wsl(variable: impl Fn(&str) -> Option<OsString>) -> bool {
    cfg!(target_os = "linux") && set(&variable, "WSL_DISTRO_NAME")
}

pub(super) fn set(variable: &impl Fn(&str) -> Option<OsString>, name: &str) -> bool {
    variable(name).is_some_and(|value| !value.is_empty())
}

/// Finds the first of `names` in `path`, a `PATH`-style variable, trying every
/// absolute directory for one name before the next.
pub(super) fn find_executable(path: Option<&OsStr>, names: &[&str]) -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::{super::test_support::*, *};

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn on_linux_the_clipboard_copier_is_one_for_the_display_the_session_has() {
        fn names(variables: &'static [(&'static str, &'static str)]) -> Vec<&'static str> {
            copiers(only(variables))
                .into_iter()
                .map(|(program, _)| program)
                .collect()
        }
        assert_eq!(names(&[("DISPLAY", ":0")]), ["xclip", "xsel"]);
        assert_eq!(names(&[("WAYLAND_DISPLAY", "wayland-0")]), ["wl-copy"]);
        assert_eq!(names(&[]), Vec::<&str>::new());
        assert_eq!(names(&[("WSL_DISTRO_NAME", "Ubuntu")]), ["clip.exe"]);
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
    fn under_wsl_addresses_open_with_the_windows_url_handler() {
        assert_eq!(opener(only(UNDER_WSL)).program, WSL_OPENER);
        assert_eq!(opener(only(&[])).program, "xdg-open");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn elsewhere_a_terminal_reached_over_ssh_is_somewhere_else() {
        assert!(launch_blocked(only(&[("SSH_CONNECTION", "10.0.0.1 5000 10.0.0.2 22")])).is_some());
        assert_eq!(launch_blocked(only(&[])), None);
    }
}
