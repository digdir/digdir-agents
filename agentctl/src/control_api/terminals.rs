//! The terminals agentd serves: a session's, or a program of its own in an agent's sandbox.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use sandbox::{
    LocalFuture, SandboxHandle,
    execution::ExecutionId,
    terminal::{StartedTerminalExecution, TerminalSize},
};

use crate::{
    Error,
    sandbox::{ExecutionService, TerminalSpec},
    sessions,
};

use super::{
    relay::{Terminal, TerminalProcess},
    server::TerminalApi,
};

/// The most terminals one agent can have open. Each holds a Microsandbox Execution, of which a sandbox runs a
/// limited number, so a client that leaks terminals runs out here first, with an error that says so.
const MAX_PER_AGENT: usize = 16;

/// The most terminals agentd keeps open in all. Each costs agentd a connection and a relay.
const MAX_TOTAL: usize = 64;

/// Starts terminals through the sessions and executions services, as many as the caps allow.
pub struct Terminals {
    sessions: Rc<sessions::Service>,
    executions: Rc<ExecutionService>,
    open: Rc<Open>,
}

/// The terminals open now, by agent.
#[derive(Default)]
struct Open {
    by_agent: RefCell<HashMap<String, usize>>,
    total: Cell<usize>,
}

impl Open {
    /// Takes a place for a terminal of `agent`, before anything starts.
    fn admit(self: &Rc<Self>, agent: &str) -> Result<Admitted, Error> {
        if self.total.get() >= MAX_TOTAL {
            return Err(Error::Invalid(format!(
                "agentd has {MAX_TOTAL} terminals open, the most it allows. Close one first"
            )));
        }
        let mut by_agent = self.by_agent.borrow_mut();
        let count = by_agent.entry(agent.to_owned()).or_default();
        if *count >= MAX_PER_AGENT {
            return Err(Error::Invalid(format!(
                "Agent {agent:?} has {MAX_PER_AGENT} terminals open, the most agentd allows. Close one first"
            )));
        }
        *count += 1;
        self.total.set(self.total.get() + 1);
        Ok(Admitted {
            open: self.clone(),
            agent: agent.to_owned(),
        })
    }
}

/// One open terminal's place under the caps, given back when dropped.
struct Admitted {
    open: Rc<Open>,
    agent: String,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.open.total.set(self.open.total.get() - 1);
        let mut by_agent = self.open.by_agent.borrow_mut();
        if let Some(count) = by_agent.get_mut(&self.agent) {
            *count -= 1;
            if *count == 0 {
                by_agent.remove(&self.agent);
            }
        }
    }
}

impl Terminals {
    /// Serves terminals of sessions from `sessions` and of programs of their own from `executions`.
    #[must_use]
    pub fn new(sessions: Rc<sessions::Service>, executions: Rc<ExecutionService>) -> Self {
        Self {
            sessions,
            executions,
            open: Rc::default(),
        }
    }
}

impl TerminalApi for Terminals {
    fn attach_session<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        size: TerminalSize,
    ) -> LocalFuture<'a, Result<(sessions::Session, Terminal), Error>> {
        Box::pin(async move {
            let admitted = self.open.admit(agent)?;
            let (session, sandbox, started) = self.sessions.attach(agent, name, size).await?;
            Ok((session, terminal(sandbox, started, admitted)))
        })
    }

    fn open<'a>(
        &'a self,
        agent: &'a str,
        spec: TerminalSpec,
        size: TerminalSize,
    ) -> LocalFuture<'a, Result<Terminal, Error>> {
        Box::pin(async move {
            let admitted = self.open.admit(agent)?;
            let (sandbox, started) = self.executions.open_terminal(agent, spec, size).await?;
            Ok(terminal(sandbox, started, admitted))
        })
    }
}

fn terminal(sandbox: SandboxHandle, started: StartedTerminalExecution, admitted: Admitted) -> Terminal {
    Terminal {
        control: started.control,
        events: started.events,
        process: Box::new(Execution {
            sandbox,
            id: started.id,
            _admitted: admitted,
        }),
    }
}

/// The Execution behind a terminal, and its place under the caps.
struct Execution {
    sandbox: SandboxHandle,
    id: ExecutionId,
    _admitted: Admitted,
}

impl TerminalProcess for Execution {
    fn terminate(&self) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move { Ok(self.sandbox.terminate_execution(&self.id).await?) })
    }

    fn kill(&self) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move { Ok(self.sandbox.kill_execution(&self.id).await?) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_has_at_most_its_share_and_a_closed_terminal_frees_its_place() {
        let open = Rc::new(Open::default());
        let first = open.admit("worker").expect("under the cap");
        let _rest: Vec<_> = (1..MAX_PER_AGENT)
            .map(|_| open.admit("worker").expect("under the cap"))
            .collect();

        let refused = open.admit("worker").err().expect("over the cap");
        assert!(refused.to_string().contains("16 terminals open"), "{refused}");
        let _other = open.admit("reviewer").expect("another agent has its own share");

        drop(first);
        let _again = open.admit("worker").expect("a closed terminal frees its place");
    }

    #[test]
    fn agentd_has_at_most_its_total() {
        let open = Rc::new(Open::default());
        let _admitted: Vec<_> = (0..MAX_TOTAL)
            .map(|index| open.admit(&format!("agent-{}", index % 8)).expect("under the cap"))
            .collect();

        let refused = open.admit("another").err().expect("over the total");
        assert!(refused.to_string().contains("64 terminals open"), "{refused}");
    }
}
