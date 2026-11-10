//! Agent implementation of `sandbox_authorization::PolicyEngine`.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use tokio::sync::{mpsc, oneshot};

use sandbox_authorization::{
    AuthorizationDecision, AuthorizationRequest, PolicyEngine,
    vocabulary::{action, context, principal_kind},
};

use super::SecretMirror;
use crate::Agent;
use sandbox::SandboxName;

/// Live policy registry keyed by stable agent/Sandbox name.
#[derive(Default)]
pub struct AgentPolicyEngine {
    agents: RefCell<BTreeMap<String, AgentPolicy>>,
    platform_endpoint: RefCell<Option<PlatformEndpoint>>,
    /// The copy of the secrets stored under a name, whose hosts are checked at every use.
    secrets: Option<Rc<SecretMirror>>,
}

struct PlatformEndpoint {
    host: String,
    port: u16,
}

impl AgentPolicyEngine {
    /// Creates an empty, fail-closed policy registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks the hosts of secrets stored under a name in `secrets`. Without it, a stored secret is
    /// substituted nowhere.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Rc<SecretMirror>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Applies the changes `updates` receives until every [`PolicyUpdates`] sending them is gone.
    /// It runs on the thread the engine decides on, which may be another than theirs.
    pub fn serve(self: Rc<Self>, mut updates: PolicyUpdateReceiver) {
        tokio::task::spawn_local(async move {
            while let Some((update, applied)) = updates.0.recv().await {
                match update {
                    Update::Agent { sandbox, policy } => {
                        // Preparing the agent may have written its secrets just before. The policy
                        // waits for them, so that it never allows a new host an old value.
                        if let Some(secrets) = &self.secrets
                            && let Err(error) = secrets.caught_up().await
                        {
                            tracing::error!(%error, "could not wait for the copy of the secrets");
                        }
                        self.agents.borrow_mut().insert(sandbox.as_str().into(), policy);
                    }
                    Update::RemoveAgent(sandbox) => {
                        self.agents.borrow_mut().remove(sandbox.as_str());
                    }
                    Update::PlatformEndpoint(endpoint) => *self.platform_endpoint.borrow_mut() = Some(endpoint),
                }
                let _ = applied.send(());
            }
        });
    }
}

/// Changes the policy of an [`AgentPolicyEngine`] that [`AgentPolicyEngine::serve`] runs, which
/// may be on another thread.
///
/// Each change is applied before its call returns, so what the caller does next sees it, such as
/// revoking the connections an earlier policy admitted.
#[derive(Clone)]
pub struct PolicyUpdates(mpsc::UnboundedSender<(Update, Applied)>);

/// Where an [`AgentPolicyEngine`] receives what [`PolicyUpdates`] send.
pub struct PolicyUpdateReceiver(mpsc::UnboundedReceiver<(Update, Applied)>);

type Applied = oneshot::Sender<()>;

enum Update {
    Agent { sandbox: SandboxName, policy: AgentPolicy },
    RemoveAgent(SandboxName),
    PlatformEndpoint(PlatformEndpoint),
}

