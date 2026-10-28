use std::{cell::Cell, rc::Rc, time::Duration};

use sandbox::LocalFuture;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use crate::{
    Agent, Error, control_plane, control_plane::WaitPolicy, harness, progress::AgentProgress, resources::Changes,
    sessions,
};

use super::protocol::{
    CODE_IMMUTABLE, CODE_INTERNAL, CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_NOT_FOUND,
    CODE_PARSE_ERROR, CODE_UPDATING, DirectoryParams, ExecutionEnsureParams, GuestTcpParams, JSON_RPC_VERSION,
    LoginParams, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, METHOD_APPLY, METHOD_AUTH_LOGIN, METHOD_CONVERGE,
    METHOD_DELETE, METHOD_EXECUTION_ENSURE, METHOD_GET, METHOD_GUEST_TCP, METHOD_HEALTH, METHOD_LIST, METHOD_PROGRESS,
    METHOD_RESOLVE_DIRECTORY, METHOD_RESOURCES_WATCH, METHOD_RESTART, METHOD_SESSION_ARCHIVE, METHOD_SESSION_DELETE,
    METHOD_SESSION_ENSURE, METHOD_SESSION_GET, METHOD_SESSION_LIST, METHOD_SESSION_PROMPT, METHOD_SESSION_TURNS,
    METHOD_SESSION_UNARCHIVE, METHOD_SHUTDOWN, METHOD_SSH_ACCESS, METHOD_START, METHOD_STOP, METHOD_STOP_NOW,
    METHOD_VNC_ACCESS, NameParams, PROTOCOL_VERSION, ProgressParams, ReadMessage, Request, ResourcesWatchParams,
    Response, RestartParams, SessionEnsureParams, SessionListParams, SessionParams, SessionPromptParams,
    SessionTurnsParams, ShutdownParams, error_response, read_message,
};
use super::streams::{GuestTcpTarget, Open, Opening, Streams};

/// Quiet period after a change before a progress reply, so a burst of byte
/// progress costs one reply.
const PROGRESS_SETTLE: Duration = Duration::from_millis(50);
/// Quiet period after a resource change before a watch replies, so a burst of
/// changes, such as byte progress during an image pull, costs one reply.
const WATCH_SETTLE: Duration = Duration::from_millis(150);
/// Longest a watch waits without a change; the unchanged reply tells the
/// watcher the daemon is still there.
const WATCH_KEEPALIVE: Duration = Duration::from_secs(30);
/// Most connections switched to streams at once, unless the open-file limit
/// allows fewer. A stream lasts as long as what it carries, such as an SSH
/// session, so streams have a budget of their own rather than taking the
/// connections that calls need.
const MAX_OPEN_STREAMS: usize = 1024;

/// Agent operations exposed through the Control API.
pub trait AgentApi {
    /// Creates or updates desired agent state.
    fn apply(&self, request: control_plane::ApplyRequest) -> LocalFuture<'_, Result<Agent, Error>>;

    /// Gets an agent by name.
    fn get<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<Agent, Error>>;

    /// Lists every active agent.
    fn list(&self) -> LocalFuture<'_, Result<Vec<Agent>, Error>>;

    /// Resolves an agent from its persisted source directory.
    fn resolve_directory<'a>(
        &'a self,
        directory: &'a std::path::Path,
        variant: Option<&'a crate::AgentVariantName>,
    ) -> LocalFuture<'a, Result<Agent, Error>>;

    /// Requests asynchronous deletion.
    fn delete<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>>;

    /// Records whether an agent's sandbox runs; see [`control_plane::ControlPlane::set_run_state`].
    fn set_run_state<'a>(&'a self, name: &'a str, state: crate::RunState) -> LocalFuture<'a, Result<Agent, Error>>;

    /// Stops an agent without letting its guest shut down; see
    /// [`control_plane::ControlPlane::stop_now`].
    fn stop_now<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<Agent, Error>>;

    /// Records a restart of an agent; see [`control_plane::ControlPlane::restart`].
    fn restart<'a>(&'a self, name: &'a str, now: bool) -> LocalFuture<'a, Result<Agent, Error>>;

    /// Reads an agent's stored status and its latest pass's progress, with
    /// only the output after `output` when it names the same pass.
    fn progress<'a>(
        &'a self,
        name: &'a str,
        output: Option<crate::progress::OutputPosition>,
    ) -> LocalFuture<'a, Result<(crate::Status, Option<crate::progress::Provisioning>), Error>>;
}

