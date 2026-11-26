//! Starting a console program in a console window of its own on Windows, reading and writing that window.
//!
//! `std::process::Command` always hands a Windows child the standard handles it is given, and the caller's are
//! either none, as for a GUI, or its own console's, as for `agentctl`. The program would then read and write those
//! instead of its window. `CreateProcessW` without `STARTF_USESTDHANDLES` lets Windows give it the new console's.

/// One command line for `CreateProcessW` from `words`, each in UTF-16, which programs split back into the same words
/// as Microsoft's C runtime does.
///
/// A word with a space, a tab or a `"` in it, or none at all, is put in double quotes. In there a backslash is
/// literal unless it comes before a `"`, so those backslashes, and the ones before the closing quote, are doubled.
#[cfg(any(windows, test))]
fn command_line<W: IntoIterator<Item = u16>>(words: impl IntoIterator<Item = W>) -> Vec<u16> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let mut line = Vec::new();
    for word in words {
        let word = word.into_iter().collect::<Vec<_>>();
        if !line.is_empty() {
            line.push(u16::from(b' '));
        }
        if !word.is_empty() && !word.iter().any(|&unit| b" \t\"".map(u16::from).contains(&unit)) {
            line.extend(word);
            continue;
        }
        line.push(QUOTE);
        let mut backslashes = 0;
        for unit in word {
            if unit == BACKSLASH {
                backslashes += 1;
            } else {
                if unit == QUOTE {
                    line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(unit);
        }
        line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        line.push(QUOTE);
    }
    line
}

/// Starts `program` with `arguments` in a console window of its own, which is its standard input and output, and
/// waits up to `grace` for it to fail.
///
/// `program` is run itself, so it must be a program rather than a script that `cmd` runs.
///
/// # Errors
///
/// Returns why the program could not be started, or the exit code it failed with within `grace`.
#[cfg(windows)]
pub(super) async fn run(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
    grace: std::time::Duration,
) -> Result<(), String> {
    let name = program.to_string_lossy();
    let process = start(program, arguments).map_err(|error| format!("could not run {name}: {error}"))?;
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        match exit_code(&process) {
            Err(error) => return Err(format!("could not wait for {name}: {error}")),
            Ok(Some(0)) => return Ok(()),
            Ok(Some(code)) => return Err(format!("{name} failed (exit code {code})")),
            Ok(None) if tokio::time::Instant::now() >= deadline => return Ok(()),
            Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "std::process::Command can't start a program without handing it standard handles, so only CreateProcessW \
              leaves it the new console's"
)]
fn start(
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
) -> std::io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::{ffi::OsStrExt as _, io::FromRawHandle as _};

    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_CONSOLE, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let application = program.encode_wide().chain([0]).collect::<Vec<u16>>();
    let mut line = command_line(
        std::iter::once(program)
            .chain(arguments.iter().map(std::ffi::OsString::as_os_str))
            .map(|word| word.encode_wide()),
    );
    line.push(0);
    let startup = STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).map_err(std::io::Error::other)?,
        ..STARTUPINFOW::default()
    };
    let mut started = PROCESS_INFORMATION::default();
    // SAFETY: `application` and `line` are NUL-terminated UTF-16 that outlive the call, and `line` is writable as
    // CreateProcessW needs. `startup` is a zeroed STARTUPINFOW with its size set, so no handles are handed over, and
    // `started` is a live PROCESS_INFORMATION for the call to fill. The attributes, environment and directory are
    // null, so the child gets the defaults and inherits no handles.
    let succeeded = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_CONSOLE | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &raw const startup,
            &raw mut started,
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW succeeded, so both handles are open and owned by this process, and each is wrapped once.
    let (process, thread) = unsafe {
        (
            std::os::windows::io::OwnedHandle::from_raw_handle(started.hProcess),
            std::os::windows::io::OwnedHandle::from_raw_handle(started.hThread),
        )
    };
    drop(thread);
    Ok(process)
}

/// The process's exit code once it has exited.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows reports a process's exit code only through GetExitCodeProcess"
)]
fn exit_code(process: &std::os::windows::io::OwnedHandle) -> std::io::Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle as _;

    /// What GetExitCodeProcess reports for a process that is still running.
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0_u32;
    // SAFETY: `process` is an open process handle for the duration of the call, and `code` is a live `u32`.
    let succeeded =
        unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(process.as_raw_handle(), &raw mut code) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((code != STILL_ACTIVE).then_some(code))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::command_line;

    fn line(words: &[&str]) -> String {
        String::from_utf16(&command_line(words.iter().map(|word| word.encode_utf16()))).expect("UTF-16")
    }

    /// The words come back as the C runtime splits a command line: a backslash is literal unless it comes before a
    /// `"`, including the closing one.
    #[test]
    fn words_are_quoted_as_the_c_runtime_splits_them() {
        assert_eq!(
            line(&[r"C:\agentctl\agentctl.exe", "attach", r"C:\R&D;a\.agent"]),
            r"C:\agentctl\agentctl.exe attach C:\R&D;a\.agent",
            "& and ; mean nothing to CreateProcessW"
        );
        assert_eq!(
            line(&[
                r"C:\Program Files\agentctl.exe",
                "",
                r"C:\my agents\",
                r#"say "hi""#,
                r#"a\"b"#
            ]),
            r#""C:\Program Files\agentctl.exe" "" "C:\my agents\\" "say \"hi\"" "a\\\"b""#
        );
    }

    /// Starting a program through [`super::run`] gives it the new console as its standard input and output, which
    /// PowerShell reports through `GetConsoleMode` as not redirected.
    #[cfg(windows)]
    #[tokio::test(flavor = "local")]
    async fn a_console_program_reads_and_writes_its_own_window() {
        let directory = tempfile::tempdir().expect("directory");
        let report = directory.path().join("console report.txt");
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = format!(
            "\"$([Console]::IsInputRedirected) $([Console]::IsOutputRedirected)\" | Set-Content -LiteralPath '{}'",
            report.display()
        );

        super::run(
            powershell.as_os_str(),
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("PowerShell starts in a console of its own");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let reported = loop {
            let reported = std::fs::read_to_string(&report).unwrap_or_default();
            if !reported.trim().is_empty() || std::time::Instant::now() > deadline {
                break reported;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        };
        assert_eq!(
            reported.trim(),
            "False False",
            "standard input and output are the new console, not NUL"
        );
    }
}
