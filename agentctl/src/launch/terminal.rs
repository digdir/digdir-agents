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
/// Returns why no terminal application was found, the command is empty, or the terminal can only be handed the
/// command as text and a word of it is not Unicode.
pub fn in_terminal(command: &[OsString], variable: impl Fn(&str) -> Option<OsString>) -> Result<Launch, String> {
    let (program, arguments) = command.split_first().ok_or("there is no command to run")?;
    if cfg!(target_os = "macos") {
        terminal_app(program, arguments)
    } else if cfg!(windows) {
        windows_terminal(program, arguments, &variable)
    } else {
        linux_terminal(program, arguments, &variable)
    }
}

/// The command's words as text, for a terminal that can only be handed text.
fn text_words<'a>(program: &'a OsStr, arguments: &'a [OsString], terminal: &str) -> Result<Vec<&'a str>, String> {
    std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|word| {
            word.to_str()
                .ok_or_else(|| format!("{terminal} cannot be handed {}, which is not Unicode", word.display()))
        })
        .collect()
}

/// Terminal.app, the terminal every Mac has, which runs a command only as a line of shell for it to type.
fn terminal_app(program: &OsStr, arguments: &[OsString]) -> Result<Launch, String> {
    let line = text_words(program, arguments, "Terminal")?
        .into_iter()
        .map(|word| shell_word(word, CommandShell::Posix))
        .collect::<Vec<_>>()
        .join(" ");
    // `exec` makes the window close with the command instead of returning to a shell.
    let script = format!(
        "tell application \"Terminal\"\n  activate\n  do script \"exec {}\"\nend tell",
        applescript_text(&line)
    );
    Ok(Launch::Command {
        program: PathBuf::from("osascript"),
        arguments: vec!["-e".into(), script.into()],
    })
}

/// `text` inside a string of Apple's script language, which only reads backslashes and double quotes.
fn applescript_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Windows Terminal when it is installed, as it is by default on Windows 11, or else a console window.
fn windows_terminal(
    program: &OsStr,
    arguments: &[OsString],
    variable: &impl Fn(&str) -> Option<OsString>,
) -> Result<Launch, String> {
    if let Some(wt) = crate::local::process::find_executable(variable("PATH").as_deref(), &["wt.exe"]) {
        // Windows Terminal reads `;` as the start of another command, unless escaped. `--window new` opens a window
        // even when its settings put new tabs in one that is open.
        let command = text_words(program, arguments, "Windows Terminal")?
            .into_iter()
            .map(|word| word.replace(';', "\\;").into());
        return Ok(Launch::Command {
            program: wt,
            arguments: ["--window", "new", "new-tab", "--"]
                .into_iter()
                .map(OsString::from)
                .chain(command)
                .collect(),
        });
    }
    // The program gets a console window of its own and its arguments as they are, which `cmd /c start` would read
    // as a line of its own syntax, where `&` in a path starts another command.
    Ok(Launch::Console {
        program: windows_program(program, variable)?,
        arguments: arguments.to_vec(),
    })
}

/// `program` as a path for Windows to start: as given when it names a folder, or else found on `PATH`.
///
/// Windows looks a program up only when it is given a command line without the program's path, and then in the
/// current folder before `PATH`, so a bare name is looked up here instead. Without an extension it is the `.exe`, as
/// Windows would add.
fn windows_program(program: &OsStr, variable: &impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    let text = program.to_string_lossy();
    if text.contains(['\\', '/', ':']) {
        return Ok(program.into());
    }
    let name = program
        .to_str()
        .ok_or_else(|| format!("{text} is not Unicode, so it can't be looked up on PATH"))?;
    let name = if std::path::Path::new(name).extension().is_some() {
        name.to_owned()
    } else {
        format!("{name}.exe")
    };
    crate::local::process::find_executable(variable("PATH").as_deref(), &[&name])
        .ok_or_else(|| format!("{name} not found on PATH"))
}

