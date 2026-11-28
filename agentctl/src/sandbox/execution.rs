//! Transient command execution against an agent-owned sandbox.

use std::{collections::BTreeMap, path::Path, rc::Rc};

use ::sandbox::execution;
use serde::{Deserialize, Serialize};

use crate::{Error, control_plane, control_plane::WaitPolicy};

use super::Assignment;

/// Exact materialized sandbox selected after agent convergence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionTarget {
    /// Sticky Provider and immutable sandbox identity.
    pub sandbox: Assignment,
    /// Sandbox operating system used to construct the Execution.
    pub operating_system: String,
}

/// A program to run in a terminal of its own in an agent's sandbox, such as a shell, rather than a session's.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalSpec {
    /// The program and its arguments.
    pub command: Vec<String>,
    /// Variables set on top of the agent's own.
    pub environment: BTreeMap<String, String>,
    /// Where the program starts, when not the agent's default.
    pub working_directory: Option<String>,
}

/// Resolves transient executions without taking ownership of sandbox lifecycle effects.
pub struct ExecutionService {
    agents: Rc<dyn control_plane::AgentStore>,
    convergence: control_plane::Convergence,
    sandboxes: Rc<super::Service>,
}

impl ExecutionService {
    /// Creates an execution-target resolver over the agent controller, opening sandboxes through `sandboxes`.
    #[must_use]
    pub const fn new(
        agents: Rc<dyn control_plane::AgentStore>,
        convergence: control_plane::Convergence,
        sandboxes: Rc<super::Service>,
    ) -> Self {
        Self {
            agents,
            convergence,
            sandboxes,
        }
    }

    /// Starts `spec` in a terminal in the sandbox of the agent `name`, for agentd to relay to a client. The program
    /// gets the environment and folder every command in the sandbox gets, with the spec's own over them. It does not
    /// converge the agent: a client asks for that first, with [`Self::ensure`], to show its progress.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent is missing, deleting, stopped or not Ready, or the terminal cannot start.
    pub async fn open_terminal(
        &self,
        name: &str,
        spec: TerminalSpec,
        size: ::sandbox::terminal::TerminalSize,
    ) -> Result<(::sandbox::SandboxHandle, ::sandbox::terminal::StartedTerminalExecution), Error> {
        let record = self.agents.get_by_name(name).await?;
        if record.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Conflict);
        }
        record.reject_stopped()?;
        let target = target(record.clone(), name)?;
        let mut execution = super::platform::execution_spec(&target.operating_system, &spec.command, true)?
            .with_environment(spec.environment);
        if let Some(directory) = spec.working_directory {
            execution = execution.with_working_directory(::sandbox::SandboxPath::new(directory));
        }
        let sandbox = self.sandboxes.open(&record).await?;
        let terminal = sandbox
            .start_terminal_execution(::sandbox::terminal::StartTerminalExecutionRequest::new(execution, size))
            .await?;
        Ok((sandbox, terminal))
    }

    /// Wakes agent convergence and returns its exact ready sandbox assignment.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent is missing, deleting, or invalid; with
    /// [`WaitPolicy::FirstPass`] also when the single pass fails or leaves the
    /// agent without a ready materialized sandbox.
    pub async fn ensure(&self, name: &str, wait: WaitPolicy) -> Result<ExecutionTarget, Error> {
        // A stopped agent runs nothing, so it is refused before anything is woken.
        let record = self.agents.get_by_name(name).await?;
        if record.agent.metadata.deletion_timestamp.is_some() {
            return Err(Error::Conflict);
        }
        record.reject_stopped()?;
        let record = self.convergence.converge(name, wait).await?;
        record.reject_stopped()?;
        target(record, name)
    }
}

fn target(record: control_plane::AgentRecord, name: &str) -> Result<ExecutionTarget, Error> {
    let ready = record.agent.status.ready_condition();
    if !record.agent.status.is_ready() {
        let detail = ready.map_or_else(
            || "no Ready condition was reported".to_owned(),
            crate::Condition::summary,
        );
        return Err(Error::Invalid(format!("Agent {name:?} is not Ready: {detail}")));
    }
    let sandbox = record
        .agent
        .status
        .sandbox
        .filter(|assignment| assignment.id().is_some())
        .ok_or_else(|| Error::Invalid(format!("Agent {name:?} has no materialized sandbox")))?;
    Ok(ExecutionTarget {
        sandbox,
        operating_system: record.agent.spec.sandbox.platform.os,
    })
}

/// Starts a non-interactive Execution through the recorded sandbox provider.
///
/// The returned stream belongs to the exact sandbox lifecycle ID in `target`;
/// this function does not create, start, or otherwise reconcile a sandbox.
///
/// # Errors
///
/// Returns an error when the Provider is unsupported by this client or the
/// exact sandbox cannot start the Execution.
pub async fn start_execution(
    home: &Path,
    target: &ExecutionTarget,
    request: execution::StartExecutionRequest,
) -> Result<execution::StartedExecution, Error> {
    match target.sandbox.provider().as_str() {
        super::microsandbox::PROVIDER_ID => super::microsandbox::start_execution(home, &target.sandbox, request).await,
        provider => Err(Error::Invalid(format!(
            "command execution is not supported through sandbox provider {provider:?}"
        ))),
    }
}
