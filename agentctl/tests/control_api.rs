#![allow(clippy::expect_used, clippy::panic)]

use crate::support;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use agent::{
    Error,
    control_api::{
        AuthenticationApi, ByteStream, Client, Connection, Connector, ConvergenceApi, ExecutionApi, GuestTcpApi,
        GuestTcpTarget, Server, SessionApi, SshAccessApi, StreamConnection, VncAccessApi,
    },
    control_plane::WaitPolicy,
    control_plane::{ApplyRequest, ControlPlane, memory::InMemoryAgentStore},
    harness::ImportedAuthentication,
    resources::Changes,
};
use sandbox::LocalFuture;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::Notify,
};

use support::{IgnoreNotifications, agent};

struct FakeAuthentication;
/// Converges an agent at once, except `stuck`, which never converges.
/// Counts the hints to converge every agent.
#[derive(Default)]
struct FakeConvergence {
    woken: Rc<Cell<usize>>,
}
struct FakeSshAccess;
struct FakeVncAccess;

impl SshAccessApi for FakeSshAccess {
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<agent::ssh::AccessInfo, Error>> {
        Box::pin(async move {
            if name != "worker" {
                return Err(Error::NotFound);
            }
            Ok(agent::ssh::AccessInfo {
                kind: "ssh".into(),
                agent: name.into(),
                agent_id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
                alias: "agentctl-worker".into(),
                user: "agent".into(),
                identity_file: "/home/me/.agent/ssh/38f41de4-6ff7-4679-ae46-678bc61e4dcb/id_ed25519".into(),
                known_hosts_file: "/home/me/.agent/ssh/known_hosts".into(),
                config_file: "/home/me/.agent/ssh/config".into(),
                proxy_command: "/usr/local/bin/agentctl ssh-proxy agent/worker".into(),
                working_directory: "/home/agent/code".into(),
            })
        })
    }
}

impl VncAccessApi for FakeVncAccess {
    fn describe<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<agent::vnc::AccessInfo, Error>> {
        Box::pin(async move {
            if name != "worker" {
                return Err(Error::NotFound);
            }
            Ok(agent::vnc::AccessInfo {
                kind: "vnc".into(),
                agent: name.into(),
                agent_id: "38f41de4-6ff7-4679-ae46-678bc61e4dcb".parse().expect("Agent ID"),
                guest_port: 5900,
                web_guest_port: Some(6080),
                forward_command: "/usr/local/bin/agentctl port-forward agent/worker :5900".into(),
            })
        })
    }
}
/// Executions of agent `worker`; an ensure for `stuck` waits until dropped.
#[derive(Default)]
struct FakeExecutions {
    waiting: Rc<Cell<usize>>,
}

/// Counts one waiting ensure for as long as it lives.
struct Waiting(Rc<Cell<usize>>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
/// One `sessions.v1.prompt` as the fake saw it: prompt, wait flag, timeout.
type SentMessage = (String, bool, Option<std::time::Duration>);

#[derive(Default)]
struct UpgradeGates {
    prompt: RefCell<Option<Rc<Notify>>>,
    prompt_started: Notify,
    readiness: RefCell<Option<Rc<Notify>>>,
    readiness_started: Notify,
}

struct FakeSessions {
    conversation: Rc<RefCell<Vec<agent::sessions::Turn>>>,
    ensured: Rc<RefCell<Vec<agent::sessions::SessionRequest>>>,
    sent: Rc<RefCell<Vec<SentMessage>>>,
    deleted: Rc<RefCell<Vec<(String, agent::sessions::SessionName)>>>,
    archived: Rc<RefCell<Vec<(String, agent::sessions::SessionName, bool)>>>,
    upgrade_blockers: Rc<RefCell<Vec<String>>>,
    upgrade_warnings: Rc<RefCell<Vec<String>>>,
    upgrade_gates: Rc<UpgradeGates>,
}

fn answered_turn(prompt: &str, answer: &str) -> agent::sessions::Turn {
    agent::sessions::Turn {
        messages: vec![
            agent::sessions::Message {
                role: agent::sessions::Role::User,
                parts: vec![agent::sessions::Part::Text { text: prompt.into() }],
            },
            agent::sessions::Message {
                role: agent::sessions::Role::Assistant,
                parts: vec![
                    agent::sessions::Part::ToolCall {
                        name: "Bash".into(),
                        failed: true,
                    },
                    agent::sessions::Part::Text { text: answer.into() },
                ],
            },
        ],
    }
}

impl AuthenticationApi for FakeAuthentication {
    fn login<'a>(
        &'a self,
        _harness: agent::Harness,
        _token: &'a str,
        _imported: bool,
    ) -> LocalFuture<'a, Result<ImportedAuthentication, Error>> {
        Box::pin(async {
            Ok(ImportedAuthentication {
                provider: "claude".into(),
                ready: true,
            })
        })
    }
}

impl SessionApi for FakeSessions {
    fn ensure<'a>(
        &'a self,
        _agent: &'a str,
        _name: &'a agent::sessions::SessionName,
        request: agent::sessions::SessionRequest,
        _wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<agent::sessions::AttachTarget, Error>> {
        self.ensured.borrow_mut().push(request);
        Box::pin(async { Err(Error::NotFound) })
    }

