//! Sandbox operating-system-specific adapters.

pub(crate) mod files;
pub(crate) mod linux;

use ::sandbox::execution::ExecutionSpec;

use crate::Error;

pub use linux::Linux;
pub(crate) use linux::{
    HOME, PLATFORM_API_URL_FILE, PORTABLE_TERMINAL, USER, UTF8_LOCALE, WORKING_DIRECTORY, run_checked,
};

/// Where `agent`'s sessions start, and so where a bind mount of the person's checkout belongs.
///
/// That is the folder its spec names, or else its sandbox OS's working directory, and `None` for
/// an OS without an agent adapter.
#[must_use]
pub fn working_directory(agent: &crate::Agent) -> Option<&str> {
    match (&agent.spec.working_directory, agent.spec.sandbox.platform.os.as_str()) {
        (Some(directory), _) => Some(directory.as_str()),
        (None, "linux") => Some(WORKING_DIRECTORY),
        (None, _) => None,
    }
}

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