impl PolicyUpdates {
    /// A sender of policy changes, and where an engine receives them.
    #[must_use]
    pub fn channel() -> (Self, PolicyUpdateReceiver) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), PolicyUpdateReceiver(receiver))
    }

    /// Registers the one sandbox-reachable host endpoint exposed by the platform.
    ///
    /// Host-destined traffic (the Network Backend's host alias, and raw dials
    /// into host-reserved address ranges) is denied except for exactly this
    /// endpoint; without a registered endpoint it is denied entirely.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine is gone.
    pub async fn set_platform_endpoint(&self, host: impl Into<String>, port: u16) -> Result<(), crate::Error> {
        self.apply(Update::PlatformEndpoint(PlatformEndpoint {
            host: host.into(),
            port,
        }))
        .await
    }

    /// Replaces the policy for one desired agent generation, whose sandbox gets `secrets`.
    /// Connections an earlier policy admitted stay open until they are revoked.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine is gone.
    pub async fn set_agent(
        &self,
        sandbox: &SandboxName,
        agent: &Agent,
        secrets: impl IntoIterator<Item = SecretHosts>,
    ) -> Result<(), crate::Error> {
        self.apply(Update::Agent {
            sandbox: sandbox.clone(),
            policy: AgentPolicy::of(agent, secrets),
        })
        .await
    }

    /// Removes policy for a released agent.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine is gone.
    pub async fn remove_agent(&self, sandbox: &SandboxName) -> Result<(), crate::Error> {
        self.apply(Update::RemoveAgent(sandbox.clone())).await
    }

    async fn apply(&self, update: Update) -> Result<(), crate::Error> {
        let (applied, is_applied) = oneshot::channel();
        let gone = || crate::Error::Daemon("the network policy engine stopped".into());
        self.0.send((update, applied)).map_err(|_| gone())?;
        is_applied.await.map_err(|_| gone())
    }
}

/// Where a sandbox's secret may be substituted: at a host that each of its host lists matches,
/// such as both the hosts a manifest sends a stored secret to and those the secret allows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretHosts {
    /// The environment variable holding the secret's placeholder.
    pub environment: String,
    /// Host patterns, such as `api.example.com` or `*.example.com`. A host must match one pattern
    /// of every list. Only a stored secret may have none, as its own hosts are checked.
    pub hosts: Vec<Vec<String>>,
    /// The secret stored under this name, whose hosts a host must match as well. They are checked
    /// at every use, as its value is read, in a copy that every change reaches in the order it was
    /// made, so a host they no longer allow never receives a value set with them.
    pub stored: Option<String>,
}

impl SecretHosts {
    /// A secret substituted at the hosts one list of patterns matches.
    #[must_use]
    pub fn new(environment: impl Into<String>, hosts: Vec<String>) -> Self {
        Self {
            environment: environment.into(),
            hosts: vec![hosts],
            stored: None,
        }
    }
}

struct AgentPolicy {
    allowed_hosts: crate::NetworkAllow,
    denied_hosts: Vec<String>,
    secrets: BTreeMap<String, SecretHosts>,
}

impl AgentPolicy {
    fn of(agent: &Agent, secrets: impl IntoIterator<Item = SecretHosts>) -> Self {
        Self {
            allowed_hosts: agent.spec.network.allow.clone(),
            denied_hosts: agent.spec.network.deny.clone(),
            secrets: secrets
                .into_iter()
                .map(|secret| (secret.environment.clone(), secret))
                .collect(),
        }
    }
}

impl PolicyEngine for AgentPolicyEngine {
    fn evaluate(
        &self,
        request: AuthorizationRequest,
    ) -> sandbox_authorization::LocalFuture<'_, Result<AuthorizationDecision, sandbox_authorization::Error>> {
        Box::pin(async move {
            let decided = self.evaluate_request(&request);
            Ok(match self.stored_secret_to_check(&request) {
                Some((name, host)) if decided == AuthorizationDecision::Allow => {
                    decision(self.stored_secret_allows(&name, &host))
                }
                _ => decided,
            })
        })
    }
}