impl AgentApi for control_plane::ControlPlane {
    fn apply(&self, request: control_plane::ApplyRequest) -> LocalFuture<'_, Result<Agent, Error>> {
        Box::pin(async move { Self::apply(self, request).await })
    }

    fn get<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<Agent, Error>> {
        Box::pin(async move { Self::get(self, name).await })
    }

    fn list(&self) -> LocalFuture<'_, Result<Vec<Agent>, Error>> {
        Box::pin(async move { Self::list(self).await })
    }

    fn resolve_directory<'a>(
        &'a self,
        directory: &'a std::path::Path,
        variant: Option<&'a crate::AgentVariantName>,
    ) -> LocalFuture<'a, Result<Agent, Error>> {
        Box::pin(async move { self.resolve_directory_variant(directory, variant).await })
    }

    fn delete<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { Self::delete(self, name).await })
    }

    fn set_run_state<'a>(&'a self, name: &'a str, state: crate::RunState) -> LocalFuture<'a, Result<Agent, Error>> {
        Box::pin(async move { Self::set_run_state(self, name, state).await })
    }

    fn stop_now<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<Agent, Error>> {
        Box::pin(async move { Self::stop_now(self, name).await })
    }

    fn restart<'a>(&'a self, name: &'a str, now: bool) -> LocalFuture<'a, Result<Agent, Error>> {
        Box::pin(async move { Self::restart(self, name, now).await })
    }

    fn progress<'a>(
        &'a self,
        name: &'a str,
        output: Option<crate::progress::OutputPosition>,
    ) -> LocalFuture<'a, Result<(crate::Status, Option<crate::progress::Provisioning>), Error>> {
        Box::pin(async move { Self::progress(self, name, output).await })
    }
}

/// Host-side authentication operations exposed through the local control API.
pub trait AuthenticationApi {
    /// Stores a credential for one harness; `imported` marks one supplied by the caller
    /// instead of minted by the host login flow.
    fn login<'a>(
        &'a self,
        harness: harness::Harness,
        credential: &'a str,
        imported: bool,
    ) -> LocalFuture<'a, Result<harness::ImportedAuthentication, Error>>;
}

impl AuthenticationApi for harness::AuthenticationManager {
    fn login<'a>(
        &'a self,
        harness: harness::Harness,
        credential: &'a str,
        imported: bool,
    ) -> LocalFuture<'a, Result<harness::ImportedAuthentication, Error>> {
        Box::pin(async move {
            self.login(harness, zeroize::Zeroizing::new(credential.to_owned()), imported)
                .await
        })
    }
}

/// Host-tracked session operations exposed through the local control API.
pub trait SessionApi {
    /// Creates or resolves one named session attach target; see [`sessions::Service::ensure`].
    fn ensure<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        request: sessions::SessionRequest,
        wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<sessions::AttachTarget, Error>>;

    /// Gets one named session scoped to an agent.
    fn get<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
    ) -> LocalFuture<'a, Result<sessions::Session, Error>>;

    /// Lists tracked sessions, optionally scoped to one agent.
    fn list<'a>(&'a self, agent: Option<&'a str>) -> LocalFuture<'a, Result<Vec<sessions::Session>, Error>>;

    /// Delivers a prompt to a running session's harness, optionally waiting for
    /// a completed turn and settled activity; see [`sessions::Service::prompt`].
    fn prompt<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        prompt: &'a str,
        wait: bool,
        timeout: Option<std::time::Duration>,
    ) -> LocalFuture<'a, Result<(), Error>>;

    /// Reads the harness transcript of a session as ordered turns.
    fn turns<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        last: Option<usize>,
    ) -> LocalFuture<'a, Result<Vec<sessions::Turn>, Error>>;

    /// Requests release of one session; see [`sessions::Service::delete`].
    fn delete<'a>(&'a self, agent: &'a str, name: &'a sessions::SessionName) -> LocalFuture<'a, Result<(), Error>>;

    /// Archives or unarchives one session; see [`sessions::Service::set_archived`].
    fn set_archived<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        archived: bool,
    ) -> LocalFuture<'a, Result<sessions::Session, Error>>;

    /// Lists sessions whose work or terminal attachment prevents an upgrade.
    fn upgrade_readiness(&self) -> LocalFuture<'_, Result<sessions::UpgradeReadiness, Error>>;
}

impl SessionApi for sessions::Service {
    fn ensure<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        request: sessions::SessionRequest,
        wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<sessions::AttachTarget, Error>> {
        Box::pin(async move { Self::ensure(self, agent, name, request, wait).await })
    }

