#![allow(clippy::expect_used)]

use crate::support;

use agent::authorization::{AgentPolicyEngine, SecretHosts};
use sandbox::SandboxName;
use sandbox_authorization::{
    Action, AuthorizationContext, AuthorizationDecision, AuthorizationRequest, PolicyEngine as _, Principal, Resource,
    vocabulary::{action, context, principal_kind, resource_kind},
};

#[tokio::test(flavor = "local")]
async fn allows_general_egress_but_scopes_each_secret_to_its_hosts() {
    let mut agent = support::agent("worker");
    agent.spec.network.deny.push("blocked.example".into());
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(
        &sandbox,
        &agent,
        [SecretHosts::new("GITHUB_TOKEN", vec!["github.com".into()])],
    );

    assert_eq!(
        evaluate(
            &policy,
            sandbox.as_str(),
            action::HTTP_REQUEST,
            "externalService",
            "github.com",
            "github.com"
        )
        .await,
        AuthorizationDecision::Allow
    );
    assert_eq!(
        evaluate(
            &policy,
            sandbox.as_str(),
            action::HTTP_REQUEST,
            "externalService",
            "blocked.example",
            "blocked.example",
        )
        .await,
        AuthorizationDecision::Deny
    );
    assert_eq!(
        evaluate(
            &policy,
            sandbox.as_str(),
            action::SECRET_USE,
            resource_kind::SECRET,
            "GITHUB_TOKEN",
            "github.com",
        )
        .await,
        AuthorizationDecision::Allow
    );
    assert_eq!(
        evaluate(
            &policy,
            sandbox.as_str(),
            action::SECRET_USE,
            resource_kind::SECRET,
            "GITHUB_TOKEN",
            "example.com",
        )
        .await,
        AuthorizationDecision::Deny
    );
}

/// With a list of allowed hosts, only those are reached, and a denied host is refused even when the list matches it.
#[tokio::test(flavor = "local")]
async fn an_allow_list_reaches_only_its_hosts() {
    let mut agent = support::agent("worker");
    agent.spec.network.allow = agent::NetworkAllow::Hosts(vec!["*.github.com".into(), "auth.openai.com".into()]);
    agent.spec.network.deny.push("uploads.github.com".into());
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(&sandbox, &agent, []);

    for operation in [action::HTTP_REQUEST, action::DNS_QUERY] {
        for (host, expected) in [
            ("api.github.com", AuthorizationDecision::Allow),
            ("auth.openai.com", AuthorizationDecision::Allow),
            ("github.com", AuthorizationDecision::Deny),
            ("uploads.github.com", AuthorizationDecision::Deny),
            ("example.com", AuthorizationDecision::Deny),
        ] {
            assert_eq!(
                evaluate(&policy, sandbox.as_str(), operation, "externalService", host, host).await,
                expected,
                "{operation} {host}"
            );
        }
    }
    assert_eq!(
        connect_resolved(&policy, sandbox.as_str(), "auth.openai.com", "203.0.113.7:443", true).await,
        AuthorizationDecision::Allow
    );
    assert_eq!(
        connect_resolved(&policy, sandbox.as_str(), "example.com", "203.0.113.8:443", true).await,
        AuthorizationDecision::Deny
    );
    // An allowed name the sandbox's DNS never gave for the address it dials, as a program sends
    // to reach any address.
    assert_eq!(
        connect_resolved(&policy, sandbox.as_str(), "auth.openai.com", "203.0.113.9:8443", false).await,
        AuthorizationDecision::Deny
    );
    assert_eq!(
        connect(&policy, sandbox.as_str(), Some("auth.openai.com"), "203.0.113.9:80").await,
        AuthorizationDecision::Deny,
        "a runtime that does not report it fails closed"
    );

    // A resolver the sandbox picks could answer an allowed name with any address.
    let to_resolver = |resolver: Option<&'static str>| {
        let mut authorization_context = AuthorizationContext::new()
            .with_attribute(context::SANDBOX_NAME, sandbox.as_str())
            .with_attribute(context::DNS_RECORD_TYPE, "A");
        if let Some(resolver) = resolver {
            authorization_context.insert(context::DNS_RESOLVER, resolver);
        }
        policy.evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(action::DNS_QUERY),
            resource: Resource::new(resource_kind::DOMAIN, "auth.openai.com"),
            context: authorization_context,
        })
    };
    assert_eq!(to_resolver(None).await.expect("decision"), AuthorizationDecision::Allow);
    assert_eq!(
        to_resolver(Some("198.51.100.53:53")).await.expect("decision"),
        AuthorizationDecision::Deny
    );

    // Allowing every host leaves the name where it was dialed unchecked.
    let open = SandboxName::new("agent-open-id").expect("Sandbox name");
    policy.set_agent(&open, &support::agent("open"), []);
    assert_eq!(
        connect_resolved(&policy, open.as_str(), "example.com", "203.0.113.9:8443", false).await,
        AuthorizationDecision::Allow
    );
}