    fn get<'a>(
        &'a self,
        _agent: &'a str,
        _name: &'a agent::sessions::SessionName,
    ) -> LocalFuture<'a, Result<agent::sessions::Session, Error>> {
        Box::pin(async { Err(Error::NotFound) })
    }

    fn list<'a>(&'a self, _agent: Option<&'a str>) -> LocalFuture<'a, Result<Vec<agent::sessions::Session>, Error>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn prompt<'a>(
        &'a self,
        agent: &'a str,
        _name: &'a agent::sessions::SessionName,
        prompt: &'a str,
        wait: bool,
        timeout: Option<std::time::Duration>,
    ) -> LocalFuture<'a, Result<(), Error>> {
        self.sent.borrow_mut().push((prompt.to_owned(), wait, timeout));
        let gate = self.upgrade_gates.prompt.borrow().clone();
        let upgrade_gates = self.upgrade_gates.clone();
        let blockers = self.upgrade_blockers.clone();
        Box::pin(async move {
            if agent != "worker" {
                return Err(Error::NotFound);
            }
            if let Some(gate) = gate {
                upgrade_gates.prompt_started.notify_one();
                gate.notified().await;
                blockers.borrow_mut().push("session/worker/s1 (working)".into());
            }
            Ok(())
        })
    }

    fn turns<'a>(
        &'a self,
        agent: &'a str,
        _name: &'a agent::sessions::SessionName,
        last: Option<usize>,
    ) -> LocalFuture<'a, Result<Vec<agent::sessions::Turn>, Error>> {
        Box::pin(async move {
            if agent != "worker" {
                return Err(Error::NotFound);
            }
            let mut turns = self.conversation.borrow().clone();
            if let Some(last) = last {
                turns.drain(0..turns.len().saturating_sub(last));
            }
            Ok(turns)
        })
    }

    fn set_archived<'a>(
        &'a self,
        agent: &'a str,
        name: &'a agent::sessions::SessionName,
        archived: bool,
    ) -> LocalFuture<'a, Result<agent::sessions::Session, Error>> {
        self.archived
            .borrow_mut()
            .push((agent.to_owned(), name.clone(), archived));
        Box::pin(async move {
            if agent != "worker" {
                return Err(Error::NotFound);
            }
            let session = serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000001",
                "agentId": "00000000-0000-4000-8000-000000000002",
                "agent": agent,
                "name": name,
                "harness": "claudeCode",
                "createdAt": "2026-09-25T00:00:00Z",
                "archivedAt": archived.then_some("2026-09-25T00:00:01Z"),
            });
            Ok(serde_json::from_value(session).expect("archived session"))
        })
    }

    fn delete<'a>(
        &'a self,
        agent: &'a str,
        name: &'a agent::sessions::SessionName,
    ) -> LocalFuture<'a, Result<(), Error>> {
        self.deleted.borrow_mut().push((agent.to_owned(), name.clone()));
        Box::pin(async move {
            if agent == "worker" {
                Ok(())
            } else {
                Err(Error::NotFound)
            }
        })
    }

    fn upgrade_readiness(&self) -> LocalFuture<'_, Result<agent::sessions::UpgradeReadiness, Error>> {
        let blockers = self.upgrade_blockers.borrow().clone();
        let warnings = self.upgrade_warnings.borrow().clone();
        let gate = self.upgrade_gates.readiness.borrow().clone();
        let upgrade_gates = self.upgrade_gates.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                upgrade_gates.readiness_started.notify_one();
                gate.notified().await;
            }
            Ok(agent::sessions::UpgradeReadiness { blockers, warnings })
        })
    }
}

impl ConvergenceApi for FakeConvergence {
    fn converge<'a>(&'a self, name: &'a str) -> LocalFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            if name == "stuck" {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }

    fn wake_all(&self) -> LocalFuture<'_, Result<(), Error>> {
        self.woken.set(self.woken.get() + 1);
        Box::pin(async { Ok(()) })
    }
}

impl ExecutionApi for FakeExecutions {
    fn ensure<'a>(
        &'a self,
        name: &'a str,
        _wait: WaitPolicy,
    ) -> LocalFuture<'a, Result<agent::sandbox::ExecutionTarget, Error>> {
        Box::pin(async move {
            if name == "stuck" {
                self.waiting.set(self.waiting.get() + 1);
                let _waiting = Waiting(self.waiting.clone());
                std::future::pending::<()>().await;
            }
            if name == "stopped" {
                return Err(Error::Stopped(name.into()));
            }
            if name != "worker" {
                return Err(Error::NotFound);
            }
            Ok(agent::sandbox::ExecutionTarget {
                uid: WORKER_UID.parse().expect("uid"),
                sandbox: agent::sandbox::Assignment::Materialized {
                    provider: agent::sandbox::ProviderId::new("memory")?,
                    id: "ca4e2f21-91d9-43f1-97c6-13f0f350fbe7"
                        .parse()
                        .map_err(|error| Error::Invalid(format!("invalid test sandbox ID: {error}")))?,
                    harnesses: Vec::new(),
                },
                operating_system: "linux".into(),
            })
        })
    }
}

/// Resolves the agent `worker`. Its guest port 22 greets, then answers
/// everything it read once its client has finished, in upper case. Port 7
/// echoes, and every other port refuses the connection.
struct FakeGuestTcp;

const GUEST_GREETING: &[u8] = b"SSH-2.0-fake\r\n";
const SLOW_AGENT: &str = "slow";
const WORKER_UID: &str = "5f0c8a7e-2b1d-4c3e-9f4a-6d7e8f9a0b1c";

impl GuestTcpApi for FakeGuestTcp {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
        uid: Option<agent::AgentId>,
    ) -> LocalFuture<'a, Result<Box<dyn GuestTcpTarget>, Error>> {
        Box::pin(async move {
            if uid.is_some_and(|uid| uid.to_string() != WORKER_UID) {
                return Err(Error::Invalid(format!("agent {name:?} was replaced")));
            }
            // `slow` is `worker` behind a lookup that takes a while, so calls overlap in it.
            if name == SLOW_AGENT {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            match name {
                "worker" | SLOW_AGENT => Ok(Box::new(FakeSandbox) as Box<dyn GuestTcpTarget>),
                "stopped" => Err(Error::Stopped(name.into())),
                _ => Err(Error::NotFound),
            }
        })
    }
}

struct FakeSandbox;

impl GuestTcpTarget for FakeSandbox {
    fn connect(
        self: Box<Self>,
        port: std::num::NonZeroU16,
    ) -> LocalFuture<'static, Result<Box<dyn ByteStream>, Error>> {
        Box::pin(async move {
            match port.get() {
                22 => Ok(Box::new(ShoutingGuest) as Box<dyn ByteStream>),
                7 => Ok(Box::new(EchoGuest) as Box<dyn ByteStream>),
                port => Err(Error::Invalid(format!(
                    "guest TCP connect to 127.0.0.1:{port} failed: connection refused"
                ))),
            }
        })
    }
}

struct ShoutingGuest;

impl ByteStream for ShoutingGuest {
    fn relay(self: Box<Self>, mut connection: Box<dyn StreamConnection>) -> LocalFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            connection.write_all(GUEST_GREETING).await?;
            connection.flush().await?;
            let mut received = Vec::new();
            connection.read_to_end(&mut received).await?;
            connection.write_all(&received.to_ascii_uppercase()).await?;
            connection.shutdown().await?;
            Ok(())
        })
    }
}

struct EchoGuest;

impl ByteStream for EchoGuest {
    fn relay(self: Box<Self>, connection: Box<dyn StreamConnection>) -> LocalFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let (mut reader, mut writer) = tokio::io::split(connection);
            tokio::io::copy(&mut reader, &mut writer).await?;
            Ok(())
        })
    }
}

struct InProcessConnector {
    server: Rc<Server>,
}

struct ScriptedConnector {
    frames: Rc<str>,
}

struct ApiFixture {
    server: Rc<Server>,
    client: Client,
    conversation: Rc<RefCell<Vec<agent::sessions::Turn>>>,
    /// Execution ensures still waiting in the server.
    waiting: Rc<Cell<usize>>,
    /// Hints to converge every agent.
    woken: Rc<Cell<usize>>,
    ensured: Rc<RefCell<Vec<agent::sessions::SessionRequest>>>,
    sent: Rc<RefCell<Vec<SentMessage>>>,
    changes: Changes,
    deleted: Rc<RefCell<Vec<(String, agent::sessions::SessionName)>>>,
    archived: Rc<RefCell<Vec<(String, agent::sessions::SessionName, bool)>>>,
    upgrade_blockers: Rc<RefCell<Vec<String>>>,
    upgrade_warnings: Rc<RefCell<Vec<String>>>,
    upgrade_gates: Rc<UpgradeGates>,
}

