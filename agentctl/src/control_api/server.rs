use std::{cell::Cell, rc::Rc, time::Duration};

use sandbox::LocalFuture;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use crate::{
    Agent, Error, accounts::AccountType, control_plane, control_plane::WaitPolicy, harness, progress::AgentProgress,
    resources::Changes, sessions,
};

use super::protocol::{
    AccountLoginParams, AccountLogoutResult, AccountParams, AgentListParams, CODE_IMMUTABLE, CODE_INTERNAL,
    CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_NOT_FOUND, CODE_PARSE_ERROR, CODE_UPDATING,
    DefinitionListParams, DirectoryParams, ExecutionEnsureParams, JSON_RPC_VERSION, LoginParams, LogoutParams,
    LogoutResult, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, METHOD_ACCOUNT_LIST, METHOD_ACCOUNT_LOGIN,
    METHOD_ACCOUNT_LOGOUT, METHOD_APPLY, METHOD_AUTH_LOGIN, METHOD_AUTH_LOGINS, METHOD_AUTH_LOGOUT, METHOD_CONVERGE,
    METHOD_DEFINITIONS, METHOD_DELETE, METHOD_EXECUTION_ENSURE, METHOD_GET, METHOD_HEALTH, METHOD_LIST,
    METHOD_PROGRESS, METHOD_RESOLVE_DIRECTORY, METHOD_RESOURCES_WATCH, METHOD_RESTART, METHOD_RUN,
    METHOD_SESSION_ARCHIVE, METHOD_SESSION_DELETE, METHOD_SESSION_ENSURE, METHOD_SESSION_GET, METHOD_SESSION_LIST,
    METHOD_SESSION_PROMPT, METHOD_SESSION_TURNS, METHOD_SESSION_UNARCHIVE, METHOD_SHUTDOWN, METHOD_SSH_ACCESS,
    METHOD_START, METHOD_STOP, METHOD_STOP_NOW, METHOD_VNC_ACCESS, METHOD_VOLUME_APPLY, METHOD_VOLUME_DELETE,
    METHOD_VOLUME_GET, METHOD_VOLUME_LIST, NameParams, PROTOCOL_VERSION, ProgressParams, ReadMessage, Request,
    ResourcesWatchParams, Response, RestartParams, SessionEnsureParams, SessionListParams, SessionParams,
    SessionPromptParams, SessionTurnsParams, ShutdownParams, error_response, read_message,
};

/// Quiet period after a change before a progress reply, so a burst of byte
/// progress costs one reply.
const PROGRESS_SETTLE: Duration = Duration::from_millis(50);
/// Quiet period after a resource change before a watch replies, so a burst of
/// changes, such as byte progress during an image pull, costs one reply.
const WATCH_SETTLE: Duration = Duration::from_millis(150);
/// Longest a watch waits without a change; the unchanged reply tells the
/// watcher the daemon is still there.
const WATCH_KEEPALIVE: Duration = Duration::from_secs(30);

/// Volume operations exposed through the Control API.
pub trait VolumeApi {
    /// Creates or updates a volume, requesting a new revision when its source changed.
    fn apply(
        &self,
        request: crate::volumes::ApplyVolumeRequest,
    ) -> LocalFuture<'_, Result<crate::volumes::Volume, Error>>;

    /// Gets a volume by name.
    fn get<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::volumes::Volume, Error>>;

    /// Lists every volume.
    fn list(&self) -> LocalFuture<'_, Result<Vec<crate::volumes::Volume>, Error>>;

    /// Requests a volume's deletion.
    fn delete<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>>;
}

impl VolumeApi for crate::volumes::VolumeService {
    fn apply(
        &self,
        request: crate::volumes::ApplyVolumeRequest,
    ) -> LocalFuture<'_, Result<crate::volumes::Volume, Error>> {
        Box::pin(async move { Self::apply(self, request).await })
    }

    fn get<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<crate::volumes::Volume, Error>> {
        Box::pin(async move { Self::get(self, name).await })
    }

    fn list(&self) -> LocalFuture<'_, Result<Vec<crate::volumes::Volume>, Error>> {
        Box::pin(async move { Self::list(self).await })
    }

    fn delete<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { Self::delete(self, name).await })
    }
}

/// Agent operations exposed through the Control API.
pub trait AgentApi {
    /// Creates or updates desired agent state.
    fn apply(&self, request: control_plane::ApplyRequest) -> LocalFuture<'_, Result<Agent, Error>>;

    /// Runs an agent from a definition; see [`control_plane::ControlPlane::run`].
    fn run(
        &self,
        request: control_plane::RunRequest,
    ) -> LocalFuture<'_, Result<(control_plane::Ran, crate::AgentId), Error>>;

