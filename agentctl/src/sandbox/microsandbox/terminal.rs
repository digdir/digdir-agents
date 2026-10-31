//! Terminal transport for the Microsandbox Backend.

use sandbox::{
    provider::SandboxProvider as _,
    terminal::{AttachTerminalRequest, StartTerminalExecutionRequest, StartedTerminalExecution, TerminalAttachOutcome},
};
use sandbox_microsandbox::MicrosandboxProvider;

use crate::{Error, sandbox::Assignment};

/// Attaches the caller's terminal to an Execution in a materialized sandbox.
///
/// # Errors
///
/// Returns an error when the Microsandbox cannot be inspected or attached.
pub async fn attach_terminal(
    home: &std::path::Path,
    assignment: &Assignment,
    request: AttachTerminalRequest,
) -> Result<TerminalAttachOutcome, Error> {
    // TODO: Route attachment through the daemon once it can proxy an interactive terminal stream.
    let provider = MicrosandboxProvider::open(home.join("microsandbox")).await?;
    let Assignment::Materialized { id, .. } = assignment else {
        return Err(Error::Invalid("Session target sandbox is not materialized".into()));
    };
    let sandbox = provider.backend().inspect(id).await?;
    let outcome = provider.backend().attach_terminal(&sandbox.id, request).await?;
    drop(provider);
    Ok(outcome)
}

/// Starts a terminal Execution in a materialized sandbox, for the caller to relay.
///
/// # Errors
///
/// Returns an error when the Microsandbox cannot be inspected or start the Execution.
pub async fn start_terminal_execution(
    home: &std::path::Path,
    assignment: &Assignment,
    request: StartTerminalExecutionRequest,
) -> Result<StartedTerminalExecution, Error> {
    let provider = MicrosandboxProvider::open(home.join("microsandbox")).await?;
    let Assignment::Materialized { id, .. } = assignment else {
        return Err(Error::Invalid("Execution target sandbox is not materialized".into()));
    };
    provider.backend().inspect(id).await?;
    provider
        .backend()
        .start_terminal_execution(id, request)
        .await
        .map_err(Error::from)
}