impl AgentPolicyEngine {
    fn evaluate_request(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        if request.principal.kind != principal_kind::SANDBOX {
            return AuthorizationDecision::Deny;
        }
        let Some(agent_name) = request
            .context
            .get(context::SANDBOX_NAME)
            .and_then(|value| value.as_str())
        else {
            return AuthorizationDecision::Deny;
        };
        let agents = self.agents.borrow();
        let Some(policy) = agents.get(agent_name) else {
            return AuthorizationDecision::Deny;
        };
        let host = request_host(request);
        if let Some(host_destined) = self.host_destined_decision(request, host) {
            return host_destined;
        }
        match request.action.as_str() {
            action::NETWORK_CONNECT | action::DNS_QUERY | action::HTTP_REQUEST => {
                host.map_or(AuthorizationDecision::Deny, |host| {
                    decision(
                        policy.allowed_hosts.admits(host, uses_platform_dns(request))
                            && !policy
                                .denied_hosts
                                .iter()
                                .any(|pattern| crate::host_pattern::matches(pattern, host)),
                    )
                })
            }
            action::SECRET_USE => {
                let Some(host) = host else {
                    return AuthorizationDecision::Deny;
                };
                // A secret with no host list is allowed nowhere, unless it is stored, whose own
                // hosts are checked next.
                decision(policy.secrets.get(&request.resource.id).is_some_and(|secret| {
                    (!secret.hosts.is_empty() || secret.stored.is_some())
                        && secret.hosts.iter().all(|patterns| {
                            patterns
                                .iter()
                                .any(|pattern| crate::host_pattern::matches(pattern, host))
                        })
                }))
            }
            _ => AuthorizationDecision::Deny,
        }
    }
}

impl AgentPolicyEngine {
    /// The stored secret, and the host, that a use of a secret must also be allowed by.
    fn stored_secret_to_check(&self, request: &AuthorizationRequest) -> Option<(String, String)> {
        if request.action.as_str() != action::SECRET_USE {
            return None;
        }
        let agent = request.context.get(context::SANDBOX_NAME)?.as_str()?;
        let agents = self.agents.borrow();
        let stored = agents.get(agent)?.secrets.get(&request.resource.id)?.stored.clone()?;
        Some((stored, request_host(request)?.to_owned()))
    }

    /// Whether the secret stored as `name` allows `host` now. One that is gone allows nothing.
    fn stored_secret_allows(&self, name: &str, host: &str) -> bool {
        self.secrets
            .as_ref()
            .and_then(|secrets| secrets.allowed_hosts(name))
            .is_some_and(|hosts| hosts.iter().any(|pattern| crate::host_pattern::matches(pattern, host)))
    }

    /// Decides host-destined traffic: the Network Backend rewrites its host
    /// alias (and only its gateway addresses) to host loopback, so anything
    /// aimed at the host must match the registered Platform API endpoint exactly.
    ///
    /// The trusted Network Backend reports host-destined flows through the
    /// `network.destinationIsHost` attribute, set from the same gateway-to-
    /// loopback rewrite it applies at dial time — this is the authoritative
    /// signal. The host-reserved range check is kept as defense in depth for
    /// requests that predate the flag or arrive without it. Returns `None` for
    /// traffic that is not host-destined.
    fn host_destined_decision(
        &self,
        request: &AuthorizationRequest,
        host: Option<&str>,
    ) -> Option<AuthorizationDecision> {
        let action = request.action.as_str();
        if !matches!(
            action,
            action::NETWORK_CONNECT | action::DNS_QUERY | action::HTTP_REQUEST
        ) {
            return None;
        }
        let endpoint = self.platform_endpoint.borrow();
        let alias = endpoint
            .as_ref()
            .zip(host)
            .is_some_and(|(endpoint, host)| host.eq_ignore_ascii_case(&endpoint.host));
        let destination = destination_address(request);
        let flagged = request
            .context
            .get(context::NETWORK_DESTINATION_IS_HOST)
            .and_then(sandbox_authorization::AuthorizationValue::as_bool)
            .unwrap_or(false);
        let reserved = destination.is_some_and(|address| is_host_reserved(address.ip()));
        if !alias && !flagged && !reserved {
            return None;
        }
        if action == action::DNS_QUERY {
            // Resolving the alias through the platform's DNS only reveals the gateway address. A
            // resolver the sandbox picks gets the query itself, so it is decided as any other.
            return request
                .context
                .get(context::DNS_RESOLVER)
                .is_none()
                .then_some(AuthorizationDecision::Allow);
        }
        // Every port the request names must be the API's: an HTTP CONNECT through a proxy names
        // the proxy as its destination and the tunnel's own port in its authority.
        let ports = [destination.map(|address| address.port()), authority_port(request)];
        let at_api_port =
            |api: u16| ports.iter().any(Option::is_some) && ports.iter().flatten().all(|&port| port == api);
        // The sandbox names the alias itself, so a connection counts only towards the host's own
        // address, which only the Network Backend's flag identifies: host-reserved ranges also hold
        // other machines, such as VPN peers, and the gateway need not be in them. An HTTP request
        // carries no flag, and goes over a connection already decided, so naming the alias there
        // reaches only what that connection could.
        let at_host = action != action::NETWORK_CONNECT || flagged;
        let allowed = alias && at_host && endpoint.as_ref().is_some_and(|endpoint| at_api_port(endpoint.port));
        Some(decision(allowed))
    }
}