    /// Lists the definitions a client can apply or run; see [`control_plane::ControlPlane::definitions`].
    fn definitions(
        &self,
        directory: Option<std::path::PathBuf>,
    ) -> LocalFuture<'_, Result<Vec<control_plane::DefinitionSummary>, Error>>;

    /// Deletes an ephemeral agent whose client is gone; see [`control_plane::ControlPlane::release`].
    fn release(&self, id: crate::AgentId) -> LocalFuture<'_, Result<(), Error>>;

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

    fn run(
        &self,
        request: control_plane::RunRequest,
    ) -> LocalFuture<'_, Result<(control_plane::Ran, crate::AgentId), Error>> {
        Box::pin(async move { Self::run(self, request).await })
    }

    fn definitions(
        &self,
        directory: Option<std::path::PathBuf>,
    ) -> LocalFuture<'_, Result<Vec<control_plane::DefinitionSummary>, Error>> {
        Box::pin(async move { Self::definitions(self, directory).await })
    }

    fn release(&self, id: crate::AgentId) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(async move { Self::release(self, id).await })
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
    /// Stores a credential for one harness as the login `login`; `imported` marks one supplied by
    /// the caller instead of minted by the host login flow.
    fn login<'a>(
        &'a self,
        harness: harness::Harness,
        login: harness::LoginName,
        credential: &'a str,
        imported: bool,
    ) -> LocalFuture<'a, Result<harness::ImportedAuthentication, Error>>;

    /// Forgets one harness's login `login`, returning whether it was stored.
    fn logout<'a>(
        &'a self,
        harness: harness::Harness,
        login: &'a harness::LoginName,
    ) -> LocalFuture<'a, Result<bool, Error>>;

    /// Lists the stored logins of every harness, without their credentials.
    fn logins(&self) -> LocalFuture<'_, Result<Vec<harness::StoredLogin>, Error>>;
}

impl AuthenticationApi for harness::AuthenticationManager {
    fn login<'a>(
        &'a self,
        harness: harness::Harness,
        login: harness::LoginName,
        credential: &'a str,
        imported: bool,
    ) -> LocalFuture<'a, Result<harness::ImportedAuthentication, Error>> {
        Box::pin(async move {
            self.login(harness, login, zeroize::Zeroizing::new(credential.to_owned()), imported)
                .await
        })
    }

    fn logout<'a>(
        &'a self,
        harness: harness::Harness,
        login: &'a harness::LoginName,
    ) -> LocalFuture<'a, Result<bool, Error>> {
        Box::pin(self.logout(harness, login))
    }

    fn logins(&self) -> LocalFuture<'_, Result<Vec<harness::StoredLogin>, Error>> {
        Box::pin(self.logins())
    }
}

/// The logins of the accounts agents act as, exposed through the local control API.
pub trait AccountsApi {
    /// Checks `credential` and keeps it as the login `login` for an account of `account_type`.
    fn login<'a>(
        &'a self,
        account_type: AccountType,
        login: &'a str,
        credential: &'a str,
    ) -> LocalFuture<'a, Result<crate::accounts::Login, Error>>;

    /// Forgets the login `login` for an account of `account_type`, returning it, or `None` when
    /// there was none.
    fn logout<'a>(
        &'a self,
        account_type: AccountType,
        login: &'a str,
    ) -> LocalFuture<'a, Result<Option<crate::accounts::Account>, Error>>;

    /// The kept logins of every type, without their credentials.
    fn list(&self) -> LocalFuture<'_, Result<Vec<crate::accounts::Account>, Error>>;
}

impl AccountsApi for crate::accounts::Accounts {
    fn login<'a>(
        &'a self,
        account_type: AccountType,
        login: &'a str,
        credential: &'a str,
    ) -> LocalFuture<'a, Result<crate::accounts::Login, Error>> {
        Box::pin(self.login(account_type, login, zeroize::Zeroizing::new(credential.to_owned())))
    }

    fn logout<'a>(
        &'a self,
        account_type: AccountType,
        login: &'a str,
    ) -> LocalFuture<'a, Result<Option<crate::accounts::Account>, Error>> {
        Box::pin(self.logout(account_type, login))
    }

    fn list(&self) -> LocalFuture<'_, Result<Vec<crate::accounts::Account>, Error>> {
        Box::pin(self.list())
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
    /// Asks for a reconciliation pass of an Agent without waiting for it; see
    /// [`control_plane::Convergence::wake`].
    fn wake<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>>;
}

