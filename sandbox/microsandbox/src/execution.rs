use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

use futures_util::stream;
use microsandbox::sandbox::{AttachOptionsBuilder, ExecOptionsBuilder};
use microsandbox::{ExecControl, ExecEvent};
use sandbox::{Error, LocalFuture, ResourceKind, SandboxId, execution, terminal};

use crate::{
    backend::{MicrosandboxProvider, RuntimeNetwork},
    error,
    state::SandboxRecord,
};

type ExecutionKey = (SandboxId, execution::ExecutionId);
pub(crate) type ExecutionControls = Rc<RefCell<HashMap<ExecutionKey, ExecControl>>>;

// Microsandbox uses -1 when attachment ends before receiving a process exit.
const DETACHED_EXIT_CODE: i32 = -1;
/// The extra CA certificates Node.js loads.
const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";
// TODO: Remove with `execution_environment` once the Microsandbox pin includes
// digdir/microsandbox#4, which points `NODE_EXTRA_CA_CERTS` at this guest copy in the guest init.
/// The copy of the TLS interception CA that Microsandbox installs as a trust anchor in the guest.
/// Microsandbox points `NODE_EXTRA_CA_CERTS` at its own copy on the runtime share instead, which
/// has the host user's ownership and mode and so may be readable only by root in the guest.
const GUEST_INTERCEPTION_CA: &str = "/usr/local/share/ca-certificates/microsandbox-ca.pem";

impl MicrosandboxProvider {
    pub(crate) async fn start_execution_stream(
        &self,
        sandbox_id: &SandboxId,
        request: execution::StartExecutionRequest,
    ) -> Result<execution::StartedExecution, Error> {
        let (execution_id, spec) = request.into_parts();
        let sandbox = self.state.sandbox_by_id(sandbox_id).await?;
        let runtime = self.connect_running(&sandbox).await?;
        let handle = start_runtime_execution(&runtime, &spec, execution_environment(&sandbox, &spec))
            .await
            .map_err(error::microsandbox)?;
        let key = (sandbox_id.clone(), execution_id.clone());
        self.executions.borrow_mut().insert(key.clone(), handle.control());
        let guard = ExecutionGuard {
            controls: Rc::clone(&self.executions),
            key,
        };
        let events = stream::unfold((handle, guard), |(mut handle, guard)| async move {
            handle.recv().await.map(|event| {
                let event = map_event(event);
                (event, (handle, guard))
            })
        });

        Ok(execution::StartedExecution {
            id: execution_id,
            events: Box::pin(events),
        })
    }

    pub(crate) async fn start_terminal_execution_stream(
        &self,
        sandbox_id: &SandboxId,
        request: terminal::StartTerminalExecutionRequest,
    ) -> Result<terminal::StartedTerminalExecution, Error> {
        let (execution_id, spec, initial_size) = request.into_parts();
        let sandbox = self.state.sandbox_by_id(sandbox_id).await?;
        let runtime = self.connect_running(&sandbox).await?;
        let environment = execution_environment(&sandbox, &spec);
        let mut handle = start_runtime_terminal_execution(&runtime, &spec, environment, initial_size)
            .await
            .map_err(error::microsandbox)?;
        let input = handle
            .take_stdin()
            .ok_or_else(|| Error::Backend("Microsandbox terminal Execution did not provide stdin".to_string()))?;
        let runtime_control = handle.control();
        let key = (sandbox_id.clone(), execution_id.clone());
        self.executions
            .borrow_mut()
            .insert(key.clone(), runtime_control.clone());
        let guard = ExecutionGuard {
            controls: Rc::clone(&self.executions),
            key,
        };
        let events = stream::unfold((handle, guard), |(mut handle, guard)| async move {
            handle.recv().await.map(|event| {
                let event = map_terminal_event(event);
                (event, (handle, guard))
            })
        });

        Ok(terminal::StartedTerminalExecution {
            id: execution_id,
            control: Rc::new(MicrosandboxTerminalControl { input, runtime_control }),
            events: Box::pin(events),
        })
    }

    pub(crate) async fn attach_terminal_to_runtime(
        &self,
        sandbox_id: &SandboxId,
        request: terminal::AttachTerminalRequest,
    ) -> Result<terminal::TerminalAttachOutcome, Error> {
        let sandbox = self.state.sandbox_by_id(sandbox_id).await?;
        let runtime = self.connect_running(&sandbox).await?;
        let (spec, detach_keys) = request.into_parts();
        let environment = execution_environment(&sandbox, &spec);
        let exit_code = match spec.program() {
            execution::Program::ImageEntrypoint => {
                runtime
                    .attach_default_with(|options| {
                        apply_attach_options(options, &spec, environment, detach_keys.as_deref())
                    })
                    .await
            }
            execution::Program::Command { executable, args } => {
                runtime
                    .attach_with(executable.as_str(), |options| {
                        apply_attach_options(
                            options.args(args.iter().cloned()),
                            &spec,
                            environment,
                            detach_keys.as_deref(),
                        )
                    })
                    .await
            }
        }
        .map_err(error::microsandbox)?;

        Ok(if exit_code == DETACHED_EXIT_CODE {
            terminal::TerminalAttachOutcome::Detached
        } else {
            terminal::TerminalAttachOutcome::Exited(execution::ExitStatus { code: exit_code })
        })
    }