    fn prompt<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        prompt: &'a str,
        wait: bool,
        timeout: Option<std::time::Duration>,
    ) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { Self::prompt(self, agent, name, prompt, wait, timeout).await })
    }

    fn turns<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        last: Option<usize>,
    ) -> LocalFuture<'a, Result<Vec<sessions::Turn>, Error>> {
        Box::pin(async move { Self::turns(self, agent, name, last).await })
    }

    fn get<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
    ) -> LocalFuture<'a, Result<sessions::Session, Error>> {
        Box::pin(async move { Self::get(self, agent, name).await })
    }

    fn list<'a>(&'a self, agent: Option<&'a str>) -> LocalFuture<'a, Result<Vec<sessions::Session>, Error>> {
        Box::pin(async move { Self::list(self, agent).await })
    }

    fn set_archived<'a>(
        &'a self,
        agent: &'a str,
        name: &'a sessions::SessionName,
        archived: bool,
    ) -> LocalFuture<'a, Result<sessions::Session, Error>> {
        Box::pin(async move { Self::set_archived(self, agent, name, archived).await })
    }

    fn delete<'a>(&'a self, agent: &'a str, name: &'a sessions::SessionName) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { Self::delete(self, agent, name).await })
    }

    fn upgrade_readiness(&self) -> LocalFuture<'_, Result<sessions::UpgradeReadiness, Error>> {
        Box::pin(Self::upgrade_readiness(self))
    }
}

/// Waiting for an agent to converge, exposed through the local control API.
pub trait ConvergenceApi {
    /// Waits until an agent has its desired run state; see
    /// [`control_plane::Convergence::converge`].
    fn converge<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>>;

    /// Hints convergence of every agent; see [`control_plane::Convergence::wake_all`].
    fn wake_all(&self) -> LocalFuture<'_, Result<(), Error>>;
}

impl ConvergenceApi for control_plane::Convergence {
    fn converge<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { self.converge(name, WaitPolicy::UntilConverged).await.map(drop) })
    }

    fn wake_all(&self) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(self.wake_all())
    }
}

/// Transient agent execution target resolution exposed through the local control API.
pub trait ExecutionApi {
    /// Converges an agent and returns its exact ready sandbox assignment.
    fn ensure<'a>(
        &'a self,
        name: &'a str,
        wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<crate::sandbox::ExecutionTarget, Error>>;
}

impl ExecutionApi for crate::sandbox::ExecutionService {
    fn ensure<'a>(
        &'a self,
        name: &'a str,
        wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<crate::sandbox::ExecutionTarget, Error>> {
        Box::pin(async move { Self::ensure(self, name, wait).await })
    }
}

/// SSH access descriptors exposed through the local control API.
pub trait SshAccessApi {
    /// Describes the SSH access of a named agent.
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::ssh::AccessInfo, Error>>;
}

impl SshAccessApi for crate::ssh::Access {
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::ssh::AccessInfo, Error>> {
        Box::pin(async move { Self::describe(self, name).await })
    }
}

/// VNC access descriptors exposed through the local control API.
pub trait VncAccessApi {
    /// Describes the VNC access of a named agent.
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::vnc::AccessInfo, Error>>;
}

impl VncAccessApi for crate::vnc::Access {
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::vnc::AccessInfo, Error>> {
        Box::pin(async move { Self::describe(self, name).await })
    }
}

/// Connections into agent sandboxes exposed through the local control API.
pub trait GuestTcpApi {
    /// Resolves the named agent to its running sandbox, provided the agent
    /// still has `uid` when one is given. The stream thread dials it.
    fn resolve<'a>(
        &'a self,
        name: &'a str,
        uid: Option<crate::AgentId>,
    ) -> LocalFuture<'a, Result<Box<dyn GuestTcpTarget>, Error>>;
}

impl GuestTcpApi for crate::sandbox::GuestTcp {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
        uid: Option<crate::AgentId>,
    ) -> LocalFuture<'a, Result<Box<dyn GuestTcpTarget>, Error>> {
        Box::pin(async move {
            let target = Self::resolve(self, name, uid).await?;
            Ok(Box::new(target) as Box<dyn GuestTcpTarget>)
        })
    }
}

/// Observes an isolated connection error without terminating the daemon.
pub type ErrorHandler = Rc<dyn Fn(&Error)>;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum LifecycleState {
    #[default]
    Running,
    Checking,
    Draining,
}

#[derive(Default)]
struct Lifecycle {
    state: Cell<LifecycleState>,
    active_mutations: Cell<usize>,
    mutations_idle: Notify,
    shutdown: Notify,
}