impl ConvergenceApi for control_plane::Convergence {
    fn converge<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move { self.converge(name, WaitPolicy::UntilConverged).await.map(drop) })
    }

    fn wake_all(&self) -> LocalFuture<'_, Result<(), Error>> {
        Box::pin(self.wake_all())
    }

    fn wake<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(self.wake(name))
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

/// Serves the Control API.
pub struct Server {
    agents: Rc<dyn AgentApi>,
    authentication: Rc<dyn AuthenticationApi>,
    convergence: Rc<dyn ConvergenceApi>,
    executions: Rc<dyn ExecutionApi>,
    sessions: Rc<dyn SessionApi>,
    ssh: Rc<dyn SshAccessApi>,
    vnc: Rc<dyn VncAccessApi>,
    volumes: Option<Rc<dyn VolumeApi>>,
    accounts: Option<Rc<dyn AccountsApi>>,
    changes: Changes,
    on_error: ErrorHandler,
    lifecycle: Lifecycle,
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
            sessions,
            ssh,
            vnc,
            volumes: None,
            accounts: None,
            changes,
            on_error,
            lifecycle: Lifecycle::default(),
        }
    }

    /// Serves volume operations as well.
    #[must_use]
    pub fn with_volumes(mut self, volumes: Rc<dyn VolumeApi>) -> Self {
        self.volumes = Some(volumes);
        self
    }

    /// Serves the logins of the accounts agents act as as well.
    #[must_use]
    pub fn with_accounts(mut self, accounts: Rc<dyn AccountsApi>) -> Self {
        self.accounts = Some(accounts);
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

    /// Serves one JSON object per line until the client closes its stream.
    ///
    /// # Errors
    ///
    /// Returns an error when a message is malformed, exceeds the limit, or cannot be read or written.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let owned = std::cell::RefCell::new(Vec::new());
        let result = self.serve_requests(stream, &owned).await;
        // The ephemeral agents this connection asked for live as long as it does, however it ends.
        for id in owned.take() {
            if let Err(error) = self.agents.release(id).await {
                tracing::warn!(agent = %id, %error, "deleting an ephemeral agent failed; agentd deletes it when it starts");
            }
        }
        result
    }

    /// Serves one JSON object per line until the client closes its stream, recording in `owned`
    /// the ephemeral agents it asks for.
    async fn serve_requests<S>(&self, stream: S, owned: &std::cell::RefCell<Vec<crate::AgentId>>) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
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
            let response = if ends_with_its_client(&request.method) {
                // A reply that is ready wins over a client that has half-closed.
                tokio::select! {
                    biased;
                    response = self.handle(request, owned) => response,
                    () = disconnected(&mut stream) => return Ok(()),
                }
            } else {
                self.handle(request, owned).await
            };
            write_response(stream.get_mut(), &response, Some(&method)).await?;
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

    async fn handle(&self, request: Request, owned: &std::cell::RefCell<Vec<crate::AgentId>>) -> Response {
        if request.jsonrpc != JSON_RPC_VERSION || request.method.is_empty() {
            return error_response(request.id, CODE_INVALID_REQUEST, "invalid JSON-RPC 2.0 request");
        }
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
            METHOD_RUN => self.handle_run(request.id, request.params, owned).await,
            METHOD_DEFINITIONS => self.handle_definitions(request.id, request.params).await,
            METHOD_HEALTH => result_response(
                request.id,
                Ok(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "buildVersion": crate::build_version()
                })),
            ),
            METHOD_SHUTDOWN => self.handle_shutdown(request.id, request.params).await,
            METHOD_GET => self.handle_get(request.id, request.params).await,
            METHOD_LIST => self.handle_list(request.id, request.params).await,
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
            METHOD_AUTH_LOGOUT => self.handle_auth_logout(request.id, request.params).await,
            METHOD_AUTH_LOGINS => result_response(request.id, self.authentication.logins().await),
            METHOD_ACCOUNT_LOGIN => Box::pin(self.handle_account_login(request.id, request.params)).await,
            METHOD_ACCOUNT_LOGOUT => Box::pin(self.handle_account_logout(request.id, request.params)).await,
            METHOD_ACCOUNT_LIST => match &self.accounts {
                Some(accounts) => result_response(request.id, accounts.list().await),
                None => accounts_unavailable(request.id),
            },
            METHOD_SESSION_ENSURE => self.handle_session_ensure(request.id, request.params).await,
            METHOD_SESSION_GET => self.handle_session_get(request.id, request.params).await,
            METHOD_SESSION_LIST => self.handle_session_list(request.id, request.params).await,
            METHOD_SESSION_PROMPT => self.handle_session_prompt(request.id, request.params).await,
            METHOD_SESSION_TURNS => self.handle_session_turns(request.id, request.params).await,
            METHOD_SESSION_DELETE => self.handle_session_delete(request.id, request.params).await,
            METHOD_SESSION_ARCHIVE => self.handle_session_archive(request.id, request.params, true).await,
            METHOD_SESSION_UNARCHIVE => self.handle_session_archive(request.id, request.params, false).await,
            METHOD_VOLUME_APPLY => self.handle_volume_apply(request.id, request.params).await,
            METHOD_VOLUME_GET => self.handle_volume_get(request.id, request.params).await,
            METHOD_VOLUME_LIST => match &self.volumes {
                Some(volumes) => result_response(request.id, volumes.list().await),
                None => volumes_unavailable(request.id),
            },
            METHOD_VOLUME_DELETE => self.handle_volume_delete(request.id, request.params).await,
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
            Ok(Ok(readiness)) if readiness.blockers.is_empty() => {
                check.commit();
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

    async fn handle_run(&self, id: u64, value: Value, owned: &std::cell::RefCell<Vec<crate::AgentId>>) -> Response {
        let Ok(params) = serde_json::from_value::<control_plane::RunRequest>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "invalid run parameters");
        };
        let result = self.agents.run(params).await.map(|(ran, agent)| {
            if ran.agent.status.is_ephemeral() {
                owned.borrow_mut().push(agent);
            }
            ran
        });
        result_response(id, result)
    }

    async fn handle_volume_apply(&self, id: u64, value: Value) -> Response {
        let Some(volumes) = &self.volumes else {
            return volumes_unavailable(id);
        };
        let Ok(params) = serde_json::from_value::<crate::volumes::ApplyVolumeRequest>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "invalid volume apply parameters");
        };
        result_response(id, volumes.apply(params).await)
    }

    async fn handle_volume_get(&self, id: u64, value: Value) -> Response {
        let Some(volumes) = &self.volumes else {
            return volumes_unavailable(id);
        };
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, volumes.get(&params.name).await)
    }

    async fn handle_volume_delete(&self, id: u64, value: Value) -> Response {
        let Some(volumes) = &self.volumes else {
            return volumes_unavailable(id);
        };
        let params = match name_params(value) {
            Ok(params) => params,
            Err(response) => return response_with_id(id, response),
        };
        result_response(id, volumes.delete(&params.name).await.map(|()| serde_json::json!({})))
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
            let (agents, ephemeral) = listed(self.agents.list().await?, params.include_ephemeral);
            let mut sessions = self.sessions.list(None).await?;
            sessions.retain(|session| !ephemeral.contains(&session.agent));
            Ok(crate::resources::Resources {
                revision,
                agents,
                ephemeral_agents: ephemeral.len(),
                sessions,
                volumes: match &self.volumes {
                    Some(volumes) => volumes.list().await?,
                    None => Vec::new(),
                },
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
            .login(params.harness, params.login, &params.credential, params.imported)
            .await;
        if stored.is_ok() {
            // An agent may be waiting on this login, such as one whose optional installation of
            // the harness was left out, so it need not wait for its next periodic pass.
            self.wake_all().await;
        }
        result_response(id, stored)
    }

    async fn handle_auth_logout(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<LogoutParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "harness and login are required");
        };
        let forgotten = self.authentication.logout(params.harness, &params.login).await;
        if matches!(forgotten, Ok(true)) {
            // The agents using the login lose it now rather than at their next periodic pass.
            self.wake_all().await;
        }
        result_response(id, forgotten.map(|forgotten| LogoutResult { forgotten }))
    }

    async fn wake_all(&self) {
        if let Err(error) = self.convergence.wake_all().await {
            (self.on_error)(&error);
        }
    }

    async fn handle_account_login(&self, id: u64, value: Value) -> Response {
        // The parser's message could quote the credential, so it is never passed on.
        let Ok(params) = serde_json::from_value::<AccountLoginParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "type, login and credential are required");
        };
        let Some(accounts) = &self.accounts else {
            return accounts_unavailable(id);
        };
        let login = accounts
            .login(params.account_type, &params.login, &params.credential)
            .await;
        if let Ok(login) = &login {
            self.wake_account(&login.account()).await;
        }
        result_response(id, login)
    }

    async fn handle_account_logout(&self, id: u64, value: Value) -> Response {
        let Ok(params) = serde_json::from_value::<AccountParams>(value) else {
            return error_response(id, CODE_INVALID_PARAMS, "type and login are required");
        };
        let Some(accounts) = &self.accounts else {
            return accounts_unavailable(id);
        };
        let forgotten = accounts.logout(params.account_type, &params.login).await;
        if let Ok(Some(account)) = &forgotten {
            self.wake_account(account).await;
        }
        result_response(id, forgotten.map(|forgotten| AccountLogoutResult { forgotten }))
    }

    /// Re-converges the agents that ask for `account`, so their setup acts as the account now
    /// kept at once instead of on the controller's next periodic pass.
    async fn wake_account(&self, account: &crate::accounts::Account) {
        let agents = match self.agents.list().await {
            Ok(agents) => agents,
            Err(error) => return self.report(&error),
        };
        for agent in agents {
            if agent
                .spec
                .account(account.account_type())
                .is_some_and(|declared| declared.eq_ignore_ascii_case(account.name()))
                && let Err(error) = self.convergence.wake(&agent.metadata.name).await
            {
                self.report(&error);
            }
        }
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
        let sessions = async {
            let mut sessions = self.sessions.list(params.agent.as_deref()).await?;
            // A named agent's sessions are listed whatever its lifetime, as the agent is shown.
            if params.agent.is_none() {
                let (_, ephemeral) = listed(self.agents.list().await?, params.include_ephemeral);
                sessions.retain(|session| !ephemeral.contains(&session.agent));
            }
            Ok(sessions)
        };
        result_response(id, sessions.await)
    }

    async fn handle_definitions(&self, id: u64, value: Value) -> Response {
        let params = if value.is_null() {
            DefinitionListParams::default()
        } else {
            match serde_json::from_value::<DefinitionListParams>(value) {
                Ok(params) => params,
                Err(_) => return error_response(id, CODE_INVALID_PARAMS, "directory must be a path"),
            }
        };
        result_response(id, self.agents.definitions(params.directory).await)
    }

    async fn handle_list(&self, id: u64, value: Value) -> Response {
        let params = if value.is_null() {
            AgentListParams::default()
        } else {
            match serde_json::from_value::<AgentListParams>(value) {
                Ok(params) => params,
                Err(_) => return error_response(id, CODE_INVALID_PARAMS, "includeEphemeral must be a boolean"),
            }
        };
        let agents = async { Ok(listed(self.agents.list().await?, params.include_ephemeral).0) };
        result_response(id, agents.await)
    }
}

