//! Running setup commands in the sandbox.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::Error;

const SETUP_STDERR_LINES: usize = 3;

/// Runs one setup command in the sandbox and fails with what it ran and what it printed.
///
/// # Errors
///
/// Returns an error when the Execution cannot start or exits unsuccessfully.
pub(crate) async fn run_checked<S: AsRef<str>>(
    sandbox: &SandboxHandle,
    executable: &str,
    args: impl IntoIterator<Item = S>,
) -> Result<(), Error> {
    let args: Vec<String> = args.into_iter().map(|arg| arg.as_ref().to_owned()).collect();
    let output = sandbox
        .run_execution(ExecutionSpec::command(SandboxPath::new(executable), args.clone()))
        .await?;
    checked_output(executable, &args, &output)
}

/// Runs `test <test> <path>` in the guest: whether the path exists in the tested form.
///
/// # Errors
///
/// Returns an error when the Execution cannot start or `test` fails for any
/// reason other than the path being absent.
pub(super) async fn path_exists(sandbox: &SandboxHandle, test: &str, path: &str) -> Result<bool, Error> {
    let output = sandbox
        .run_execution(ExecutionSpec::command(
            SandboxPath::new("/usr/bin/test"),
            [test.to_owned(), path.to_owned()],
        ))
        .await?;
    match output.status.code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(Error::SandboxSetup(format!(
            "presence check `test {test} {path}` exited with code {code}"
        ))),
    }
}

pub(super) fn checked_output(
    executable: &str,
    args: &[impl AsRef<str>],
    output: &::sandbox::execution::ExecutionOutput,
) -> Result<(), Error> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let detail = if stderr.is_empty() {
        String::new()
    } else {
        let tail = stderr
            .lines()
            .rev()
            .take(SETUP_STDERR_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        format!(": {tail}")
    };
    Err(Error::SandboxSetup(format!(
        "command `{executable} {}` exited with code {}{detail}",
        args.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" "),
        output.status.code
    )))
}