/// `$TERMINAL`, the convention for a person's own terminal, or the first terminal from [`LINUX_TERMINALS`] on `PATH`.
fn linux_terminal(
    program: &OsStr,
    arguments: &[OsString],
    variable: &impl Fn(&str) -> Option<OsString>,
) -> Result<Launch, String> {
    let path = variable("PATH");
    let chosen = variable("TERMINAL")
        .filter(|terminal| !terminal.is_empty())
        .and_then(|terminal| {
            let terminal = PathBuf::from(terminal);
            if terminal.is_absolute() {
                crate::local::process::runnable(&terminal).then_some(terminal)
            } else {
                crate::local::process::find_executable(path.as_deref(), &[&terminal.to_string_lossy()])
            }
        })
        .map(|terminal| {
            // A terminal the desktops ship with takes the command as it does there. Others take it after `-e`, as
            // most do.
            let name = terminal.file_name().unwrap_or_default();
            let before = LINUX_TERMINALS
                .iter()
                .find(|(known, _)| OsStr::new(known) == name)
                .map_or(&["-e"][..], |(_, before)| *before);
            (terminal, before)
        })
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
        arguments: before
            .iter()
            .map(OsString::from)
            .chain(std::iter::once(program.to_owned()))
            .chain(arguments.iter().cloned())
            .collect(),
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
            &command(&["attach", "session/s1", "--agent", "it's", "--home", "~/agents"]),
        );
        let Ok(Launch::Command { program, arguments }) = launch else {
            panic!("a command");
        };
        assert_eq!(program, PathBuf::from("osascript"));
        assert_eq!(arguments[0], "-e");
        let script = arguments[1].to_string_lossy();
        assert!(
            script.contains(
                r#"do script "exec '/Applications/agent ctl/agentctl' attach session/s1 --agent 'it'\\''s' --home '~/agents'""#
            ),
            "the shell gets every word as given, and no home in place of ~: {script}"
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

    /// A terminal handed the command as text would get a different word in place of one that is not Unicode, so it
    /// is refused instead.
    #[cfg(unix)]
    #[test]
    fn a_text_only_terminal_refuses_a_word_that_is_not_unicode() {
        use std::os::unix::ffi::OsStringExt as _;
        let home = OsString::from_vec(b"/srv/agents-\xff".to_vec());
        let error = terminal_app(OsStr::new("agentctl"), &[OsString::from("--home"), home]).expect_err("not Unicode");
        assert!(error.contains("Terminal") && error.contains("not Unicode"), "{error}");
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

        fn environment(directory: &std::path::Path, terminal: Option<OsString>) -> impl Fn(&str) -> Option<OsString> {
            let path = directory.as_os_str().to_owned();
            move |name: &str| match name {
                "PATH" => Some(path.clone()),
                "TERMINAL" => terminal.clone(),
                _ => None,
            }
        }

        fn launched(program: PathBuf, arguments: &[&str]) -> Launch {
            Launch::Command {
                program,
                arguments: arguments.iter().map(OsString::from).collect(),
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
            let terminal = |name: &str| environment(directory.path(), Some(name.into()));

            assert_eq!(
                linux_terminal(program, &arguments, &terminal("alacritty")).expect("a terminal"),
                launched(alacritty, &["-e", "agentctl", "attach"])
            );
            assert_eq!(
                linux_terminal(program, &arguments, &environment(directory.path(), None)).expect("a terminal"),
                launched(kgx.clone(), &["--", "agentctl", "attach"]),
                "kgx comes before konsole"
            );
            assert_eq!(
                linux_terminal(program, &arguments, &terminal("kgx")).expect("a terminal"),
                launched(kgx, &["--", "agentctl", "attach"]),
                "a TERMINAL the desktops ship with takes the command as it does there"
            );
            std::fs::remove_file(directory.path().join("kgx")).expect("remove kgx");
            assert_eq!(
                linux_terminal(program, &arguments, &terminal("missing")).expect("a terminal"),
                launched(konsole.clone(), &["-e", "agentctl", "attach"]),
                "a TERMINAL that is not installed is passed over"
            );
            let stale = directory.path().join("removed-terminal");
            assert_eq!(
                linux_terminal(program, &arguments, &terminal(&stale.to_string_lossy())).expect("a terminal"),
                launched(konsole.clone(), &["-e", "agentctl", "attach"]),
                "a TERMINAL path that is not there is passed over"
            );
            std::fs::write(&stale, "").expect("a file that may not be run");
            assert_eq!(
                linux_terminal(program, &arguments, &terminal(&stale.to_string_lossy())).expect("a terminal"),
                launched(konsole, &["-e", "agentctl", "attach"]),
                "a TERMINAL path that may not be run is passed over"
            );
        }

        /// The terminal is handed each word as it is, so one that is not Unicode reaches the command unchanged.
        #[test]
        fn linux_hands_the_command_over_as_given() {
            use std::os::unix::ffi::OsStringExt as _;
            let directory = tempfile::tempdir().expect("directory");
            let xterm = runnable(directory.path(), "xterm");
            let home = OsString::from_vec(b"/srv/agents-\xff".to_vec());

            assert_eq!(
                linux_terminal(
                    OsStr::new("agentctl"),
                    &[OsString::from("--home"), home.clone()],
                    &environment(directory.path(), None)
                ),
                Ok(Launch::Command {
                    program: xterm,
                    arguments: vec!["-e".into(), "agentctl".into(), "--home".into(), home],
                })
            );
        }

        #[test]
        fn linux_without_a_terminal_says_what_to_install() {
            let directory = tempfile::tempdir().expect("directory");
            let error = linux_terminal(OsStr::new("agentctl"), &[], &environment(directory.path(), None))
                .expect_err("no terminal");
            assert!(error.contains("TERMINAL") && error.contains("xterm"), "{error}");
        }

        /// Without Windows Terminal the program gets a console of its own and its arguments as they are, so `&` in
        /// a path is part of it rather than the start of another command.
        #[test]
        fn windows_uses_a_new_windows_terminal_window_when_installed_or_else_a_console() {
            let directory = tempfile::tempdir().expect("directory");
            let program = OsStr::new(r"C:\agentctl\agentctl.exe");
            let arguments = [OsString::from("--home"), OsString::from(r"C:\R&D;a\.agent")];

            assert_eq!(
                windows_terminal(program, &arguments, &environment(directory.path(), None)),
                Ok(Launch::Console {
                    program: PathBuf::from(r"C:\agentctl\agentctl.exe"),
                    arguments: arguments.to_vec(),
                })
            );
            runnable(directory.path(), "agentctl.exe");
            assert_eq!(
                windows_terminal(OsStr::new("agentctl"), &arguments, &environment(directory.path(), None)),
                Ok(Launch::Console {
                    program: directory.path().join("agentctl.exe"),
                    arguments: arguments.to_vec(),
                }),
                "a program given by name is found on PATH, as Windows only does from the current folder first"
            );
            assert!(
                windows_terminal(OsStr::new("missing"), &arguments, &environment(directory.path(), None))
                    .is_err_and(|error| error.contains("missing.exe not found on PATH")),
            );
            let wt = runnable(directory.path(), "wt.exe");
            assert_eq!(
                windows_terminal(program, &arguments, &environment(directory.path(), None)).expect("Windows Terminal"),
                launched(
                    wt,
                    &[
                        "--window",
                        "new",
                        "new-tab",
                        "--",
                        r"C:\agentctl\agentctl.exe",
                        "--home",
                        r"C:\R&D\;a\.agent"
                    ]
                )
            );
        }
    }
}