impl Lifecycle {
    fn admit_mutation(&self) -> Option<MutationGuard<'_>> {
        if self.state.get() != LifecycleState::Running {
            return None;
        }
        self.active_mutations.set(self.active_mutations.get() + 1);
        Some(MutationGuard { lifecycle: self })
    }

    async fn wait_for_mutations(&self) {
        loop {
            let notified = self.mutations_idle.notified();
            if self.active_mutations.get() == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct MutationGuard<'a> {
    lifecycle: &'a Lifecycle,
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        let remaining = self.lifecycle.active_mutations.get() - 1;
        self.lifecycle.active_mutations.set(remaining);
        if remaining == 0 {
            self.lifecycle.mutations_idle.notify_waiters();
        }
    }
}

struct ShutdownCheck<'a> {
    lifecycle: &'a Lifecycle,
    committed: bool,
}

impl ShutdownCheck<'_> {
    fn commit(mut self) {
        self.lifecycle.state.set(LifecycleState::Draining);
        self.lifecycle.shutdown.notify_waiters();
        self.committed = true;
    }
}

impl Drop for ShutdownCheck<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.lifecycle.state.set(LifecycleState::Running);
        }
    }
}

/// What a call leaves its connection to: a response, or a stream the stream thread opens and answers for.
enum Reply {
    Respond(Response),
    Switch { id: u64, opening: Opening, open: Open },
}

impl From<Response> for Reply {
    fn from(response: Response) -> Self {
        Self::Respond(response)
    }
}

/// Serves the Control API.
pub struct Server {
    agents: Rc<dyn AgentApi>,
    authentication: Rc<dyn AuthenticationApi>,
    convergence: Rc<dyn ConvergenceApi>,
    executions: Rc<dyn ExecutionApi>,
    guest_tcp: Rc<dyn GuestTcpApi>,
    sessions: Rc<dyn SessionApi>,
    ssh: Rc<dyn SshAccessApi>,
    vnc: Rc<dyn VncAccessApi>,
    changes: Changes,
    on_error: ErrorHandler,
    lifecycle: Lifecycle,
    streams: Streams,
    max_open_streams: usize,
}