/// `agents` as a listing shows them: without the ephemeral ones unless `include_ephemeral`, as
/// each belongs to the client that ran it, which shows it if it wants to. Also the names of those
/// left out.
fn listed(mut agents: Vec<crate::Agent>, include_ephemeral: bool) -> (Vec<crate::Agent>, Vec<String>) {
    if include_ephemeral {
        return (agents, Vec::new());
    }
    let mut ephemeral = Vec::new();
    agents.retain(|agent| {
        let keep = !agent.status.is_ephemeral();
        if !keep {
            ephemeral.push(agent.metadata.name.clone());
        }
        keep
    });
    (agents, ephemeral)
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
/// interrupted command never leaves a change half made.
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
            | METHOD_RUN
            | METHOD_DELETE
            | METHOD_STOP
            | METHOD_STOP_NOW
            | METHOD_RESTART
            | METHOD_START
            | METHOD_EXECUTION_ENSURE
            | METHOD_AUTH_LOGIN
            | METHOD_AUTH_LOGOUT
            | METHOD_ACCOUNT_LOGIN
            | METHOD_ACCOUNT_LOGOUT
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

fn accounts_unavailable(id: u64) -> Response {
    error_response(id, CODE_METHOD_NOT_FOUND, "this agentd keeps no account logins")
}

fn volumes_unavailable(id: u64) -> Response {
    error_response(id, CODE_METHOD_NOT_FOUND, "this agentd serves no volumes")
}

const fn response_with_id(id: u64, mut response: Response) -> Response {
    response.id = id;
    response
}

fn result_response<T: Serialize>(id: u64, result: Result<T, Error>) -> Response {
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
        Err(error @ Error::VolumeNotFound(_)) => error_response(id, CODE_NOT_FOUND, error.to_string()),
        Err(Error::Immutable(field)) => error_response(id, CODE_IMMUTABLE, Error::Immutable(field).to_string()),
        Err(Error::Conflict) => error_response(id, CODE_IMMUTABLE, Error::Conflict.to_string()),
        Err(Error::Invalid(message)) => error_response(id, CODE_INVALID_PARAMS, message),
        Err(error @ Error::Stopped(_)) => error_response(id, CODE_INVALID_PARAMS, error.to_string()),
        Err(error) => error_response(id, CODE_INTERNAL, error.to_string()),
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
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