impl Connector for InProcessConnector {
    fn connect(&self) -> LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(64 * 1024);
            let api = self.server.clone();
            tokio::task::spawn_local(async move {
                let _ignored = api.serve_connection(server).await;
            });
            Ok(Box::new(client) as Box<dyn Connection>)
        })
    }
}

impl Connector for ScriptedConnector {
    fn connect(&self) -> LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(16 * 1024);
            let frames = self.frames.clone();
            tokio::task::spawn_local(async move {
                let mut server = BufReader::new(server);
                let mut request = String::new();
                server.read_line(&mut request).await.expect("request");
                server.get_mut().write_all(frames.as_bytes()).await.expect("responses");
            });
            Ok(Box::new(client) as Box<dyn Connection>)
        })
    }
}

fn api() -> ApiFixture {
    api_with_open_files(None)
}

fn api_with_open_files(open_files: Option<u64>) -> ApiFixture {
    let control_plane = Rc::new(ControlPlane::new(
        Rc::new(InMemoryAgentStore::new()),
        Rc::new(IgnoreNotifications),
    ));
    let conversation = Rc::new(RefCell::new(vec![answered_turn("one", "1"), answered_turn("two", "2")]));
    let ensured = Rc::new(RefCell::new(Vec::new()));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let deleted = Rc::new(RefCell::new(Vec::new()));
    let archived = Rc::new(RefCell::new(Vec::new()));
    let observed_errors = Rc::new(RefCell::new(Vec::new()));
    let changes = Changes::new();
    let upgrade_blockers = Rc::new(RefCell::new(Vec::new()));
    let upgrade_warnings = Rc::new(RefCell::new(Vec::new()));
    let upgrade_gates = Rc::new(UpgradeGates::default());
    let executions = Rc::new(FakeExecutions::default());
    let waiting = executions.waiting.clone();
    let convergence = FakeConvergence::default();
    let woken = convergence.woken.clone();
    let server = Rc::new(
        Server::new(
            control_plane,
            Rc::new(FakeAuthentication),
            Rc::new(convergence),
            executions,
            Rc::new(FakeGuestTcp),
            Rc::new(FakeSessions {
                conversation: conversation.clone(),
                ensured: ensured.clone(),
                sent: sent.clone(),
                deleted: deleted.clone(),
                archived: archived.clone(),
                upgrade_blockers: upgrade_blockers.clone(),
                upgrade_warnings: upgrade_warnings.clone(),
                upgrade_gates: upgrade_gates.clone(),
            }),
            Rc::new(FakeSshAccess),
            Rc::new(FakeVncAccess),
            changes.clone(),
            Rc::new(move |error| observed_errors.borrow_mut().push(error.to_string())),
        )
        .with_open_file_limit(open_files),
    );
    let client = Client::new(Rc::new(InProcessConnector { server: server.clone() }));
    ApiFixture {
        server,
        client,
        conversation,
        waiting,
        woken,
        ensured,
        sent,
        changes,
        deleted,
        archived,
        upgrade_blockers,
        upgrade_warnings,
        upgrade_gates,
    }
}

struct DelayedConnector {
    inner: InProcessConnector,
}

impl Connector for DelayedConnector {
    fn connect(&self) -> LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.inner.connect().await
        })
    }
}

#[tokio::test(flavor = "local")]
async fn session_operations_carry_their_parameters_across_the_wire() {
    let fixture = api();
    let name = agent::sessions::SessionName::new("s1").expect("name");
    // Time spent connecting must not shorten the completion timeout the server applies.
    let delayed = Client::new(Rc::new(DelayedConnector {
        inner: InProcessConnector {
            server: fixture.server.clone(),
        },
    }));

    delayed
        .prompt_session(
            "worker",
            name.clone(),
            "do it".into(),
            true,
            Some(Duration::from_millis(20)),
        )
        .await
        .expect("send with wait");
    fixture
        .client
        .prompt_session("worker", name.clone(), "fire and forget".into(), false, None)
        .await
        .expect("send without wait");
    assert_eq!(
        fixture.sent.borrow().as_slice(),
        [
            ("do it".into(), true, Some(Duration::from_millis(20))),
            ("fire and forget".into(), false, None)
        ]
    );

    let last = fixture
        .client
        .session_turns("worker", name.clone(), Some(1))
        .await
        .expect("turns");
    assert_eq!(last, [answered_turn("two", "2")]);
    let all = fixture
        .client
        .session_turns("worker", name.clone(), None)
        .await
        .expect("turns");
    assert_eq!(all.len(), 2);

    let archived = fixture
        .client
        .set_session_archived("worker", name.clone(), true)
        .await
        .expect("archive session");
    assert!(archived.is_archived());
    let unarchived = fixture
        .client
        .set_session_archived("worker", name.clone(), false)
        .await
        .expect("unarchive session");
    assert!(!unarchived.is_archived());
    assert_eq!(
        fixture.archived.borrow().as_slice(),
        [
            ("worker".to_owned(), name.clone(), true),
            ("worker".to_owned(), name.clone(), false)
        ]
    );

    fixture
        .client
        .delete_session("worker", name.clone())
        .await
        .expect("delete session");
    assert_eq!(fixture.deleted.borrow().as_slice(), [("worker".to_owned(), name)]);

    let request = agent::sessions::SessionRequest {
        harness: Some(agent::Harness::ClaudeCode),
        model_selection: agent::ModelSelection {
            model: Some(agent::Model::new("claude-fable-5").expect("model")),
            effort: Some(agent::Effort::new("xhigh").expect("effort")),
        },
        initial_prompt: None,
    };
    fixture
        .client
        .ensure_session(
            "worker",
            agent::sessions::SessionName::new("s1").expect("Session name"),
            request.clone(),
            WaitPolicy::FirstPass,
        )
        .await
        .expect_err("fake session ensure should fail after decoding parameters");
    fixture
        .client
        .ensure_session(
            "worker",
            agent::sessions::SessionName::new("s2").expect("Session name"),
            agent::sessions::SessionRequest::default(),
            WaitPolicy::FirstPass,
        )
        .await
        .expect_err("fake session ensure should fail after decoding parameters");
    assert_eq!(
        fixture.ensured.borrow().as_slice(),
        &[request, agent::sessions::SessionRequest::default()],
        "model and effort travel as opaque values and stay absent when omitted"
    );
}

/// An agent whose optional harness waits for this login installs it at once, not at its next pass.
#[tokio::test(flavor = "local")]
async fn a_login_wakes_every_agent() {
    let fixture = api();

    fixture
        .client
        .auth_login(agent::Harness::ClaudeCode, "token".into(), false)
        .await
        .expect("login");

    assert_eq!(fixture.woken.get(), 1);
}