impl Server {
    /// Creates a Control API server.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "each API the server dispatches to is wired explicitly at the daemon boundary"
    )]
    pub fn new(
        agents: Rc<dyn AgentApi>,
        authentication: Rc<dyn AuthenticationApi>,
        convergence: Rc<dyn ConvergenceApi>,
        executions: Rc<dyn ExecutionApi>,
        guest_tcp: Rc<dyn GuestTcpApi>,
        sessions: Rc<dyn SessionApi>,
        ssh: Rc<dyn SshAccessApi>,
        vnc: Rc<dyn VncAccessApi>,
        changes: Changes,
        on_error: ErrorHandler,
    ) -> Self {
        Self {
            agents,
            authentication,
            convergence,
            executions,
            guest_tcp,
            sessions,
            ssh,
            vnc,
            changes,
            on_error,
            lifecycle: Lifecycle::default(),
            streams: Streams::start(),
            max_open_streams: MAX_OPEN_STREAMS,
        }
    }

    /// Accepts only as many streams as `open_files` descriptors carry, when
    /// that is fewer than usual. A stream holds about two, and half the
    /// descriptors are left for everything else agentd opens, such as calls
    /// and its connections to sandboxes.
    #[must_use]
    pub fn with_open_file_limit(mut self, open_files: Option<u64>) -> Self {
        if let Some(open_files) = open_files {
            self.max_open_streams =
                usize::try_from(open_files / 4).map_or(MAX_OPEN_STREAMS, |streams| streams.min(MAX_OPEN_STREAMS));
        }
        self
    }

    /// Listens on the platform's local socket implementation.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint cannot be secured, bound, or served.
    pub async fn serve_path(self: Rc<Self>, path: &std::path::Path) -> Result<(), Error> {
        super::socket::serve(self, path).await
    }

    /// Serves one JSON object per line until the client closes its stream, or
    /// until a call switches the connection to a stream, which the stream
    /// thread then carries.
    ///
    /// # Errors
    ///
    /// Returns an error when a message is malformed, exceeds the limit, or cannot be read or written,
    /// or when the stream thread has stopped.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut stream = BufReader::new(stream);
        loop {
            if self.is_draining() {
                return Ok(());
            }
            let message = tokio::select! {
                message = read_message(&mut stream, MAX_REQUEST_BYTES) => message?,
                () = self.shutdown_requested() => return Ok(()),
            };
            let line = match message {
                ReadMessage::EndOfStream => return Ok(()),
                ReadMessage::Complete(line) => line,
                ReadMessage::TooLarge => {
                    write_response(
                        stream.get_mut(),
                        &error_response(0, CODE_PARSE_ERROR, "JSON-RPC request exceeds 4 MiB"),
                        None,
                    )
                    .await?;
                    return Err(Error::Invalid("Control API request exceeds 4 MiB".into()));
                }
            };

            let request = match serde_json::from_slice::<Request>(&line) {
                Ok(request) => request,
                Err(error) => {
                    write_response(
                        stream.get_mut(),
                        &error_response(0, CODE_PARSE_ERROR, "invalid JSON-RPC request"),
                        None,
                    )
                    .await?;
                    return Err(Error::Json(error));
                }
            };
            let method = request.method.clone();
            let reply = if ends_with_its_client(&request.method) {
                // A reply that is ready wins over a client that has half-closed.
                tokio::select! {
                    biased;
                    reply = self.handle(request) => reply,
                    () = disconnected(&mut stream) => return Ok(()),
                }
            } else {
                self.handle(request).await
            };
            match reply {
                Reply::Respond(response) => write_response(stream.get_mut(), &response, Some(&method)).await?,
                // The reader may already hold the first bytes of the stream, so it goes along.
                Reply::Switch { id, opening, open } => {
                    return self.streams.carry(id, Box::new(stream), opening, open).await;
                }
            }
        }
    }

    pub(crate) fn report(&self, error: &Error) {
        (self.on_error)(error);
    }

    pub(crate) fn is_draining(&self) -> bool {
        self.lifecycle.state.get() == LifecycleState::Draining
    }

    pub(crate) async fn shutdown_requested(&self) {
        if !self.is_draining() {
            self.lifecycle.shutdown.notified().await;
        }
    }

    async fn handle(&self, request: Request) -> Reply {
        if request.jsonrpc != JSON_RPC_VERSION || request.method.is_empty() {
            return error_response(request.id, CODE_INVALID_REQUEST, "invalid JSON-RPC 2.0 request").into();
        }
        if request.method == METHOD_GUEST_TCP {
            return self.open_guest_tcp(request.id, request.params).await;
        }
        self.respond(request).await.into()
    }

    async fn respond(&self, request: Request) -> Response {
        let _mutation = if is_mutating(&request.method) {
            let Some(mutation) = self.lifecycle.admit_mutation() else {
                return error_response(request.id, CODE_UPDATING, "Agent daemon is preparing for an upgrade");
            };
            Some(mutation)
        } else {
            None
        };
        match request.method.as_str() {
            METHOD_APPLY => self.handle_apply(request.id, request.params).await,
            METHOD_HEALTH => result_response(
                request.id,
                Ok(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "buildVersion": crate::build_version()
                })),
            ),
            METHOD_SHUTDOWN => self.handle_shutdown(request.id, request.params).await,
            METHOD_GET => self.handle_get(request.id, request.params).await,
            METHOD_LIST => result_response(request.id, self.agents.list().await),
            METHOD_PROGRESS => self.handle_progress(request.id, request.params).await,
            METHOD_RESOURCES_WATCH => self.handle_resources_watch(request.id, request.params).await,
            METHOD_RESOLVE_DIRECTORY => self.handle_resolve_directory(request.id, request.params).await,
            METHOD_EXECUTION_ENSURE => self.handle_execution_ensure(request.id, request.params).await,
            METHOD_DELETE => self.handle_delete(request.id, request.params).await,
            METHOD_CONVERGE => self.handle_converge(request.id, request.params).await,
            METHOD_STOP => {
                self.handle_run_state(request.id, request.params, crate::RunState::Stopped)
                    .await
            }
            METHOD_START => {
                self.handle_run_state(request.id, request.params, crate::RunState::Running)
                    .await
            }
            METHOD_STOP_NOW => self.handle_stop_now(request.id, request.params).await,
            METHOD_RESTART => self.handle_restart(request.id, request.params).await,
            METHOD_SSH_ACCESS => self.handle_ssh_access(request.id, request.params).await,
            METHOD_VNC_ACCESS => self.handle_vnc_access(request.id, request.params).await,
            METHOD_AUTH_LOGIN => self.handle_auth_login(request.id, request.params).await,
            METHOD_SESSION_ENSURE => self.handle_session_ensure(request.id, request.params).await,
            METHOD_SESSION_GET => self.handle_session_get(request.id, request.params).await,
            METHOD_SESSION_LIST => self.handle_session_list(request.id, request.params).await,
            METHOD_SESSION_PROMPT => self.handle_session_prompt(request.id, request.params).await,
            METHOD_SESSION_TURNS => self.handle_session_turns(request.id, request.params).await,
            METHOD_SESSION_DELETE => self.handle_session_delete(request.id, request.params).await,
            METHOD_SESSION_ARCHIVE => self.handle_session_archive(request.id, request.params, true).await,
            METHOD_SESSION_UNARCHIVE => self.handle_session_archive(request.id, request.params, false).await,
            _ => error_response(request.id, CODE_METHOD_NOT_FOUND, "method not found"),
        }
    }

    async fn handle_shutdown(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<ShutdownParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "shutdown reason is required");
        };
        if params.reason != "upgrade" {
            return error_response(id, CODE_INVALID_PARAMS, "unsupported shutdown reason");
        }
        if self.lifecycle.state.get() != LifecycleState::Running {
            return error_response(id, CODE_UPDATING, "Agent daemon is already preparing for an upgrade");
        }
        self.lifecycle.state.set(LifecycleState::Checking);
        let check = ShutdownCheck {
            lifecycle: &self.lifecycle,
            committed: false,
        };
        let readiness = tokio::time::timeout(Duration::from_mins(1), async {
            let readiness = self.sessions.upgrade_readiness().await?;
            if !readiness.blockers.is_empty() {
                return Ok(readiness);
            }
            self.lifecycle.wait_for_mutations().await;
            self.sessions.upgrade_readiness().await
        })
        .await;
        match readiness {
            Ok(Ok(mut readiness)) if readiness.blockers.is_empty() => {
                let open = self.streams.open();
                if open > 0 {
                    let connections = if open == 1 { "connection" } else { "connections" };
                    readiness.warnings.push(format!(
                        "{open} open {connections} into agents, such as SSH sessions and forwarded ports, will close"
                    ));
                }
                check.commit();
                self.streams.close();
                result_response(id, Ok(serde_json::json!({"warnings": readiness.warnings})))
            }
            Ok(Ok(readiness)) => error_response(
                id,
                CODE_INVALID_PARAMS,
                format!("active sessions block the upgrade: {}", readiness.blockers.join(", ")),
            ),
            Ok(Err(error)) => result_response::<serde_json::Value>(id, Err(error)),
            Err(_) => error_response(id, CODE_UPDATING, "Agent did not finish preparing for an upgrade"),
        }
    }

    async fn handle_apply(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<control_plane::ApplyRequest>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "invalid apply parameters");
        };
        result_response(id, self.agents.apply(params).await)
    }

    async fn handle_get(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, self.agents.get(&params.name).await)
    }

    async fn handle_resolve_directory(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<DirectoryParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "directory is required");
        };
        result_response(
            id,
            self.agents
                .resolve_directory(&params.directory, params.variant.as_ref())
                .await,
        )
    }

    async fn handle_delete(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(
            id,
            self.agents.delete(&params.name).await.map(|()| serde_json::json!({})),
        )
    }

    /// Waits until the agent has its desired run state, then returns it as
    /// `agents.v1.get` does. Draining ends the wait, so an upgrade is never held
    /// by a waiter; the desired state is stored, so nothing is lost.
    async fn handle_converge(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        let converged = tokio::select! {
            converged = self.convergence.converge(&params.name) => converged,
            () = self.shutdown_requested() => {
                return error_response(
                    id,
                    CODE_UPDATING,
                    "Agent daemon is preparing for an upgrade; run the command again once it is back",
                );
            }
        };
        let agent = match converged {
            Ok(()) => self.agents.get(&params.name).await,
            Err(error) => Err(error),
        };
        result_response(id, agent)
    }

    async fn handle_run_state(&self, id: u64, value: Value, state: crate::RunState) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, self.agents.set_run_state(&params.name, state).await)
    }

    async fn handle_stop_now(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, self.agents.stop_now(&params.name).await)
    }

    async fn handle_restart(&self, id: u64, value: Value) -> Response {
        let Some(params) = serde_json::from_value::<RestartParams>(value)
            .ok()
            .filter(|params| !params.name.is_empty())
        else {
            return error_response(id, CODE_INVALID_PARAMS, "name is required");
        };
        result_response(id, self.agents.restart(&params.name, params.now).await)
    }

    async fn handle_ssh_access(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, self.ssh.describe(&params.name).await)
    }

    /// Long-polls for a change after the caller's revision, then returns the
    /// agent's status and progress. Draining returns at once so an upgrade is
    /// never held by a follower.
    async fn handle_progress(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<ProgressParams>(value) else {
            return error_response(
                id,
                CODE_INVALID_PARAMS,
                "name is required, and after must be a revision",
            );
        };
        tokio::select! {
            _changed = self.changes.changed_since(params.after, PROGRESS_SETTLE, WATCH_KEEPALIVE) => {}
            () = self.shutdown_requested() => {}
        }
        let revision = self.changes.revision();
        let progress = self
            .agents
            .progress(&params.name, params.output)
            .await
            .map(|(status, provisioning)| AgentProgress {
                revision,
                status,
                provisioning,
            });
        result_response(id, progress)
    }

    /// Long-polls for a resource change after the caller's revision, then
    /// returns every agent and session. Draining returns at once so an upgrade
    /// is never held by a watcher.
    async fn handle_resources_watch(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<ResourcesWatchParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "after must be a resource revision");
        };
        tokio::select! {
            _changed = self.changes.changed_since(params.after, WATCH_SETTLE, WATCH_KEEPALIVE) => {}
            () = self.shutdown_requested() => {}
        }
        let revision = self.changes.revision();
        let resources = async {
            Ok(crate::resources::Resources {
                revision,
                agents: self.agents.list().await?,
                sessions: self.sessions.list(None).await?,
            })
        };
        result_response(id, resources.await)
    }

    async fn handle_vnc_access(&self, id: u64, value: Value) -> Response {
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, self.vnc.describe(&params.name).await)
    }

    async fn open_guest_tcp(&self, id: u64, value: Value) -> Reply {
        let Some((name, uid, port)) = serde_json::from_value::<GuestTcpParams>(value)
            .ok()
            .filter(|params| !params.name.is_empty())
            .and_then(|params| Some((params.name, params.uid, std::num::NonZeroU16::new(params.port)?)))
        else {
            return error_response(id, CODE_INVALID_PARAMS, "name and a port from 1 to 65535 are required").into();
        };
        // Dropping the reservation releases it, whether the lookup fails or the call is cancelled.
        let Some(open) = self.streams.reserve(self.max_open_streams) else {
            let most = self.max_open_streams;
            return error_response(
                id,
                CODE_INTERNAL,
                format!("agentd already carries {most} streams, the most it carries at once"),
            )
            .into();
        };
        match self.guest_tcp.resolve(&name, uid).await {
            Ok(target) => Reply::Switch {
                id,
                opening: Opening::GuestTcp { target, port },
                open,
            },
            Err(error) => result_response::<()>(id, Err(error)).into(),
        }
    }

    async fn handle_execution_ensure(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<ExecutionEnsureParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "name is required");
        };
        if params.name.is_empty() {
            return error_response(id, CODE_INVALID_PARAMS, "name is required");
        }
        result_response(
            id,
            self.executions.ensure(&params.name, wait_policy(params.follow)).await,
        )
    }

    async fn handle_auth_login(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<LoginParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "harness and credential are required");
        };
        let stored = self
            .authentication
            .login(params.harness, &params.credential, params.imported)
            .await;
        if stored.is_ok() {
            // An agent may be waiting on this login, such as one whose optional installation of
            // the harness was left out, so it need not wait for its next periodic pass.
            if let Err(error) = self.convergence.wake_all().await {
                (self.on_error)(&error);
            }
        }
        result_response(id, stored)
    }

    async fn handle_session_ensure(&self, id: u64, value: Value) -> Response {
        let params = match serde_json::from_value::<SessionEnsureParams>(value) {
            Ok(params) => params,
            // The selections carry their own validation, so name the decoding failure
            // instead of blaming the two required fields.
            Err(error) => {
                return error_response(
                    id,
                    CODE_INVALID_PARAMS,
                    format!("agent and session name are required, and selections must be valid: {error}"),
                );
            }
        };
        let wait = wait_policy(params.follow);
        let request = sessions::SessionRequest {
            harness: params.harness,
            model_selection: params.model_selection,
            initial_prompt: params.initial_prompt,
        };
        result_response(
            id,
            self.sessions.ensure(&params.agent, &params.name, request, wait).await,
        )
    }

    async fn handle_session_prompt(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<SessionPromptParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "agent, session name and prompt are required");
        };
        result_response(
            id,
            self.sessions
                .prompt(&params.agent, &params.name, &params.prompt, params.wait, params.timeout)
                .await
                .map(|()| serde_json::json!({})),
        )
    }

    async fn handle_session_turns(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<SessionTurnsParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "agent and session name are required");
        };
        result_response(id, self.sessions.turns(&params.agent, &params.name, params.last).await)
    }

    async fn handle_session_get(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<SessionParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "agent and session name are required");
        };
        result_response(id, self.sessions.get(&params.agent, &params.name).await)
    }

    async fn handle_session_delete(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<SessionParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "agent and session name are required");
        };
        result_response(
            id,
            self.sessions
                .delete(&params.agent, &params.name)
                .await
                .map(|()| serde_json::json!({})),
        )
    }

    async fn handle_session_archive(&self, id: u64, value: Value, archived: bool) -> Response {
        let Ok(params) = serde_json::from_value::<SessionParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "agent and session name are required");
        };
        result_response(
            id,
            self.sessions.set_archived(&params.agent, &params.name, archived).await,
        )
    }

    async fn handle_session_list(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<SessionListParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "invalid session list parameters");
        };
        result_response(id, self.sessions.list(params.agent.as_deref()).await)
    }
}