async fn connect_resolved(
    policy: &AgentPolicyEngine,
    agent: &str,
    hostname: &str,
    destination: &str,
    resolved: bool,
) -> AuthorizationDecision {
    policy
        .evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(action::NETWORK_CONNECT),
            resource: Resource::new(resource_kind::EXTERNAL_SERVICE, hostname),
            context: AuthorizationContext::new()
                .with_attribute(context::SANDBOX_NAME, agent)
                .with_attribute(context::NETWORK_DESTINATION_ADDRESS, destination)
                .with_attribute(context::NETWORK_DESTINATION_IS_HOST, false)
                .with_attribute(context::NETWORK_HOSTNAME, hostname)
                .with_attribute(context::NETWORK_HOSTNAME_RESOLVED, resolved),
        })
        .await
        .expect("policy decision")
}

/// A stored secret is substituted only at a host that both the manifest and the secret allow.
#[tokio::test(flavor = "local")]
async fn a_secret_with_several_host_lists_is_used_only_where_all_of_them_match() {
    let agent = support::agent("worker");
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(
        &sandbox,
        &agent,
        [SecretHosts {
            environment: "API_KEY".into(),
            hosts: vec![
                vec!["*.example.com".into()],
                vec!["api.example.com".into(), "other.test".into()],
            ],
            stored: None,
        }],
    );
    for (host, decision) in [
        ("api.example.com", AuthorizationDecision::Allow),
        ("www.example.com", AuthorizationDecision::Deny),
        ("other.test", AuthorizationDecision::Deny),
    ] {
        assert_eq!(
            evaluate(
                &policy,
                sandbox.as_str(),
                action::SECRET_USE,
                resource_kind::SECRET,
                "API_KEY",
                host
            )
            .await,
            decision,
            "{host}"
        );
    }
}

#[tokio::test(flavor = "local")]
async fn host_destined_traffic_reaches_only_the_platform_api() {
    let agent = support::agent("worker");
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(&sandbox, &agent, []);

    // Fail closed: without a registered Platform API endpoint every host-destined
    // connect is denied, even under the allow-all egress default.
    assert_eq!(
        connect(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "100.64.0.2:9999"
        )
        .await,
        AuthorizationDecision::Deny
    );

    policy.set_platform_endpoint("host.microsandbox.internal", 4_100);

    assert_eq!(
        connect_flagged(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "100.64.0.2:4100",
            true
        )
        .await,
        AuthorizationDecision::Allow
    );
    assert_eq!(
        connect(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "100.64.0.2:8080"
        )
        .await,
        AuthorizationDecision::Deny
    );
    // A raw dial into a host-reserved range carries no hostname and is denied
    // even on the Platform API port: only the alias identifies the endpoint.
    assert_eq!(
        connect(&policy, sandbox.as_str(), None, "100.64.0.2:4100").await,
        AuthorizationDecision::Deny
    );
    assert_eq!(
        connect(&policy, sandbox.as_str(), None, "127.0.0.1:80").await,
        AuthorizationDecision::Deny
    );
    assert_eq!(
        connect(&policy, sandbox.as_str(), None, "169.254.169.254:80").await,
        AuthorizationDecision::Deny
    );
    assert_eq!(
        connect(&policy, sandbox.as_str(), None, "[fd00::2]:443").await,
        AuthorizationDecision::Deny
    );
    // Ordinary public egress is unaffected.
    assert_eq!(
        connect(&policy, sandbox.as_str(), Some("github.com"), "140.82.121.4:443").await,
        AuthorizationDecision::Allow
    );
    // Resolving the alias stays possible; it only reveals the gateway.
    assert_eq!(
        evaluate(
            &policy,
            sandbox.as_str(),
            action::DNS_QUERY,
            resource_kind::DOMAIN,
            "host.microsandbox.internal",
            "host.microsandbox.internal",
        )
        .await,
        AuthorizationDecision::Allow
    );
}

