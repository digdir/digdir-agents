//! Agent implementation of `sandbox_authorization::PolicyEngine`.

use std::{cell::RefCell, collections::BTreeMap};

use sandbox_authorization::{
    AuthorizationDecision, AuthorizationRequest, PolicyEngine,
    vocabulary::{action, context, principal_kind},
};

use crate::Agent;
use sandbox::SandboxName;

/// Live policy registry keyed by stable agent/Sandbox name.
#[derive(Default)]
pub struct AgentPolicyEngine {
    agents: RefCell<BTreeMap<String, AgentPolicy>>,
    platform_endpoint: RefCell<Option<PlatformEndpoint>>,
    /// Where the secrets stored under a name are, whose hosts are read at every use.
    stored_secrets: Option<crate::persistence::Database>,
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

    /// Reads the hosts of secrets stored under a name from `database`. Without it, a stored secret is
    /// substituted nowhere.
    #[must_use]
    pub fn with_stored_secrets(mut self, database: crate::persistence::Database) -> Self {
        self.stored_secrets = Some(database);
        self
    }

    /// Registers the one sandbox-reachable host endpoint exposed by the platform.
    ///
    /// Host-destined traffic (the Network Backend's host alias, and raw dials
    /// into host-reserved address ranges) is denied except for exactly this
    /// endpoint; without a registered endpoint it is denied entirely.
    pub fn set_platform_endpoint(&self, host: impl Into<String>, port: u16) {
        *self.platform_endpoint.borrow_mut() = Some(PlatformEndpoint {
            host: host.into(),
            port,
        });
    }

    /// Replaces the policy for one desired agent generation, whose sandbox gets `secrets`.
    pub fn set_agent(&self, sandbox: &SandboxName, agent: &Agent, secrets: impl IntoIterator<Item = SecretHosts>) {
        let secrets = secrets
            .into_iter()
            .map(|secret| (secret.environment.clone(), secret))
            .collect();
        self.agents.borrow_mut().insert(
            sandbox.as_str().into(),
            AgentPolicy {
                denied_hosts: agent.spec.network.deny.clone(),
                secrets,
            },
        );
    }

    /// Removes policy for a released agent.
    pub fn remove_agent(&self, sandbox: &SandboxName) {
        self.agents.borrow_mut().remove(sandbox.as_str());
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
    /// The secret stored under this name, whose hosts a host must match as well. They are read
    /// where it is stored at every use, as its value is, so changing them applies at once and a
    /// host they no longer allow never receives a value set with them.
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
    denied_hosts: Vec<String>,
    secrets: BTreeMap<String, SecretHosts>,
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
                    decision(self.stored_secret_allows(&name, &host).await)
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
            action::NETWORK_CONNECT | action::DNS_QUERY | action::HTTP_REQUEST => host
                .map_or(AuthorizationDecision::Deny, |host| {
                    decision(!policy.denied_hosts.iter().any(|pattern| host_matches(pattern, host)))
                }),
            action::SECRET_USE => {
                let Some(host) = host else {
                    return AuthorizationDecision::Deny;
                };
                // A secret with no host list is allowed nowhere, unless it is stored, whose own
                // hosts are checked next.
                decision(policy.secrets.get(&request.resource.id).is_some_and(|secret| {
                    (!secret.hosts.is_empty() || secret.stored.is_some())
                        && secret
                            .hosts
                            .iter()
                            .all(|patterns| patterns.iter().any(|pattern| host_matches(pattern, host)))
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

    /// Whether the secret stored as `name` allows `host` now. One that is gone, or cannot be read,
    /// allows nothing.
    async fn stored_secret_allows(&self, name: &str, host: &str) -> bool {
        let Some(database) = &self.stored_secrets else {
            return false;
        };
        crate::secrets::stored(database, name)
            .await
            .ok()
            .flatten()
            .is_some_and(|secret| secret.allowed_hosts.iter().any(|pattern| host_matches(pattern, host)))
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
            // Resolving the alias only reveals the gateway address.
            return Some(AuthorizationDecision::Allow);
        }
        let port = destination
            .map(|address| address.port())
            .or_else(|| authority_port(request));
        let allowed = alias && endpoint.as_ref().is_some_and(|endpoint| port == Some(endpoint.port));
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

fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    pattern.strip_prefix("*.").map_or_else(
        || host == pattern,
        |suffix| {
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        },
    )
}

/// Whether every host that `inner` matches is one that `outer` matches too.
pub(crate) fn host_pattern_covers(outer: &str, inner: &str) -> bool {
    // `*.suffix` is every host below the domain `suffix`: covered by the same pattern, or by one
    // matching the domain itself as a host below its own.
    inner.strip_prefix("*.").map_or_else(
        || host_matches(outer, inner),
        |suffix| outer.eq_ignore_ascii_case(inner) || (outer.starts_with("*.") && host_matches(outer, suffix)),
    )
}

const fn decision(allowed: bool) -> AuthorizationDecision {
    if allowed {
        AuthorizationDecision::Allow
    } else {
        AuthorizationDecision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::host_pattern_covers;

    #[test]
    fn a_pattern_covers_the_hosts_it_matches_and_the_patterns_below_it() {
        for (outer, inner) in [
            ("api.example.com", "api.example.com"),
            ("*.example.com", "api.example.com"),
            ("*.example.com", "*.example.com"),
            ("*.example.com", "*.api.example.com"),
            ("*.EXAMPLE.com", "API.example.com"),
        ] {
            assert!(host_pattern_covers(outer, inner), "{outer} covers {inner}");
        }
        for (outer, inner) in [
            ("api.example.com", "*.example.com"),
            ("api.example.com", "www.example.com"),
            ("*.example.com", "example.com"),
            ("*.api.example.com", "*.example.com"),
        ] {
            assert!(!host_pattern_covers(outer, inner), "{outer} does not cover {inner}");
        }
    }
}