const fn wait_policy(follow: bool) -> WaitPolicy {
    if follow {
        WaitPolicy::UntilConverged
    } else {
        WaitPolicy::FirstPass
    }
}

/// Whether a request waits and may stop when its client disconnects. Each
/// only reads or waits, or, like `sessions.v1.ensure`, its writes are each
/// complete on their own. Every other request runs to completion, so an
/// interrupted command never leaves a change half made. A stream call runs
/// to completion too: its client may end its input right after the request
/// and still want the guest's output, which looks the same as leaving. A
/// client that has really left fails the response or the relay instead,
/// which releases the stream.
fn ends_with_its_client(method: &str) -> bool {
    matches!(
        method,
        METHOD_PROGRESS
            | METHOD_RESOURCES_WATCH
            | METHOD_CONVERGE
            | METHOD_EXECUTION_ENSURE
            | METHOD_SESSION_ENSURE
            | METHOD_SESSION_TURNS
    )
}

/// Completes once the client has closed its end. A client sends nothing while
/// it waits for a reply, so data here is a pipelined request, left for the
/// next read.
async fn disconnected<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) {
    use tokio::io::AsyncBufReadExt as _;
    match reader.fill_buf().await {
        Ok(buffer) if !buffer.is_empty() => std::future::pending().await,
        Ok(_) | Err(_) => {}
    }
}

