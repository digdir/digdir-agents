//! Sandbox operating-system-specific adapters.

pub(crate) mod files;
pub(crate) mod linux;

use ::sandbox::execution::ExecutionSpec;

use crate::Error;

pub use linux::Linux;
pub(crate) use linux::{
    HOME, PLATFORM_API_URL_FILE, PORTABLE_TERMINAL, USER, UTF8_LOCALE, WORKING_DIRECTORY, run_checked,
};

/// Builds the agent-conventional Execution environment for one sandbox OS.
///
/// # Errors
///
/// Returns an error when the command is empty or the sandbox operating system
/// has no agent execution adapter.
pub fn execution_spec(os: &str, command: &[String], terminal: bool) -> Result<ExecutionSpec, Error> {
    match os {
        "linux" => linux::execution_spec(command, terminal),
        os => Err(Error::Invalid(format!(
            "command execution is not supported on sandbox operating system {os:?}"
        ))),
    }
}