    pub(crate) async fn control_execution(
        &self,
        sandbox_id: &SandboxId,
        execution_id: &execution::ExecutionId,
        force: bool,
    ) -> Result<(), Error> {
        let key = (sandbox_id.clone(), execution_id.clone());
        let control = self
            .executions
            .borrow()
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::not_found(ResourceKind::Execution, execution_id))?;
        if force {
            control.kill().await
        } else {
            control.signal(15).await
        }
        .map_err(error::microsandbox)
    }
}

/// The environment an Execution starts with on top of the one it inherits from Microsandbox's
/// guest agent.
///
/// When it intercepts TLS, Microsandbox's guest init points `NODE_EXTRA_CA_CERTS` at its copy of
/// the CA on the runtime share, which the guest user may not be able to read. A value the agent
/// declares (not an image `ENV`) already replaces it in every Execution, so only a sandbox whose
/// agent declares none gets the guest's own readable copy here, which also replaces an image `ENV`
/// value. The Execution's own environment still overrides that default.
fn execution_environment(sandbox: &SandboxRecord, spec: &execution::ExecutionSpec) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    // The runtime intercepts TLS, and so installs the guest copy of its CA, only on the Controlled
    // network, the one `create_runtime` enables `.tls()` for.
    let intercepts_tls = matches!(
        RuntimeNetwork::for_attachment(sandbox.network.as_ref()),
        Ok(RuntimeNetwork::Controlled)
    );
    if intercepts_tls && !sandbox.environment.contains_key(NODE_EXTRA_CA_CERTS) {
        environment.insert(NODE_EXTRA_CA_CERTS.to_string(), GUEST_INTERCEPTION_CA.to_string());
    }
    environment.extend(
        spec.environment()
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    environment
}

fn apply_attach_options(
    mut options: AttachOptionsBuilder,
    spec: &execution::ExecutionSpec,
    environment: BTreeMap<String, String>,
    detach_keys: Option<&str>,
) -> AttachOptionsBuilder {
    if let Some(working_directory) = spec.working_directory() {
        options = options.cwd(working_directory.as_str());
    }
    if let Some(detach_keys) = detach_keys {
        options = options.detach_keys(detach_keys);
    }
    options.envs(environment)
}

async fn start_runtime_execution(
    runtime: &microsandbox::Sandbox,
    spec: &execution::ExecutionSpec,
    environment: BTreeMap<String, String>,
) -> microsandbox::MicrosandboxResult<microsandbox::ExecHandle> {
    let working_directory = spec.working_directory().map(|path| path.as_str().to_string());
    match spec.program() {
        execution::Program::ImageEntrypoint => {
            runtime
                .exec_default_stream_with(move |options| configure(options, working_directory, environment))
                .await
        }
        execution::Program::Command { executable, args } => {
            let args = args.clone();
            runtime
                .exec_stream_with(executable.as_str(), move |options| {
                    configure(options.args(args), working_directory, environment)
                })
                .await
        }
    }
}

async fn start_runtime_terminal_execution(
    runtime: &microsandbox::Sandbox,
    spec: &execution::ExecutionSpec,
    environment: BTreeMap<String, String>,
    size: terminal::TerminalSize,
) -> microsandbox::MicrosandboxResult<microsandbox::ExecHandle> {
    let working_directory = spec.working_directory().map(|path| path.as_str().to_string());
    match spec.program() {
        execution::Program::ImageEntrypoint => {
            runtime
                .exec_default_stream_with(move |options| {
                    configure(
                        options
                            .stdin_pipe()
                            .tty(true)
                            .terminal_size(size.rows(), size.columns()),
                        working_directory,
                        environment,
                    )
                })
                .await
        }
        execution::Program::Command { executable, args } => {
            let args = args.clone();
            runtime
                .exec_stream_with(executable.as_str(), move |options| {
                    configure(
                        options
                            .args(args)
                            .stdin_pipe()
                            .tty(true)
                            .terminal_size(size.rows(), size.columns()),
                        working_directory,
                        environment,
                    )
                })
                .await
        }
    }
}

fn configure(
    mut options: ExecOptionsBuilder,
    working_directory: Option<String>,
    environment: BTreeMap<String, String>,
) -> ExecOptionsBuilder {
    if let Some(directory) = working_directory {
        options = options.cwd(directory);
    }
    options.envs(environment)
}