fn is_mutating(method: &str) -> bool {
    matches!(
        method,
        METHOD_APPLY
            | METHOD_DELETE
            | METHOD_STOP
            | METHOD_STOP_NOW
            | METHOD_RESTART
            | METHOD_START
            | METHOD_EXECUTION_ENSURE
            | METHOD_AUTH_LOGIN
            | METHOD_SESSION_ENSURE
            | METHOD_SESSION_PROMPT
            | METHOD_SESSION_DELETE
            | METHOD_SESSION_ARCHIVE
            | METHOD_SESSION_UNARCHIVE
    )
}

fn name_params(value: Value) -> Result<NameParams, Response> {
    serde_json::from_value::<NameParams>(value)
        .ok()
        .filter(|params| !params.name.is_empty())
        .ok_or_else(|| error_response(0, CODE_INVALID_PARAMS, "name is required"))
}

const fn response_with_id(id: u64, mut response: Response) -> Response {
    response.id = id;
    response
}

pub(super) fn result_response<T: Serialize>(id: u64, result: Result<T, Error>) -> Response {
    match result {
        Ok(value) => serde_json::to_value(value).map_or_else(
            |_| error_response(id, CODE_INTERNAL, "encode response result"),
            |result| Response {
                jsonrpc: JSON_RPC_VERSION.into(),
                id,
                result: Some(result),
                error: None,
            },
        ),
        Err(Error::NotFound) => error_response(id, CODE_NOT_FOUND, Error::NotFound.to_string()),
        Err(Error::Immutable(field)) => error_response(id, CODE_IMMUTABLE, Error::Immutable(field).to_string()),
        Err(Error::Conflict) => error_response(id, CODE_IMMUTABLE, Error::Conflict.to_string()),
        Err(Error::Invalid(message)) => error_response(id, CODE_INVALID_PARAMS, message),
        Err(error @ Error::Stopped(_)) => error_response(id, CODE_INVALID_PARAMS, error.to_string()),
        Err(error) => error_response(id, CODE_INTERNAL, error.to_string()),
    }
}

pub(super) async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
    method: Option<&str>,
) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(response)?;
    // Include the newline in the same budget the client's reader enforces.
    if bytes.len() >= MAX_RESPONSE_BYTES {
        let message = if method == Some(METHOD_SESSION_TURNS) {
            "Session turns response exceeds 64 MiB; retry with --last to request fewer turns"
        } else {
            "Control API response exceeds 64 MiB"
        };
        bytes = serde_json::to_vec(&error_response(response.id, CODE_INTERNAL, message))?;
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
