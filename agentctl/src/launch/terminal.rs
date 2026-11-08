//! Opening a command in a new window of the person's terminal application, for a client that has no terminal of its
//! own, such as a GUI that attaches to a session.

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use super::Launch;
use crate::ssh::{CommandShell, shell_word};

/// Terminals tried on Linux after `$TERMINAL`, in order, with the arguments that come before the command.
///
/// `xdg-terminal-exec` runs the person's chosen terminal where the desktop supports the proposal. Debian's
/// `x-terminal-emulator` is the system's choice. The rest are the terminals desktops ship with.
const LINUX_TERMINALS: &[(&str, &[&str])] = &[
    ("xdg-terminal-exec", &[]),
    ("x-terminal-emulator", &["-e"]),
    ("kgx", &["--"]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("xterm", &["-e"]),
];

/// How to run `command`, a program and its arguments, in a new window of this machine's terminal application.
///
/// `variable` reads the environment, which decides the terminal on Linux and Windows. The window closes when the
/// command exits.
///
/// # Errors
///
/// Returns why no terminal application was found, or the command is empty.
pub fn in_terminal(command: &[OsString], variable: impl Fn(&str) -> Option<OsString>) -> Result<Launch, String> {
    let (program, arguments) = command.split_first().ok_or("there is no command to run")?;
    if cfg!(target_os = "macos") {
        Ok(terminal_app(program, arguments))
    } else if cfg!(windows) {
        Ok(windows_terminal(program, arguments, &variable))
    } else {
        linux_terminal(program, arguments, &variable)
    }
}

/// Terminal.app, the terminal every Mac has, which runs a command only as a line of shell for it to type.
fn terminal_app(program: &OsStr, arguments: &[OsString]) -> Launch {
    let line = std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|word| shell_word(&word.to_string_lossy(), CommandShell::Posix))
        .collect::<Vec<_>>()
        .join(" ");
    // `exec` makes the window close with the command instead of returning to a shell.
    let script = format!(
        "tell application \"Terminal\"\n  activate\n  do script \"exec {}\"\nend tell",
        applescript_text(&line)
    );
    Launch::Command {
        program: PathBuf::from("osascript"),
        arguments: vec!["-e".into(), script],
    }
}

/// `text` inside a string of Apple's script language, which only reads backslashes and double quotes.
fn applescript_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Windows Terminal when it is installed, as it is by default on Windows 11, or else a console window.
fn windows_terminal(program: &OsStr, arguments: &[OsString], variable: &impl Fn(&str) -> Option<OsString>) -> Launch {
    let words = std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|word| word.to_string_lossy().into_owned());
    if let Some(wt) = crate::local::process::find_executable(variable("PATH").as_deref(), &["wt.exe"]) {
        // Windows Terminal reads `;` as the start of another command, unless escaped.
        let command = words.map(|word| word.replace(';', "\\;"));
        return Launch::Command {
            program: wt,
            arguments: ["new-tab".into(), "--".into()].into_iter().chain(command).collect(),
        };
    }
    // `start` takes its first quoted argument as the window's title, so it gets an empty one.
    Launch::Command {
        program: PathBuf::from("cmd.exe"),
        arguments: ["/c".into(), "start".into(), "\"\"".into()]
            .into_iter()
            .chain(words)
            .collect(),
    }
}