/// The sandbox names the alias itself, so naming it reaches nothing but the host: not an address
/// elsewhere on the Platform API's port, and not a resolver of the sandbox's own, which would
/// receive the query.
#[tokio::test(flavor = "local")]
async fn naming_the_platform_api_reaches_only_the_host() {
    let mut agent = support::agent("worker");
    agent.spec.network.allow = agent::NetworkAllow::Hosts(Vec::new());
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(&sandbox, &agent, []);
    policy.set_platform_endpoint("host.microsandbox.internal", 4_100);

    assert_eq!(
        connect_flagged(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "100.64.0.2:4100",
            true
        )
        .await,
        AuthorizationDecision::Allow
    );
    // Host-reserved ranges also hold other machines, such as VPN peers, which only the Network
    // Backend's flag tells apart from the host.
    for destination in [
        "100.100.100.100:4100",
        "[fd7a:115c:a1e0::1]:4100",
        "169.254.10.1:4100",
        "203.0.113.7:4100",
    ] {
        assert_eq!(
            connect(
                &policy,
                sandbox.as_str(),
                Some("host.microsandbox.internal"),
                destination
            )
            .await,
            AuthorizationDecision::Deny,
            "{destination}"
        );
    }
    let http = |destination: &'static str| {
        policy.evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(action::HTTP_REQUEST),
            resource: Resource::new(resource_kind::EXTERNAL_SERVICE, "host.microsandbox.internal"),
            context: AuthorizationContext::new()
                .with_attribute(context::SANDBOX_NAME, sandbox.as_str())
                .with_attribute(context::NETWORK_DESTINATION_ADDRESS, destination)
                .with_attribute(context::HTTP_AUTHORITY, "host.microsandbox.internal:4100"),
        })
    };
    assert_eq!(
        http("100.64.0.2:4100").await.expect("decision"),
        AuthorizationDecision::Allow
    );
    // The gateway need not be in a host-reserved range, as Microsandbox's own 172.16.0.0/12 isn't.
    // An HTTP request goes over a connection already decided, which carried the flag.
    assert_eq!(
        http("172.16.0.5:4100").await.expect("decision"),
        AuthorizationDecision::Allow
    );

    let query = |resolver: Option<&'static str>| {
        let mut authorization_context = AuthorizationContext::new()
            .with_attribute(context::SANDBOX_NAME, sandbox.as_str())
            .with_attribute(context::DNS_RECORD_TYPE, "TXT");
        if let Some(resolver) = resolver {
            authorization_context.insert(context::DNS_RESOLVER, resolver);
        }
        policy.evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(action::DNS_QUERY),
            resource: Resource::new(resource_kind::DOMAIN, "host.microsandbox.internal"),
            context: authorization_context,
        })
    };
    assert_eq!(query(None).await.expect("decision"), AuthorizationDecision::Allow);
    assert_eq!(
        query(Some("198.51.100.53:53")).await.expect("decision"),
        AuthorizationDecision::Deny
    );
}

#[tokio::test(flavor = "local")]
async fn host_destined_flag_denies_the_gateway_by_number_outside_guessed_ranges() {
    let agent = support::agent("worker");
    let policy = AgentPolicyEngine::new();
    let sandbox = SandboxName::new("agent-test-id").expect("Sandbox name");
    policy.set_agent(&sandbox, &agent, []);
    policy.set_platform_endpoint("host.microsandbox.internal", 4_100);

    // The default guest pool 172.16/12 is not a guessed reserved range, so
    // before the Backend flag an HTTP dial to the gateway with a plausible but
    // spoofed authority looked like ordinary egress and reached host loopback.
    assert_eq!(
        connect_flagged(&policy, sandbox.as_str(), Some("github.com"), "172.16.0.5:4100", false).await,
        AuthorizationDecision::Allow
    );
    // Flagged host-destined by the trusted Backend, the same dial is denied:
    // the spoofed authority is not the alias, so it cannot reach host loopback.
    assert_eq!(
        connect_flagged(&policy, sandbox.as_str(), Some("github.com"), "172.16.0.5:4100", true).await,
        AuthorizationDecision::Deny
    );
    // The genuine Platform API — alias on its registered port — still succeeds.
    assert_eq!(
        connect_flagged(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "172.16.0.5:4100",
            true
        )
        .await,
        AuthorizationDecision::Allow
    );
    // Flagged host-destined on another port is denied even via the alias.
    assert_eq!(
        connect_flagged(
            &policy,
            sandbox.as_str(),
            Some("host.microsandbox.internal"),
            "172.16.0.5:8080",
            true
        )
        .await,
        AuthorizationDecision::Deny
    );
}

async fn connect(
    policy: &AgentPolicyEngine,
    agent: &str,
    hostname: Option<&str>,
    destination: &str,
) -> AuthorizationDecision {
    connect_flagged(policy, agent, hostname, destination, false).await
}

async fn connect_flagged(
    policy: &AgentPolicyEngine,
    agent: &str,
    hostname: Option<&str>,
    destination: &str,
    destination_is_host: bool,
) -> AuthorizationDecision {
    let mut authorization_context = AuthorizationContext::new()
        .with_attribute(context::SANDBOX_NAME, agent)
        .with_attribute(context::NETWORK_DESTINATION_ADDRESS, destination)
        .with_attribute(context::NETWORK_DESTINATION_IS_HOST, destination_is_host);
    if let Some(hostname) = hostname {
        authorization_context.insert(context::NETWORK_HOSTNAME, hostname);
    }
    policy
        .evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(action::NETWORK_CONNECT),
            resource: Resource::new(
                resource_kind::EXTERNAL_SERVICE,
                hostname.map_or_else(|| destination.into(), str::to_owned),
            ),
            context: authorization_context,
        })
        .await
        .expect("policy decision")
}

async fn evaluate(
    policy: &AgentPolicyEngine,
    agent: &str,
    operation: &str,
    resource_kind: &str,
    resource: &str,
    host: &str,
) -> AuthorizationDecision {
    policy
        .evaluate(AuthorizationRequest {
            principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
            action: Action::new(operation),
            resource: Resource::new(resource_kind, resource),
            context: AuthorizationContext::new()
                .with_attribute(context::SANDBOX_NAME, agent)
                .with_attribute(context::HTTP_AUTHORITY, host),
        })
        .await
        .expect("policy decision")
}