fn map_event(event: ExecEvent) -> Result<execution::ExecutionEvent, Error> {
    Ok(match event {
        ExecEvent::Started { pid } => execution::ExecutionEvent::Started { process_id: Some(pid) },
        ExecEvent::Stdout(bytes) => execution::ExecutionEvent::Stdout(bytes),
        ExecEvent::Stderr(bytes) => execution::ExecutionEvent::Stderr(bytes),
        ExecEvent::Exited { code } => execution::ExecutionEvent::Exited(execution::ExitStatus { code }),
        ExecEvent::Failed(failure) => execution::ExecutionEvent::Failed {
            message: failure.message,
        },
        ExecEvent::StdinError(failure) => {
            return Err(Error::Backend(format!(
                "Microsandbox Execution stdin failed: {failure:?}"
            )));
        }
    })
}

fn map_terminal_event(event: ExecEvent) -> Result<terminal::TerminalEvent, Error> {
    Ok(match event {
        ExecEvent::Started { pid } => terminal::TerminalEvent::Started { process_id: Some(pid) },
        ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes) => terminal::TerminalEvent::Output(bytes),
        ExecEvent::Exited { code } => terminal::TerminalEvent::Exited(execution::ExitStatus { code }),
        ExecEvent::Failed(failure) => terminal::TerminalEvent::Failed {
            message: failure.message,
        },
        ExecEvent::StdinError(failure) => {
            return Err(Error::Backend(format!(
                "Microsandbox terminal Execution stdin failed: {failure:?}"
            )));
        }
    })
}

struct MicrosandboxTerminalControl {
    input: microsandbox::sandbox::exec::ExecSink,
    runtime_control: ExecControl,
}

impl terminal::TerminalControl for MicrosandboxTerminalControl {
    fn write_input(&self, bytes: bytes::Bytes) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            if bytes.is_empty() {
                return Ok(());
            }
            self.input.write(bytes).await.map_err(error::microsandbox)
        })
    }

    fn close_input(&self) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.input.close().await.map_err(error::microsandbox) })
    }

    fn resize(&self, size: terminal::TerminalSize) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.runtime_control
                .resize(size.rows(), size.columns())
                .await
                .map_err(error::microsandbox)
        })
    }
}

struct ExecutionGuard {
    controls: ExecutionControls,
    key: ExecutionKey,
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.controls.borrow_mut().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sandbox::{
        SandboxPath,
        execution::ExecutionSpec,
        network::{NetworkControlProtocolId, NetworkEndpointSelection, PacketMedium},
    };

    use super::{GUEST_INTERCEPTION_CA, NODE_EXTRA_CA_CERTS, execution_environment};
    use crate::{backend::test_records::record_with_network, state::SandboxRecord};

    fn intercepting() -> SandboxRecord {
        record_with_network(
            "00000000-0000-4000-8000-000000000020",
            NetworkEndpointSelection::Control(NetworkControlProtocolId::new(
                microsandbox_network::control::NETWORK_CONTROL_PROTOCOL,
            )),
        )
    }

    fn command() -> ExecutionSpec {
        ExecutionSpec::command(SandboxPath::new("/usr/bin/node"), Vec::<String>::new())
    }

    fn node_extra_ca_certs(environment: &BTreeMap<String, String>) -> Option<&str> {
        environment.get(NODE_EXTRA_CA_CERTS).map(String::as_str)
    }

    #[test]
    fn tls_interception_points_node_at_the_guest_copy_of_the_ca() {
        let environment = execution_environment(&intercepting(), &command());
        assert_eq!(node_extra_ca_certs(&environment), Some(GUEST_INTERCEPTION_CA));
        assert!(!GUEST_INTERCEPTION_CA.starts_with("/.msb/"));
    }

    #[test]
    fn a_sandbox_declared_value_suppresses_the_default() {
        let mut sandbox = intercepting();
        sandbox
            .environment
            .insert(NODE_EXTRA_CA_CERTS.into(), "/etc/own-ca.pem".into());
        assert_eq!(node_extra_ca_certs(&execution_environment(&sandbox, &command())), None);
    }

    #[test]
    fn the_execution_environment_overrides_the_default() {
        let spec = command().with_environment([
            (NODE_EXTRA_CA_CERTS.into(), "/tmp/execution-ca.pem".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]);
        let environment = execution_environment(&intercepting(), &spec);
        assert_eq!(
            environment,
            BTreeMap::from([
                ("LANG".into(), "C.UTF-8".into()),
                (NODE_EXTRA_CA_CERTS.into(), "/tmp/execution-ca.pem".into()),
            ])
        );
    }

    #[test]
    fn without_tls_interception_executions_inherit_the_runtime_value() {
        let mut unattached = intercepting();
        unattached.network = None;
        assert_eq!(
            node_extra_ca_certs(&execution_environment(&unattached, &command())),
            None
        );
        let packets = record_with_network(
            "00000000-0000-4000-8000-000000000021",
            NetworkEndpointSelection::Packet(PacketMedium::Ethernet),
        );
        assert_eq!(node_extra_ca_certs(&execution_environment(&packets, &command())), None);
    }
}