/// `$TERMINAL`, the convention for a person's own terminal, or the first terminal from [`LINUX_TERMINALS`] on `PATH`.
fn linux_terminal(
    program: &OsStr,
    arguments: &[OsString],
    variable: &impl Fn(&str) -> Option<OsString>,
) -> Result<Launch, String> {
    let command = std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|word| word.to_string_lossy().into_owned());
    let path = variable("PATH");
    let chosen = variable("TERMINAL")
        .filter(|terminal| !terminal.is_empty())
        .and_then(|terminal| {
            let terminal = PathBuf::from(terminal);
            if terminal.is_absolute() {
                Some(terminal)
            } else {
                crate::local::process::find_executable(path.as_deref(), &[&terminal.to_string_lossy()])
            }
        })
        .map(|terminal| (terminal, &["-e"][..]))
        .or_else(|| {
            LINUX_TERMINALS.iter().find_map(|(name, before)| {
                crate::local::process::find_executable(path.as_deref(), &[name]).map(|found| (found, *before))
            })
        });
    let (terminal, before) = chosen.ok_or_else(|| {
        format!(
            "no terminal found; set TERMINAL or install one of {}",
            LINUX_TERMINALS
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    Ok(Launch::Command {
        program: terminal,
        arguments: before.iter().map(|&word| word.to_owned()).chain(command).collect(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::ffi::OsString;

    use super::*;

    fn command(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn terminal_app_types_the_command_as_one_line_of_shell() {
        let launch = terminal_app(
            OsStr::new("/Applications/agent ctl/agentctl"),
            &command(&["attach", "session/s1", "--agent", "it's"]),
        );
        let Launch::Command { program, arguments } = launch else {
            panic!("a command");
        };
        assert_eq!(program, PathBuf::from("osascript"));
        assert_eq!(arguments[0], "-e");
        assert!(
            arguments[1].contains(
                r#"do script "exec '/Applications/agent ctl/agentctl' attach session/s1 --agent 'it'\\''s'""#
            ),
            "{}",
            arguments[1]
        );
    }

    #[test]
    fn applescript_strings_escape_backslashes_and_quotes() {
        assert_eq!(applescript_text(r#"a\b"c"#), r#"a\\b\"c"#);
    }

    #[test]
    fn an_empty_command_is_refused() {
        assert!(in_terminal(&[], |_| None).is_err());
    }

    #[cfg(unix)]
    mod with_path {
        use std::ffi::OsString;

        use super::super::*;

        fn runnable(directory: &std::path::Path, name: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt as _;
            let path = directory.join(name);
            std::fs::write(&path, "").expect("program");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make runnable");
            path
        }

        fn environment(
            directory: &std::path::Path,
            terminal: Option<&'static str>,
        ) -> impl Fn(&str) -> Option<OsString> + use<> {
            let path = directory.as_os_str().to_owned();
            move |name: &str| match name {
                "PATH" => Some(path.clone()),
                "TERMINAL" => terminal.map(OsString::from),
                _ => None,
            }
        }

        #[test]
        fn linux_prefers_the_persons_terminal_then_the_desktops() {
            let directory = tempfile::tempdir().expect("directory");
            let konsole = runnable(directory.path(), "konsole");
            let kgx = runnable(directory.path(), "kgx");
            let alacritty = runnable(directory.path(), "alacritty");
            let program = OsStr::new("agentctl");
            let arguments = [OsString::from("attach")];

            assert_eq!(
                linux_terminal(program, &arguments, &environment(directory.path(), Some("alacritty"))),
                Ok(Launch::Command {
                    program: alacritty,
                    arguments: vec!["-e".into(), "agentctl".into(), "attach".into()],
                })
            );
            assert_eq!(
                linux_terminal(program, &arguments, &environment(directory.path(), None)),
                Ok(Launch::Command {
                    program: kgx,
                    arguments: vec!["--".into(), "agentctl".into(), "attach".into()],
                }),
                "kgx comes before konsole"
            );
            std::fs::remove_file(directory.path().join("kgx")).expect("remove kgx");
            assert_eq!(
                linux_terminal(program, &arguments, &environment(directory.path(), Some("missing"))),
                Ok(Launch::Command {
                    program: konsole,
                    arguments: vec!["-e".into(), "agentctl".into(), "attach".into()],
                }),
                "a TERMINAL that is not installed is passed over"
            );
        }

        #[test]
        fn linux_without_a_terminal_says_what_to_install() {
            let directory = tempfile::tempdir().expect("directory");
            let error = linux_terminal(OsStr::new("agentctl"), &[], &environment(directory.path(), None))
                .expect_err("no terminal");
            assert!(error.contains("TERMINAL") && error.contains("xterm"), "{error}");
        }

        #[test]
        fn windows_uses_windows_terminal_when_installed_or_else_a_console() {
            let directory = tempfile::tempdir().expect("directory");
            let program = OsStr::new(r"C:\agentctl\agentctl.exe");
            let arguments = [OsString::from("--home"), OsString::from(r"C:\a;b")];

            assert_eq!(
                windows_terminal(program, &arguments, &environment(directory.path(), None)),
                Launch::Command {
                    program: PathBuf::from("cmd.exe"),
                    arguments: vec![
                        "/c".into(),
                        "start".into(),
                        "\"\"".into(),
                        r"C:\agentctl\agentctl.exe".into(),
                        "--home".into(),
                        r"C:\a;b".into(),
                    ],
                }
            );
            let wt = runnable(directory.path(), "wt.exe");
            assert_eq!(
                windows_terminal(program, &arguments, &environment(directory.path(), None)),
                Launch::Command {
                    program: wt,
                    arguments: vec![
                        "new-tab".into(),
                        "--".into(),
                        r"C:\agentctl\agentctl.exe".into(),
                        "--home".into(),
                        r"C:\a\;b".into(),
                    ],
                }
            );
        }
    }
}