#[tokio::test(flavor = "local")]
async fn turns_responses_above_four_mib_reach_the_client() {
    let fixture = api();
    let answer = "x".repeat(50 * 1024);
    let conversation = (0..100)
        .map(|index| answered_turn(&format!("Question {index}"), &answer))
        .collect::<Vec<_>>();
    fixture.conversation.replace(conversation.clone());

    let received = fixture
        .client
        .session_turns("worker", agent::sessions::SessionName::new("s1").expect("name"), None)
        .await
        .expect("a five MiB conversation fits the response budget");
    assert_eq!(received, conversation);
}

#[tokio::test(flavor = "local")]
async fn oversized_turns_responses_explain_how_to_request_fewer_turns() {
    let fixture = api();
    let latest = answered_turn("latest question", "latest answer");
    // The text fits 64 MiB exactly, but its JSON envelope exceeds the wire budget.
    fixture.conversation.replace(vec![
        answered_turn("large answer", &"x".repeat(64 * 1024 * 1024)),
        latest.clone(),
    ]);
    let name = agent::sessions::SessionName::new("s1").expect("name");

    let error = fixture
        .client
        .session_turns("worker", name.clone(), None)
        .await
        .expect_err("the server returns a bounded error response");
    assert!(
        matches!(error, Error::Rpc(ref error) if error.message == "Session turns response exceeds 64 MiB; retry with --last to request fewer turns"),
        "unexpected error: {error}"
    );
    let received = fixture
        .client
        .session_turns("worker", name, Some(1))
        .await
        .expect("requesting fewer turns succeeds");
    assert_eq!(received, [latest]);
    fixture
        .client
        .health()
        .await
        .expect("the daemon still answers requests");
}

#[tokio::test(flavor = "local")]
async fn requests_above_four_mib_are_still_rejected() {
    let fixture = api();
    let mut connection = InProcessConnector {
        server: fixture.server.clone(),
    }
    .connect()
    .await
    .expect("connection");
    // Fill the request budget without a newline, then cross it by one byte.
    connection
        .write_all(&vec![b' '; 4 * 1024 * 1024])
        .await
        .expect("request prefix");
    connection
        .write_all(b" \n")
        .await
        .expect("last request byte and delimiter");
    let mut response = String::new();
    BufReader::new(connection)
        .read_line(&mut response)
        .await
        .expect("error response");
    let response: serde_json::Value = serde_json::from_str(&response).expect("JSON-RPC error");
    assert_eq!(response["error"]["message"], "JSON-RPC request exceeds 4 MiB");
    fixture
        .client
        .health()
        .await
        .expect("the daemon still answers requests");
}

#[tokio::test(flavor = "local")]
async fn client_reports_responses_above_sixty_four_mib_from_a_peer() {
    let client = Client::new(Rc::new(ScriptedConnector {
        frames: " ".repeat(64 * 1024 * 1024 + 1).into(),
    }));
    let error = client.health().await.expect_err("oversized peer response");
    assert!(
        matches!(error, Error::Invalid(ref message) if message == "Control API response exceeds 64 MiB"),
        "unexpected error: {error}"
    );
}

#[tokio::test(flavor = "local")]
async fn a_wait_ends_when_its_client_goes_away() {
    let fixture = api();
    let interrupted = tokio::time::timeout(
        Duration::from_millis(100),
        fixture.client.ensure_execution("stuck", WaitPolicy::UntilConverged),
    )
    .await;
    assert!(interrupted.is_err(), "the wait never ends on its own");

    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.waiting.get() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the server stops waiting once its client has gone");
}

#[tokio::test(flavor = "local")]
async fn an_interrupted_prompt_is_still_delivered() {
    let fixture = api();
    let gate = Rc::new(Notify::new());
    fixture.upgrade_gates.prompt.replace(Some(gate.clone()));
    let interrupted = tokio::time::timeout(Duration::from_millis(100), async {
        let prompting = fixture.client.prompt_session(
            "worker",
            agent::sessions::SessionName::new("s1").expect("name"),
            "go".into(),
            false,
            None,
        );
        tokio::select! {
            result = prompting => result,
            () = async {
                fixture.upgrade_gates.prompt_started.notified().await;
                std::future::pending::<()>().await;
            } => unreachable!(),
        }
    })
    .await;
    assert!(interrupted.is_err(), "the delivery is still in progress");

    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.upgrade_blockers.borrow().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the delivery runs to completion without its client");
}

#[tokio::test(flavor = "local")]
async fn health_reports_a_compatible_daemon() {
    let fixture = api();
    let daemon = fixture.client.require_compatible_daemon().await.expect("health check");
    assert_eq!(daemon.protocol_version.as_deref(), Some("v5"));
    assert_eq!(daemon.build_version.as_deref(), Some(agent::build_version()));
}

#[test]
fn daemon_identity_rejects_preview_1_and_mixed_builds() {
    let extended: agent::control_api::DaemonInfo = serde_json::from_value(serde_json::json!({
        "protocolVersion": "v5",
        "buildVersion": agent::build_version(),
        "futureCapability": true
    }))
    .expect("extended health response");
    extended.require_compatible().expect("compatible extended response");

    for daemon in [
        agent::control_api::DaemonInfo {
            protocol_version: Some("v1".into()),
            build_version: None,
        },
        agent::control_api::DaemonInfo {
            protocol_version: Some("v5".into()),
            build_version: Some("another-build".into()),
        },
    ] {
        let error = daemon.require_compatible().expect_err("incompatible daemon");
        assert!(error.to_string().contains("running agentd is incompatible"));
    }
}

#[tokio::test(flavor = "local")]
async fn shutdown_reports_blocking_sessions_without_draining() {
    let fixture = api();
    fixture
        .upgrade_blockers
        .borrow_mut()
        .push("session/worker/busy (working)".into());
    let error = fixture
        .client
        .shutdown_for_upgrade()
        .await
        .expect_err("working session blocks shutdown");
    assert!(matches!(error, Error::Rpc(error) if error.is_invalid_params()));
    fixture.client.health().await.expect("daemon remains available");
}

#[tokio::test(flavor = "local")]
async fn shutdown_returns_nonblocking_session_warnings() {
    let fixture = api();
    fixture
        .upgrade_warnings
        .borrow_mut()
        .push("session/worker/fresh will start a new conversation".into());

    assert_eq!(
        fixture.client.shutdown_for_upgrade().await.expect("shutdown"),
        ["session/worker/fresh will start a new conversation"]
    );
}