/// Destination socket address reported by the Network Backend, when present.
fn destination_address(request: &AuthorizationRequest) -> Option<std::net::SocketAddr> {
    request
        .context
        .get(context::NETWORK_DESTINATION_ADDRESS)
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse().ok())
}

/// Port carried by the HTTP authority, when present.
fn authority_port(request: &AuthorizationRequest) -> Option<u16> {
    request
        .context
        .get(context::HTTP_AUTHORITY)
        .and_then(|value| value.as_str())
        .and_then(|authority| authority.rsplit_once(':'))
        .and_then(|(_, port)| port.parse().ok())
}

/// Address ranges that can carry the Network Backend's gateway, plus ranges
/// that never name a legitimate upstream from inside a sandbox.
const fn is_host_reserved(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                // CGNAT 100.64.0.0/10 carries the per-sandbox gateway.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0b1100_0000) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // ULA fc00::/7 carries the per-sandbox gateway.
                || (v6.octets()[0] & 0b1111_1110) == 0xfc
                // Link-local fe80::/10.
                || (v6.octets()[0] == 0xfe && (v6.octets()[1] & 0b1100_0000) == 0x80)
        }
    }
}

/// Whether `request` keeps to the names the platform's DNS gave the sandbox, as an agent that
/// reaches only some hosts must. The sandbox names the host it connects to itself, such as in its
/// TLS SNI or HTTP `Host`, so a connection counts only where that DNS resolved the name to the
/// address it dials. Answers from a resolver the sandbox picks would count as well, so a DNS query
/// to one is refused. An HTTP request goes over a connection already decided.
fn uses_platform_dns(request: &AuthorizationRequest) -> bool {
    match request.action.as_str() {
        action::NETWORK_CONNECT => request
            .context
            .get(context::NETWORK_HOSTNAME_RESOLVED)
            .and_then(sandbox_authorization::AuthorizationValue::as_bool)
            .unwrap_or(false),
        action::DNS_QUERY => request.context.get(context::DNS_RESOLVER).is_none(),
        _ => true,
    }
}

fn request_host(request: &AuthorizationRequest) -> Option<&str> {
    request
        .context
        .get(context::HTTP_AUTHORITY)
        .and_then(|value| value.as_str())
        .or_else(|| {
            request
                .context
                .get(context::NETWORK_HOSTNAME)
                .and_then(|value| value.as_str())
        })
        .or_else(|| (request.action.as_str() == action::DNS_QUERY).then_some(request.resource.id.as_str()))
        .map(without_port)
}

fn without_port(authority: &str) -> &str {
    if let Some(bracketed) = authority.strip_prefix('[')
        && let Some(end) = bracketed.find(']')
    {
        return &bracketed[..end];
    }
    authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(authority, |(host, _)| host)
}

const fn decision(allowed: bool) -> AuthorizationDecision {
    if allowed {
        AuthorizationDecision::Allow
    } else {
        AuthorizationDecision::Deny
    }
}