#[tokio::test(flavor = "local")]
async fn shutdown_waits_for_admitted_mutations_before_checking_sessions() {
    let fixture = api();
    let gate = Rc::new(Notify::new());
    *fixture.upgrade_gates.prompt.borrow_mut() = Some(gate.clone());
    let prompt_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let shutdown_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let started = fixture.upgrade_gates.prompt_started.notified();
    let prompt = tokio::task::spawn_local(async move {
        prompt_client
            .prompt_session(
                "worker",
                agent::sessions::SessionName::new("s1").expect("name"),
                "start work".into(),
                false,
                None,
            )
            .await
    });
    started.await;

    let shutdown = tokio::task::spawn_local(async move { shutdown_client.shutdown_for_upgrade().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished(), "shutdown passed the pending prompt");

    gate.notify_one();
    prompt.await.expect("prompt task").expect("prompt response");
    let error = shutdown
        .await
        .expect("shutdown task")
        .expect_err("new work blocks shutdown");
    assert!(matches!(error, Error::Rpc(error) if error.is_invalid_params()));
    fixture
        .client
        .health()
        .await
        .expect("rejected shutdown restores admission");
}

#[tokio::test(flavor = "local")]
async fn shutdown_rejects_reported_work_before_waiting_for_admitted_mutations() {
    let fixture = api();
    let gate = Rc::new(Notify::new());
    *fixture.upgrade_gates.prompt.borrow_mut() = Some(gate.clone());
    let prompt_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let shutdown_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let started = fixture.upgrade_gates.prompt_started.notified();
    let prompt = tokio::task::spawn_local(async move {
        prompt_client
            .prompt_session(
                "worker",
                agent::sessions::SessionName::new("s1").expect("name"),
                "continue work".into(),
                true,
                Some(Duration::from_secs(10)),
            )
            .await
    });
    started.await;
    fixture
        .upgrade_blockers
        .borrow_mut()
        .push("session/worker/s1 (working)".into());

    let error = tokio::time::timeout(Duration::from_secs(1), shutdown_client.shutdown_for_upgrade())
        .await
        .expect("shutdown should inspect reported work without draining the prompt")
        .expect_err("reported work blocks shutdown");
    assert!(matches!(error, Error::Rpc(error) if error.is_invalid_params()));
    assert!(
        !prompt.is_finished(),
        "rejected shutdown must not wait for the active prompt"
    );
    fixture.client.health().await.expect("daemon remains available");

    gate.notify_one();
    prompt.await.expect("prompt task").expect("prompt response");
}

#[tokio::test(flavor = "local", start_paused = true)]
async fn shutdown_preparation_has_one_deadline_and_restores_admission() {
    let fixture = api();
    let prompt_gate = Rc::new(Notify::new());
    let readiness_gate = Rc::new(Notify::new());
    *fixture.upgrade_gates.prompt.borrow_mut() = Some(prompt_gate.clone());
    *fixture.upgrade_gates.readiness.borrow_mut() = Some(readiness_gate);
    let prompt_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let shutdown_client = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let prompt_started = fixture.upgrade_gates.prompt_started.notified();
    let prompt = tokio::task::spawn_local(async move {
        prompt_client
            .prompt_session(
                "worker",
                agent::sessions::SessionName::new("s1").expect("name"),
                "start work".into(),
                false,
                None,
            )
            .await
    });
    prompt_started.await;

    let shutdown = tokio::task::spawn_local(async move { shutdown_client.shutdown_for_upgrade().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(59)).await;
    prompt_gate.notify_one();
    prompt.await.expect("prompt task").expect("prompt response");
    fixture.upgrade_gates.readiness_started.notified().await;
    tokio::time::advance(Duration::from_secs(2)).await;

    let error = shutdown
        .await
        .expect("shutdown task")
        .expect_err("preparation exceeds its shared deadline");
    assert!(error.to_string().contains("did not finish preparing"));
    *fixture.upgrade_gates.prompt.borrow_mut() = None;
    fixture
        .client
        .prompt_session(
            "worker",
            agent::sessions::SessionName::new("s2").expect("name"),
            "still admitted".into(),
            false,
            None,
        )
        .await
        .expect("timed out shutdown restores admission");
}

fn request(name: &str) -> ApplyRequest {
    ApplyRequest {
        source_directory: std::env::temp_dir().join("agent-platform-source"),
        manifest_path: None,
        env_file: None,
        create_only: false,
        agent: agent(name),
    }
}

#[tokio::test(flavor = "local")]
async fn client_and_server_exchange_versioned_agent_operations() {
    let fixture = api();
    let client = &fixture.client;
    let applied = client.apply(request("worker")).await.expect("apply");
    let fetched = client.get("worker").await.expect("get");
    assert_eq!(applied, fetched);
    assert_eq!(client.list_agents().await.expect("list"), vec![applied.clone()]);
    assert_eq!(
        client
            .resolve_agent(request("worker").source_directory.join("nested"))
            .await
            .expect("resolve source"),
        applied
    );
    let execution = client
        .ensure_execution("worker", WaitPolicy::FirstPass)
        .await
        .expect("execution target");
    assert_eq!(execution.operating_system, "linux");
    assert_eq!(execution.sandbox.provider().as_str(), "memory");
    assert_eq!(client.list_sessions(None).await.expect("list all sessions"), []);
    let session_error = client
        .get_session("worker", agent::sessions::SessionName::new("s1").expect("Session name"))
        .await
        .expect_err("missing session");
    assert!(matches!(session_error, Error::Rpc(error) if error.code == -32004));

    client.delete("worker").await.expect("delete request");
    let deleting = client.get("worker").await.expect("marked resource");
    assert!(deleting.metadata.deletion_timestamp.is_some());
}

#[tokio::test(flavor = "local")]
async fn resource_watch_returns_current_state_then_waits_for_the_next_change() {
    let fixture = api();
    let initial = fixture.client.watch_resources(None).await.expect("initial state");
    assert_eq!(initial.agents, []);
    assert_eq!(initial.sessions, []);

    let watcher = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let revision = initial.revision;
    let watch = tokio::task::spawn_local(async move { watcher.watch_resources(Some(revision)).await });
    tokio::task::yield_now().await;
    assert!(!watch.is_finished(), "a current revision waits for a change");

    let applied = fixture.client.apply(request("worker")).await.expect("apply");
    fixture.changes.bump();
    let changed = watch.await.expect("watch task").expect("changed state");
    assert_ne!(changed.revision, revision);
    assert_eq!(changed.agents, vec![applied]);
}

#[tokio::test(flavor = "local", start_paused = true)]
async fn resource_watch_replies_unchanged_after_the_keepalive() {
    let fixture = api();
    let current = fixture.client.watch_resources(None).await.expect("initial state");
    let started = tokio::time::Instant::now();
    let unchanged = fixture
        .client
        .watch_resources(Some(current.revision))
        .await
        .expect("keepalive state");
    assert_eq!(unchanged, current);
    assert_eq!(started.elapsed(), Duration::from_secs(30));
}

#[tokio::test(flavor = "local")]
async fn resource_watch_neither_holds_nor_outlives_an_upgrade_drain() {
    let fixture = api();
    let current = fixture.client.watch_resources(None).await.expect("initial state");
    let watcher = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let watch = tokio::task::spawn_local(async move { watcher.watch_resources(Some(current.revision)).await });
    tokio::task::yield_now().await;

    fixture
        .client
        .shutdown_for_upgrade()
        .await
        .expect("a pending watch is not an admitted mutation");
    let released = watch.await.expect("watch task").expect("state on drain");
    assert_eq!(released.revision, current.revision);
}

#[tokio::test(flavor = "local")]
async fn a_converge_wait_ends_with_an_upgrade_drain() {
    let fixture = api();
    let waiter = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let wait = tokio::task::spawn_local(async move { waiter.converge("stuck").await });
    tokio::task::yield_now().await;

    fixture
        .client
        .shutdown_for_upgrade()
        .await
        .expect("a pending wait is not an admitted mutation");
    let error = wait.await.expect("wait task").expect_err("the drain ends the wait");
    assert!(
        matches!(&error, Error::Rpc(error) if error.message.contains("run the command again")),
        "{error:?}"
    );
}

#[tokio::test(flavor = "local")]
async fn agent_progress_returns_the_status_then_waits_for_the_next_change() {
    let fixture = api();
    fixture.client.apply(request("worker")).await.expect("apply");
    let current = fixture
        .client
        .agent_progress("worker", None, None)
        .await
        .expect("current progress");
    assert!(current.provisioning.is_none(), "no pass has run");
    assert_eq!(current.status.conditions, []);

    let follower = Client::new(Rc::new(InProcessConnector {
        server: fixture.server.clone(),
    }));
    let revision = current.revision;
    let follow = tokio::task::spawn_local(async move { follower.agent_progress("worker", Some(revision), None).await });
    tokio::task::yield_now().await;
    assert!(!follow.is_finished(), "a current revision waits for a change");
    fixture.changes.bump();
    let changed = follow.await.expect("follow task").expect("changed progress");
    assert_ne!(changed.revision, revision);

    let missing = fixture
        .client
        .agent_progress("missing", None, None)
        .await
        .expect_err("unknown agent");
    assert!(matches!(missing, Error::Rpc(error) if error.is_not_found()));
}

#[tokio::test(flavor = "local")]
async fn a_frame_that_is_not_the_response_fails_the_call() {
    let notification = ScriptedConnector {
        frames: concat!(r#"{"jsonrpc":"2.0","method":"progress.v1.event","params":{}}"#, "\n").into(),
    };
    let client = Client::new(Rc::new(notification));
    let error = client
        .ensure_execution("worker", WaitPolicy::UntilConverged)
        .await
        .expect_err("a notification is not a response");
    assert!(matches!(error, Error::Json(_)), "unexpected error: {error}");
}

#[tokio::test(flavor = "local")]
async fn stop_and_start_record_the_run_state_as_a_new_generation() {
    let fixture = api();
    let client = &fixture.client;
    let applied = client.apply(request("worker")).await.expect("apply");

    let stopped = client
        .set_run_state("worker", agent::RunState::Stopped)
        .await
        .expect("stop");
    assert_eq!(stopped.spec.run_state, Some(agent::RunState::Stopped));
    assert_eq!(stopped.metadata.generation, applied.metadata.generation + 1);
    assert_eq!(client.get("worker").await.expect("get"), stopped);
    let again = client
        .set_run_state("worker", agent::RunState::Stopped)
        .await
        .expect("repeated stop");
    assert_eq!(again.metadata.generation, stopped.metadata.generation);
    assert_eq!(client.converge("worker").await.expect("converge"), stopped);

    let started = client
        .set_run_state("worker", agent::RunState::Running)
        .await
        .expect("start");
    assert_eq!(started.spec.run_state, None);
    assert_eq!(started.metadata.generation, stopped.metadata.generation + 1);

    let error = client
        .set_run_state("missing", agent::RunState::Stopped)
        .await
        .expect_err("missing agent");
    assert!(matches!(error, Error::Rpc(error) if error.is_not_found()));
}

#[tokio::test(flavor = "local")]
async fn work_in_a_stopped_agent_is_refused_with_how_to_start_it() {
    let fixture = api();
    let error = fixture
        .client
        .ensure_execution("stopped", WaitPolicy::UntilConverged)
        .await
        .expect_err("a stopped agent runs nothing");
    match error {
        Error::Rpc(error) => {
            assert!(error.is_invalid_params(), "agentctl prints it as the whole story");
            assert_eq!(
                error.message,
                "Agent \"stopped\" is stopped; run `agentctl start agent/stopped`"
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test(flavor = "local")]
async fn application_errors_keep_stable_protocol_codes() {
    let fixture = api();
    let client = &fixture.client;
    let name = || agent::sessions::SessionName::new("s1").expect("name");
    let missing: [(&str, LocalFuture<'_, Result<(), Error>>); 4] = [
        ("get", Box::pin(async { client.get("missing").await.map(drop) })),
        (
            "prompt",
            Box::pin(client.prompt_session("missing", name(), "hello".into(), false, None)),
        ),
        (
            "archive",
            Box::pin(async { client.set_session_archived("missing", name(), true).await.map(drop) }),
        ),
        ("delete", Box::pin(client.delete_session("missing", name()))),
    ];

    for (operation, call) in missing {
        match call.await {
            Err(Error::Rpc(error)) => assert_eq!(error.code, -32004, "{operation}"),
            other => panic!("{operation}: unexpected result {other:?}"),
        }
    }
}

#[tokio::test(flavor = "local")]
async fn malformed_and_idle_connections_do_not_block_other_clients() {
    let fixture = api();
    let server = fixture.server;
    let client = fixture.client;
    client.apply(request("worker")).await.expect("apply");

    let (mut malformed_client, malformed_server) = tokio::io::duplex(1024);
    let malformed_api = server.clone();
    tokio::task::spawn_local(async move {
        let _ignored = malformed_api.serve_connection(malformed_server).await;
    });
    malformed_client
        .write_all(b"{not-json}\n")
        .await
        .expect("write malformed request");
    let mut response = String::new();
    BufReader::new(&mut malformed_client)
        .read_line(&mut response)
        .await
        .expect("read parse error");
    assert!(response.contains("-32700"));

    let (_idle_client, idle_server) = tokio::io::duplex(1024);
    let idle_api = server;
    tokio::task::spawn_local(async move {
        let _ignored = idle_api.serve_connection(idle_server).await;
    });
    let fetched = tokio::time::timeout(Duration::from_secs(1), client.get("worker"))
        .await
        .expect("active client should not wait for idle connection")
        .expect("get");
    assert_eq!(fetched.metadata.name, "worker");
}

#[tokio::test(flavor = "local")]
async fn session_ensure_rejects_invalid_selections_before_reaching_the_service() {
    let fixture = api();
    let (mut raw_client, raw_server) = tokio::io::duplex(4096);
    let api = fixture.server.clone();
    tokio::task::spawn_local(async move {
        let _ignored = api.serve_connection(raw_server).await;
    });
    let mut reader = BufReader::new(&mut raw_client);
    for (id, params) in [
        (1, r#"{"agent":"worker","name":"s1","model_selection":{"model":""}}"#),
        (
            2,
            r#"{"agent":"worker","name":"s1","model_selection":{"effort":"very high"}}"#,
        ),
    ] {
        let request = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"sessions.v1.ensure","params":{params}}}"#);
        reader
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write request");
        let mut response = String::new();
        reader.read_line(&mut response).await.expect("read response");
        let response: serde_json::Value = serde_json::from_str(&response).expect("JSON-RPC response");
        assert_eq!(response["error"]["code"], -32602, "{response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("must be 1-128 ASCII letters"),
            "the validation failure names the rule: {message}"
        );
    }
    assert!(fixture.ensured.borrow().is_empty());
}

#[tokio::test(flavor = "local")]
async fn a_guest_tcp_call_switches_its_connection_to_the_guest_bytes() {
    let fixture = api();
    let mut stream = fixture
        .client
        .guest_tcp("worker", None, 22)
        .await
        .expect("guest stream");
    let mut greeting = vec![0; GUEST_GREETING.len()];
    stream.read_exact(&mut greeting).await.expect("greeting");
    assert_eq!(greeting, GUEST_GREETING, "the guest speaks first");

    stream.write_all(b"hello").await.expect("send");
    stream.shutdown().await.expect("end the guest's input");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.expect("answer");
    assert_eq!(answer, b"HELLO", "the guest still answers after its input has ended");
}

#[tokio::test(flavor = "local")]
async fn bytes_sent_with_the_request_reach_the_guest() {
    let fixture = api();
    let (mut raw_client, raw_server) = tokio::io::duplex(4096);
    let api = fixture.server.clone();
    tokio::task::spawn_local(async move {
        let _ignored = api.serve_connection(raw_server).await;
    });
    let request = r#"{"jsonrpc":"2.0","id":7,"method":"streams.v1.guestTcp","params":{"name":"worker","port":22}}"#;
    raw_client
        .write_all(format!("{request}\nearly").as_bytes())
        .await
        .expect("request and stream bytes in one write");
    raw_client.shutdown().await.expect("end the guest's input");

    let mut received = Vec::new();
    raw_client
        .read_to_end(&mut received)
        .await
        .expect("response and stream");
    let newline = received.iter().position(|byte| *byte == b'\n').expect("response line");
    let response: serde_json::Value = serde_json::from_slice(&received[..newline]).expect("JSON-RPC response");
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"], serde_json::json!({"stream": "bytes"}));
    assert_eq!(&received[newline + 1..], [GUEST_GREETING, b"EARLY"].concat());
}

#[tokio::test(flavor = "local")]
async fn streams_flow_while_the_control_thread_is_blocked() {
    let fixture = api();
    let (client_end, server_end) = tokio::io::duplex(4096);
    let api = fixture.server.clone();
    tokio::task::spawn_local(async move {
        let _ignored = api.serve_connection(server_end).await;
    });
    let (switched, switched_here) = tokio::sync::oneshot::channel();
    let (blocked, blocked_there) = std::sync::mpsc::channel::<()>();
    let (echoed, echoed_here) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client runtime");
        runtime.block_on(async move {
            let mut connection = BufReader::new(client_end);
            let request =
                r#"{"jsonrpc":"2.0","id":1,"method":"streams.v1.guestTcp","params":{"name":"worker","port":7}}"#;
            connection
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .await
                .expect("request");
            let mut response = String::new();
            connection.read_line(&mut response).await.expect("response");
            switched.send(response).expect("report the switch");
            blocked_there.recv().expect("the control thread blocks");
            let mut rounds = 0;
            for _ in 0..20 {
                connection.get_mut().write_all(b"ping").await.expect("send");
                let mut answer = [0; 4];
                connection.read_exact(&mut answer).await.expect("echo");
                rounds += usize::from(&answer == b"ping");
            }
            echoed.send(rounds).expect("report the echoes");
        });
    });

    let response = switched_here.await.expect("switched");
    assert!(response.contains(r#""stream":"bytes""#), "{response}");
    blocked.send(()).expect("start echoing");
    // Blocks the control thread's runtime, as a slow call or reconciliation step would.
    let rounds = echoed_here.recv_timeout(Duration::from_secs(5));
    assert_eq!(rounds, Ok(20), "every echo arrives while the control thread is blocked");
    client.join().expect("client thread");
}

#[tokio::test(flavor = "local")]
async fn guest_tcp_failures_are_answered_before_any_stream() {
    let fixture = api();
    for (name, port, not_found) in [
        ("worker", 2222, false),
        ("stopped", 22, false),
        ("worker", 0, false),
        ("gone", 22, true),
    ] {
        let Err(Error::Rpc(error)) = fixture.client.guest_tcp(name, None, port).await else {
            panic!("{name}:{port} should be refused with a JSON-RPC error");
        };
        assert_eq!(error.is_not_found(), not_found, "{name}:{port}: {error:?}");
        assert_eq!(error.is_invalid_params(), !not_found, "{name}:{port}: {error:?}");
    }
}

#[tokio::test(flavor = "local")]
async fn a_guest_tcp_call_reaches_only_the_agent_with_its_uid() {
    let fixture = api();
    let replaced = "00000000-0000-0000-0000-000000000001".parse().expect("uid");
    let Err(Error::Rpc(error)) = fixture.client.guest_tcp("worker", Some(replaced), 22).await else {
        panic!("another agent's uid should be refused with a JSON-RPC error");
    };
    assert!(error.is_invalid_params(), "{error:?}");

    let mut stream = fixture
        .client
        .guest_tcp("worker", Some(WORKER_UID.parse().expect("uid")), 22)
        .await
        .expect("the current agent's uid");
    let mut greeting = vec![0; GUEST_GREETING.len()];
    stream.read_exact(&mut greeting).await.expect("greeting");
}

#[tokio::test(flavor = "local")]
async fn an_upgrade_warns_about_open_streams_and_closes_them() {
    let fixture = api();
    let mut stream = fixture
        .client
        .guest_tcp("worker", None, 22)
        .await
        .expect("guest stream");
    let mut greeting = vec![0; GUEST_GREETING.len()];
    stream.read_exact(&mut greeting).await.expect("greeting");

    let warnings = fixture
        .client
        .shutdown_for_upgrade()
        .await
        .expect("an open stream does not block an upgrade");
    assert_eq!(warnings.len(), 1, "the open stream is named: {warnings:?}");
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut rest))
        .await
        .expect("the drain closes the stream")
        .expect("closed stream");
    assert_eq!(rest, b"");
}

#[tokio::test(flavor = "local")]
async fn streams_still_opening_count_against_the_budget() {
    let fixture = api_with_open_files(Some(8));
    let openings = (0..5).map(|_| fixture.client.guest_tcp(SLOW_AGENT, None, 22));
    let opened = futures_util::future::join_all(openings).await;
    let streams = opened.iter().filter(|opened| opened.is_ok()).count();
    assert_eq!(streams, 2, "only as many streams as the budget allows open at once");
}

#[tokio::test(flavor = "local")]
async fn a_stream_call_whose_client_ends_its_input_still_opens() {
    let fixture = api();
    let (mut raw_client, raw_server) = tokio::io::duplex(4096);
    let api = fixture.server.clone();
    tokio::task::spawn_local(async move {
        let _ignored = api.serve_connection(raw_server).await;
    });
    let request = r#"{"jsonrpc":"2.0","id":3,"method":"streams.v1.guestTcp","params":{"name":"slow","port":22}}"#;
    raw_client
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("request");
    raw_client
        .shutdown()
        .await
        .expect("end the guest's input with nothing sent");

    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), raw_client.read_to_end(&mut received))
        .await
        .expect("the stream ends")
        .expect("response and stream");
    let newline = received.iter().position(|byte| *byte == b'\n').expect("response line");
    let response: serde_json::Value = serde_json::from_slice(&received[..newline]).expect("JSON-RPC response");
    assert_eq!(response["result"], serde_json::json!({"stream": "bytes"}), "{response}");
    assert_eq!(&received[newline + 1..], GUEST_GREETING, "the guest's output arrives");
}

#[tokio::test(flavor = "local")]
async fn a_low_open_file_limit_caps_the_open_streams() {
    let fixture = api_with_open_files(Some(8));
    let mut streams = Vec::new();
    for _ in 0..2 {
        let mut stream = fixture
            .client
            .guest_tcp("worker", None, 22)
            .await
            .expect("guest stream");
        let mut greeting = vec![0; GUEST_GREETING.len()];
        stream.read_exact(&mut greeting).await.expect("greeting");
        streams.push(stream);
    }
    let Err(Error::Rpc(_)) = fixture.client.guest_tcp("worker", None, 22).await else {
        panic!("a third stream needs more descriptors than agentd may open");
    };
    drop(streams.pop());
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.client.guest_tcp("worker", None, 22).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a closed stream makes room for another");
}

#[cfg(unix)]
#[tokio::test(flavor = "local")]
async fn open_streams_leave_room_for_calls() {
    let temporary = tempfile::Builder::new()
        .prefix("agent-api-")
        .tempdir()
        .expect("temporary API directory");
    let socket_path = temporary.path().join("agentd.sock");
    let server = api().server;
    let served_path = socket_path.clone();
    tokio::task::spawn_local(async move { server.serve_path(&served_path).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !socket_path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("socket should be created");

    let client = Client::for_path(socket_path);
    let mut streams = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        // More than the daemon serves calls at once.
        for _ in 0..200 {
            let mut stream = client.guest_tcp("worker", None, 22).await.expect("guest stream");
            let mut greeting = vec![0; GUEST_GREETING.len()];
            stream.read_exact(&mut greeting).await.expect("greeting");
            streams.push(stream);
        }
        client.health().await.expect("health");
        let first = streams.first_mut().expect("first stream");
        first.write_all(b"still open").await.expect("send");
        first.shutdown().await.expect("end the guest's input");
        let mut answer = Vec::new();
        first.read_to_end(&mut answer).await.expect("answer");
        assert_eq!(answer, b"STILL OPEN");
    })
    .await
    .expect("calls are still served");
}

#[cfg(unix)]
#[tokio::test(flavor = "local")]
async fn unix_socket_transport_is_private_and_usable() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::Builder::new()
        .prefix("agent-api-")
        .tempdir()
        .expect("temporary API directory");
    let socket_path = temporary.path().join("p").join("agentd.sock");
    let fixture = api();
    let server = fixture.server;
    let served_path = socket_path.clone();
    let mut server_task = tokio::task::spawn_local(async move { server.serve_path(&served_path).await });
    let wait_for_socket = tokio::time::timeout(Duration::from_secs(1), async {
        while !socket_path.exists() {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = &mut server_task => panic!("server stopped before creating its socket: {result:?}"),
        result = wait_for_socket => result.expect("socket should be created"),
    }

    let client = Client::for_path(socket_path.clone());
    let applied = client.apply(request("worker")).await.expect("apply over Unix socket");
    assert_eq!(applied.metadata.name, "worker");

    let directory_mode = std::fs::metadata(socket_path.parent().expect("socket parent"))
        .expect("directory metadata")
        .permissions()
        .mode()
        & 0o777;
    let socket_mode = std::fs::metadata(&socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    assert_eq!(socket_mode, 0o600);
    client.shutdown_for_upgrade().await.expect("graceful shutdown");
    tokio::time::timeout(Duration::from_secs(1), &mut server_task)
        .await
        .expect("server should stop")
        .expect("server task")
        .expect("server result");
}

/// Set in the child process that runs [`running_out_of_descriptors_does_not_end_the_server`].
#[cfg(unix)]
const DESCRIPTOR_CHILD: &str = "AGENT_TEST_DESCRIPTOR_CHILD";

/// Runs its server in a child process, since it lowers the open-file limit of its whole process.
#[cfg(unix)]
#[test]
fn running_out_of_descriptors_does_not_end_the_server() {
    if std::env::var_os(DESCRIPTOR_CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "control_api::running_out_of_descriptors_does_not_end_the_server",
                "--test-threads=1",
            ])
            .env(DESCRIPTOR_CHILD, "1")
            .status()
            .expect("run the child test");
        assert!(status.success(), "the child test failed: {status}");
        return;
    }
    let runtime = tokio::runtime::LocalRuntime::new().expect("runtime");
    runtime.block_on(async {
        let temporary = tempfile::Builder::new()
            .prefix("agent-api-")
            .tempdir()
            .expect("temporary API directory");
        let socket_path = temporary.path().join("agentd.sock");
        let server = api().server;
        let served_path = socket_path.clone();
        let serving = tokio::task::spawn_local(async move { server.serve_path(&served_path).await });
        while !socket_path.exists() {
            tokio::task::yield_now().await;
        }

        let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(256),
                maximum: limit.maximum,
            },
        )
        .expect("lower the open-file limit");
        let mut filler = Vec::new();
        while let Ok(file) = std::fs::File::open("/dev/null") {
            filler.push(file);
        }
        // One descriptor for the client, none left for the server to accept it with.
        filler.pop();
        let client = Client::for_path(socket_path.clone());
        let health = tokio::task::spawn_local(async move { client.health().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!serving.is_finished(), "the server keeps serving without descriptors");
        drop(filler);
        let waiting = tokio::time::timeout(Duration::from_secs(2), health)
            .await
            .expect("the waiting call ends once descriptors are free")
            .expect("health task");
        // Linux leaves a connection it has no descriptor for in the backlog, while macOS closes it.
        if cfg!(target_os = "linux") {
            waiting.expect("the waiting call is served");
        }
        Client::for_path(socket_path)
            .health()
            .await
            .expect("a new call is served once descriptors are free");
    });
}
