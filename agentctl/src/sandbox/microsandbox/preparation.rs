//! Host mediation setup for the Microsandbox Network Backend.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use crate::{
    Error,
    authorization::{PolicyUpdates, SecretHosts},
    control_plane, environment, harness, persistence,
};
use ::sandbox::{SandboxHandle, SandboxId, SandboxName, network::NetworkBackend as _};
use sandbox_microsandbox::{MicrosandboxNetworkBackend, SecretBinding};

/// Connects agent policy and host-owned secrets to the Microsandbox Network Backend.
pub(super) struct Preparation {
    database: persistence::Database,
    credentials: Rc<harness::AuthenticationManager>,
    policy: PolicyUpdates,
    network: Rc<MicrosandboxNetworkBackend>,
    admitted: RefCell<AdmittedNetworks>,
}

/// The network policy each running sandbox's open connections were admitted under, and whether
/// another has been installed since, which may have admitted connections the current one would
/// not. That stays so until the connections are revoked, so a pass that fails before revoking
/// them, or a policy changed back, leaves the next pass to do it.
#[derive(Default)]
struct AdmittedNetworks(BTreeMap<SandboxName, AdmittedNetwork>);

struct AdmittedNetwork {
    policy: crate::NetworkSpec,
    other_installed: bool,
}

impl AdmittedNetworks {
    /// Records that `network` now decides `sandbox`'s connections. A sandbox seen for the first
    /// time has none admitted under another.
    fn install(&mut self, sandbox: &SandboxName, network: &crate::NetworkSpec) {
        let admitted = self.0.entry(sandbox.clone()).or_insert_with(|| AdmittedNetwork {
            policy: network.clone(),
            other_installed: false,
        });
        admitted.other_installed |= admitted.policy != *network;
    }

    /// Whether `sandbox` may have connections open that a policy other than `network` admitted.
    fn differ(&self, sandbox: &SandboxName, network: &crate::NetworkSpec) -> bool {
        self.0
            .get(sandbox)
            .is_some_and(|admitted| admitted.other_installed || admitted.policy != *network)
    }

    /// Records that `sandbox`'s open connections were all admitted under `network`.
    fn admit(&mut self, sandbox: &SandboxName, network: &crate::NetworkSpec) {
        self.0.insert(
            sandbox.clone(),
            AdmittedNetwork {
                policy: network.clone(),
                other_installed: false,
            },
        );
    }

    fn forget(&mut self, sandbox: &SandboxName) {
        self.0.remove(sandbox);
    }
}

pub(super) struct PreparedNetwork {
    pub(super) environment: BTreeMap<String, String>,
    pub(super) harnesses: Vec<crate::Harness>,
    pub(super) logins: BTreeMap<crate::Harness, harness::LoginName>,
    /// What setup configures in the guest for the accounts the agent acts as.
    pub(super) accounts: crate::sandbox::AccountSetup,
}

struct PreparedHarnesses {
    secrets: Vec<harness::MediatedSecret>,
    installed: Vec<crate::Harness>,
    logins: BTreeMap<crate::Harness, harness::LoginName>,
}

impl Preparation {
    /// Creates the agent-side Microsandbox mediation adapter.
    #[must_use]
    pub(super) const fn new(
        database: persistence::Database,
        credentials: Rc<harness::AuthenticationManager>,
        policy: PolicyUpdates,
        network: Rc<MicrosandboxNetworkBackend>,
    ) -> Self {
        Self {
            database,
            credentials,
            policy,
            network,
            admitted: RefCell::new(AdmittedNetworks(BTreeMap::new())),
        }
    }
}

/// What a sandbox's running network needs before it serves the agent's current policy.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum NetworkStep {
    /// Restart it, to apply the secret bindings prepared for it. That ends its connections too.
    Restart,
    /// Close the connections an earlier policy admitted.
    Revoke,
    /// Nothing: it serves the current bindings and policy.
    Nothing,
}

impl NetworkStep {
    const fn of(stale_bindings: bool, earlier_policy: bool) -> Self {
        if stale_bindings {
            Self::Restart
        } else if earlier_policy {
            Self::Revoke
        } else {
            Self::Nothing
        }
    }
}

impl Preparation {
    /// What `sandbox`'s running network needs to serve `network`, its agent's current policy.
    pub(super) fn network_step(
        &self,
        name: &SandboxName,
        sandbox: &SandboxId,
        network: &crate::NetworkSpec,
    ) -> NetworkStep {
        NetworkStep::of(
            self.network.has_stale_secret_bindings(sandbox),
            self.admitted.borrow().differ(name, network),
        )
    }

    pub(super) async fn restart_network(&self, sandbox: &SandboxHandle) -> Result<(), Error> {
        self.network.stop(sandbox.id()).await.map_err(Error::from)
    }

    /// Closes the connections `sandbox`'s network admitted, so that each one opened again is
    /// decided by the current policy. A network that is not running has none.
    pub(super) async fn revoke_connections(&self, sandbox: &SandboxHandle) -> Result<(), Error> {
        match self.network.revoke_all(sandbox.id()).await {
            Err(error) if error.is_not_found() => Ok(()),
            result => result.map_err(Error::from),
        }
    }

    /// Records that `sandbox`'s network has no connection open under a policy other than
    /// `network`, once it restarted or revoked them.
    pub(super) fn network_admitted(&self, sandbox: &SandboxName, network: &crate::NetworkSpec) {
        self.admitted.borrow_mut().admit(sandbox, network);
    }

    pub(super) async fn prepare(&self, record: &control_plane::AgentRecord) -> Result<PreparedNetwork, Error> {
        let sandbox_name = record.sandbox_name()?;
        let result = async {
            let (mut bindings, mut policy) = self.prepare_secrets(record).await?;
            let PreparedHarnesses {
                secrets: mut managed_secrets,
                installed,
                logins,
            } = self.prepare_harnesses(record).await?;
            let accounts = crate::accounts::prepare(&self.database, &record.agent.spec).await?;
            managed_secrets.extend(accounts.secrets);
            policy.extend(
                managed_secrets
                    .iter()
                    .map(|secret| SecretHosts::new(secret.environment, secret.allowed_hosts.clone())),
            );
            self.policy.set_agent(&sandbox_name, &record.agent, policy).await?;
            self.admitted
                .borrow_mut()
                .install(&sandbox_name, &record.agent.spec.network);
            for secret in managed_secrets {
                bindings.push(SecretBinding::with_placeholder(
                    secret.environment,
                    &secret.placeholder,
                    secret.reference,
                )?);
            }
            let guest_environment = bindings
                .iter()
                .map(|binding| {
                    let (name, value) = binding.guest_environment();
                    (name.to_owned(), value.to_owned())
                })
                .collect();
            self.network.set_secret_bindings(sandbox_name.clone(), bindings)?;
            Ok(PreparedNetwork {
                environment: guest_environment,
                harnesses: installed,
                logins,
                accounts: accounts.setup,
            })
        }
        .await;
        if result.is_err() {
            // The pass's own error says more than one from removing what it set up.
            let _ = self.remove(&sandbox_name).await;
        }
        result
    }

    /// The bindings of the secrets `record`'s manifest declares, with where each may be
    /// substituted. Those from the environment file are saved as the agent's own; a stored secret is
    /// bound by its name, so replacing it reaches the agent at once.
    async fn prepare_secrets(
        &self,
        record: &control_plane::AgentRecord,
    ) -> Result<(Vec<SecretBinding>, Vec<SecretHosts>), Error> {
        let secrets = &record.agent.spec.secrets;
        let from_file = || secrets.iter().filter(|secret| secret.source().is_some());
        let environment = if from_file().next().is_none() {
            BTreeMap::new()
        } else if from_file().all(|secret| secret.optional) {
            environment::read_or_empty(&record.env_file_path()).await?
        } else {
            environment::read(&record.env_file_path()).await?
        };
        let mut bindings = Vec::with_capacity(secrets.len());
        let mut policy = Vec::with_capacity(secrets.len());
        let mut configured_secrets = Vec::with_capacity(secrets.len());
        let mut secret_writes = Vec::with_capacity(secrets.len());
        for (index, secret) in secrets.iter().enumerate() {
            if let Some(name) = &secret.name {
                if crate::secrets::stored(&self.database, name).await?.is_none() {
                    if secret.optional {
                        continue;
                    }
                    let name = name.to_ascii_lowercase();
                    let hosts = if secret.allowed_hosts.is_empty() {
                        "HOST".to_owned()
                    } else {
                        secret.allowed_hosts.join(",")
                    };
                    return Err(Error::Invalid(format!(
                        "spec.secrets[{index}] uses the secret {name}, which is not stored here; run \
                         `agentctl secret set {name} --hosts {hosts}`"
                    )));
                }
                bindings.push(SecretBinding::with_placeholder(
                    &secret.environment,
                    secret.inert_value(),
                    crate::secrets::reference(name),
                )?);
                policy.push(SecretHosts {
                    environment: secret.environment.clone(),
                    // Without hosts of its own, the manifest leaves them to the stored secret.
                    hosts: if secret.allowed_hosts.is_empty() {
                        Vec::new()
                    } else {
                        vec![secret.allowed_hosts.clone()]
                    },
                    stored: Some(name.to_ascii_lowercase()),
                });
                continue;
            }
            let Some(value) = secret_value(&environment, secret)? else {
                continue;
            };
            configured_secrets.push(secret);
            secret_writes.push(persistence::StoredSecret {
                name: secret.environment.clone(),
                value: zeroize::Zeroizing::new(value.as_bytes().to_vec()),
            });
        }
        let references = self.database.replace_agent_secrets(record.id, secret_writes).await?;
        for (secret, reference) in configured_secrets.into_iter().zip(references) {
            bindings.push(SecretBinding::with_placeholder(
                &secret.environment,
                secret.inert_value(),
                reference,
            )?);
            policy.push(SecretHosts::new(
                secret.environment.clone(),
                secret.allowed_hosts.clone(),
            ));
        }
        Ok((bindings, policy))
    }

    /// The harness installations `record`'s sandbox gets, with the host login each mediated one uses
    /// and the mediated secrets they need.
    async fn prepare_harnesses(&self, record: &control_plane::AgentRecord) -> Result<PreparedHarnesses, Error> {
        let mut managed_secrets = Vec::new();
        let mut managed_environments = BTreeMap::new();
        let mut managed_placeholders = BTreeMap::new();
        let mut installed = Vec::with_capacity(record.agent.spec.harnesses.len());
        let mut logins = BTreeMap::new();
        for installation in &record.agent.spec.harnesses {
            if installation.auth == harness::HarnessAuthMode::None {
                installed.push(installation.kind);
                continue;
            }
            // Re-evaluated every pass, so signing in later installs it with no manifest change.
            let Some(login) = self.credentials.assign_login(record, installation).await? else {
                if installation.optional {
                    continue;
                }
                return Err(harness::no_login_stored(installation.kind, &installation.login));
            };
            self.credentials.refresh_login(installation.kind, &login).await;
            installed.push(installation.kind);
            for secret in harness::prepare(installation.kind, &login, &self.database).await? {
                if let Some(existing) = managed_environments.insert(secret.environment, installation.kind.as_str()) {
                    return Err(Error::Invalid(format!(
                        "harnesses {:?} and {:?} use the same managed environment {:?}",
                        existing,
                        installation.kind.as_str(),
                        secret.environment
                    )));
                }
                if let Some(existing) =
                    managed_placeholders.insert(secret.placeholder.clone(), installation.kind.as_str())
                {
                    return Err(Error::Invalid(format!(
                        "harnesses {:?} and {:?} use the same managed placeholder {:?}",
                        existing,
                        installation.kind.as_str(),
                        secret.placeholder
                    )));
                }
                managed_secrets.push(secret);
            }
            logins.insert(installation.kind, login);
        }
        Ok(PreparedHarnesses {
            secrets: managed_secrets,
            installed,
            logins,
        })
    }

    async fn remove(&self, sandbox: &SandboxName) -> Result<(), Error> {
        self.network.remove_secret_bindings(sandbox);
        self.policy.remove_agent(sandbox).await
    }

    /// Forgets `sandbox` once it is released, with the connections its network had open.
    pub(super) async fn release(&self, sandbox: &SandboxName) -> Result<(), Error> {
        self.admitted.borrow_mut().forget(sandbox);
        self.remove(sandbox).await
    }
}

fn secret_value<'a>(
    environment: &'a BTreeMap<String, zeroize::Zeroizing<String>>,
    secret: &crate::SecretSpec,
) -> Result<Option<&'a str>, Error> {
    let source = secret.source().unwrap_or(&secret.environment);
    if secret.optional {
        Ok(environment::optional(environment, source))
    } else {
        Ok(Some(environment::required(environment, source)?))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::secret_value;
    use crate::{SecretSpec, control_plane::AgentStore as _, harness};
    use ::sandbox::secret_store::SecretStore as _;
    use std::{collections::BTreeMap, rc::Rc};
    use zeroize::Zeroizing;

    const MANIFEST: &str = "apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: NAME
spec:
  sandbox:
    image:
      type: reference
      reference: example.com/agent:latest
    platform:
      os: linux
    resources:
      cpu: \"1\"
      memory: \"1Gi\"
      rootFilesystem:
        capacity: \"8Gi\"
        mode: layered
  home:
    source: home
  harnesses: []
  network:
    mode: mediated
    allow: all
";

    /// A Preparation over `database` whose policy is decided by `engine`, on this thread, and the
    /// engine.
    fn preparation(
        database: &crate::persistence::Database,
        engine: crate::authorization::AgentPolicyEngine,
    ) -> (super::Preparation, Rc<crate::authorization::AgentPolicyEngine>) {
        let (policy, updates) = crate::authorization::PolicyUpdates::channel();
        let engine = Rc::new(engine);
        engine.clone().serve(updates);
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(
            sandbox_microsandbox::Decider {
                policy: engine.clone(),
                secret_store: None,
            },
        ));
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        (
            super::Preparation::new(database.clone(), credentials, policy, network),
            engine,
        )
    }

    /// An agent named `name` whose one harness may use any stored login.
    fn record(name: &str, id: &str) -> crate::control_plane::AgentRecord {
        let mut agent: crate::Agent = serde_yaml_ng::from_str(&MANIFEST.replace("NAME", name)).expect("manifest");
        agent.metadata.generation = 1;
        agent.spec.harnesses.push(
            serde_json::from_value(serde_json::json!({"type": harness::test_harness(), "login": "any"}))
                .expect("installation"),
        );
        crate::control_plane::AgentRecord {
            id: id.parse().expect("agent ID"),
            source_directory: std::path::PathBuf::from("/source"),
            manifest_path: None,
            env_file: None,
            agent,
            optional_mount_sources: std::collections::BTreeMap::new(),
            volume_revisions: std::collections::BTreeMap::new(),
            origin: crate::Origin::Local,
            lifetime: crate::Lifetime::UntilDeleted,
            bindings: std::collections::BTreeMap::new(),
            exposed_files: std::collections::BTreeSet::new(),
        }
    }

    /// Stores `record` as running in a sandbox whose harness uses the logins `prepared` chose.
    async fn store_materialized(
        database: &crate::persistence::Database,
        mut record: crate::control_plane::AgentRecord,
        prepared: &super::PreparedNetwork,
    ) {
        record.agent.status.sandbox = Some(crate::sandbox::Assignment::Materialized {
            provider: crate::sandbox::ProviderId::new("microsandbox").expect("Provider ID"),
            id: "3f978c33-4d43-4ea4-b58d-10b90ef166af".parse().expect("sandbox ID"),
            harnesses: prepared.harnesses.clone(),
            skills: std::collections::BTreeMap::new(),
            logins: prepared.logins.clone(),
        });
        let status = record.agent.status.clone();
        database.put(record.clone(), 0).await.expect("store agent");
        database
            .update_status(record.id, 1, status)
            .await
            .expect("store status");
    }

    /// Agents applied together are prepared concurrently, each before any has recorded its
    /// sandbox, and still spread out.
    #[tokio::test(flavor = "local")]
    async fn agents_prepared_at_once_get_different_logins() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let (preparation, _) = preparation(&database, crate::authorization::AgentPolicyEngine::new());
        let (first, second) = (
            record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c"),
            record("second", "7b4d2a1f-3c8e-4f6a-8d95-1a2b3c4d5e6f"),
        );
        for agent in [&first, &second] {
            database.put(agent.clone(), 0).await.expect("store agent");
        }

        let (first, second) = tokio::join!(preparation.prepare(&first), preparation.prepare(&second));

        let login = |prepared: super::PreparedNetwork| prepared.logins[&harness::test_harness()].to_string();
        let mut logins = [
            login(first.expect("prepare first")),
            login(second.expect("prepare second")),
        ];
        logins.sort();
        assert_eq!(logins, ["a", "b"]);
    }

    async fn stop(database: &crate::persistence::Database, id: crate::AgentId) {
        let mut record = database.get(id).await.expect("agent");
        record.agent.spec.set_run_state(crate::RunState::Stopped);
        database.put(record, 1).await.expect("stop agent");
    }

    #[tokio::test(flavor = "local")]
    async fn agents_spread_over_the_logins_they_may_use_and_keep_the_one_they_got() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let (preparation, _) = preparation(&database, crate::authorization::AgentPolicyEngine::new());
        let login = |prepared: &super::PreparedNetwork| prepared.logins[&harness::test_harness()].to_string();

        let first = record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c");
        let prepared = preparation.prepare(&first).await.expect("prepare first");
        assert_eq!(login(&prepared), "a", "on a tie, names decide");
        store_materialized(&database, first.clone(), &prepared).await;
        let second = record("second", "7b4d2a1f-3c8e-4f6a-8d95-1a2b3c4d5e6f");
        let prepared = preparation.prepare(&second).await.expect("prepare second");
        assert_eq!(login(&prepared), "b", "the first agent uses a");
        store_materialized(&database, second.clone(), &prepared).await;

        stop(&database, second.id).await;
        let third = record("third", "8c5e3b2a-4d9f-4a7b-9ea6-2b3c4d5e6f70");
        let prepared = preparation.prepare(&third).await.expect("prepare third");
        assert_eq!(login(&prepared), "b", "a stopped agent's login counts as free");
        store_materialized(&database, third.clone(), &prepared).await;

        stop(&database, first.id).await;
        let kept = preparation
            .prepare(&database.get(third.id).await.expect("third agent"))
            .await
            .expect("prepare third again");
        assert_eq!(
            login(&kept),
            "b",
            "an agent keeps its login though a is now free and first by name"
        );
    }

    /// An agent given another login than its sandbox records keeps the recorded one refreshed too,
    /// as its sandbox uses it until it is set up with the new one.
    #[tokio::test(flavor = "local")]
    async fn a_login_a_sandbox_still_uses_is_refreshed_ahead_with_the_one_it_was_given() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let (policy, updates) = crate::authorization::PolicyUpdates::channel();
        let engine = Rc::new(crate::authorization::AgentPolicyEngine::new());
        engine.clone().serve(updates);
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(
            sandbox_microsandbox::Decider {
                policy: engine,
                secret_store: None,
            },
        ));
        let preparation = super::Preparation::new(database.clone(), credentials.clone(), policy, network);
        let agent = record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c");
        let mut prepared = preparation.prepare(&agent).await.expect("prepare");
        assert_eq!(prepared.logins[&harness::test_harness()].to_string(), "a");
        prepared
            .logins
            .insert(harness::test_harness(), harness::LoginName::new("b").expect("name"));
        store_materialized(&database, agent, &prepared).await;

        let running = credentials
            .running_logins(harness::test_harness())
            .await
            .expect("logins");
        let names: Vec<_> = running.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["a", "b"]);
    }

    /// The logins refreshed ahead of expiry are those of the agents that run.
    #[tokio::test(flavor = "local")]
    async fn only_the_logins_of_running_agents_are_refreshed_ahead() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b", "unused"] {
            harness::store_test_login(name, &database).await;
        }
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let (policy, updates) = crate::authorization::PolicyUpdates::channel();
        let engine = Rc::new(crate::authorization::AgentPolicyEngine::new());
        engine.clone().serve(updates);
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(
            sandbox_microsandbox::Decider {
                policy: engine,
                secret_store: None,
            },
        ));
        let preparation = super::Preparation::new(database.clone(), credentials.clone(), policy, network);
        let (first, second) = (
            record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c"),
            record("second", "7b4d2a1f-3c8e-4f6a-8d95-1a2b3c4d5e6f"),
        );
        for agent in [&first, &second] {
            let prepared = preparation.prepare(agent).await.expect("prepare");
            store_materialized(&database, agent.clone(), &prepared).await;
        }

        let logins = |names: &[&str]| {
            names
                .iter()
                .map(|name| harness::LoginName::new(*name).expect("name"))
                .collect()
        };
        let running = credentials
            .running_logins(harness::test_harness())
            .await
            .expect("logins");
        assert_eq!(running, logins(&["a", "b"]));
        stop(&database, second.id).await;
        let running = credentials
            .running_logins(harness::test_harness())
            .await
            .expect("logins");
        assert_eq!(running, logins(&["a"]));
    }

    const GITHUB_MANIFEST: &str = "apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: worker
spec:
  sandbox:
    image:
      type: reference
      reference: example.com/agent:latest
    platform:
      os: linux
    resources:
      cpu: \"1\"
      memory: \"1Gi\"
      rootFilesystem:
        capacity: \"8Gi\"
        mode: layered
  harnesses: []
  accounts:
    - type: github
      login: Digdir
";
    const ACCOUNTS: &str = "  accounts:\n    - type: github\n      login: Digdir\n";

    /// Stores a fake GitHub token as `name`, expiring at `expires_at` (RFC 3339) when given.
    async fn store_token(database: &crate::persistence::Database, name: &str, expires_at: Option<&str>) {
        let expiry = expires_at
            .map(|at| format!(r#","expiresAt":"{at}""#))
            .unwrap_or_default();
        database
            .put_provider_account(crate::persistence::ProviderAccountWrite {
                provider: format!("github/{name}"),
                credentials: vec![crate::persistence::StoredSecret {
                    name: format!("github-access-token/{name}"),
                    value: Zeroizing::new(format!("github_pat_FAKE_{name}").into_bytes()),
                }],
                metadata_json: format!(
                    r#"{{"login":"{name}-bot","name":"{name} bot","email":"1+{name}-bot@users.noreply.github.com","kind":"fineGrained"{expiry}}}"#
                ),
            })
            .await
            .expect("store token");
    }

    /// A Preparation over a database holding GitHub tokens for `digdir` and `altinn`.
    async fn with_tokens(
        directory: &tempfile::TempDir,
    ) -> (
        super::Preparation,
        Rc<crate::authorization::AgentPolicyEngine>,
        crate::persistence::Database,
    ) {
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["digdir", "altinn"] {
            store_token(&database, name, None).await;
        }
        let (preparation, policy) = preparation(
            &database,
            crate::authorization::AgentPolicyEngine::new().with_stored_secrets(database.clone()),
        );
        (preparation, policy, database)
    }

    /// Whether the policy allows the agent's sandbox `action` on `resource` in a request to `host`.
    async fn allows(
        policy: &crate::authorization::AgentPolicyEngine,
        record: &crate::control_plane::AgentRecord,
        (action, resource_kind, resource): (&str, &str, &str),
        host: &str,
    ) -> bool {
        use sandbox_authorization::{
            Action, AuthorizationContext, AuthorizationDecision, AuthorizationRequest, PolicyEngine as _, Principal,
            Resource,
            vocabulary::{context, principal_kind},
        };
        let sandbox = record.sandbox_name().expect("Sandbox name");
        let decision = policy
            .evaluate(AuthorizationRequest {
                principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
                action: Action::new(action),
                resource: Resource::new(resource_kind, resource),
                context: AuthorizationContext::new()
                    .with_attribute(context::SANDBOX_NAME, sandbox.as_str())
                    .with_attribute(context::HTTP_AUTHORITY, host),
            })
            .await
            .expect("policy decision");
        decision == AuthorizationDecision::Allow
    }

    /// Whether the policy lets the agent's sandbox use `secret` in a request to `host`.
    async fn may_use(
        policy: &crate::authorization::AgentPolicyEngine,
        record: &crate::control_plane::AgentRecord,
        secret: &str,
        host: &str,
    ) -> bool {
        use sandbox_authorization::vocabulary::{action, resource_kind};
        allows(
            policy,
            record,
            (action::SECRET_USE, resource_kind::SECRET, secret),
            host,
        )
        .await
    }

    /// Whether the policy lets the agent's sandbox send an HTTP request to `host`.
    async fn may_request(
        policy: &crate::authorization::AgentPolicyEngine,
        record: &crate::control_plane::AgentRecord,
        host: &str,
    ) -> bool {
        use sandbox_authorization::vocabulary::{action, resource_kind};
        allows(
            policy,
            record,
            (action::HTTP_REQUEST, resource_kind::EXTERNAL_SERVICE, host),
            host,
        )
        .await
    }

    fn github_record(directory: &tempfile::TempDir, manifest: &str) -> crate::control_plane::AgentRecord {
        crate::control_plane::AgentRecord {
            id: "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c".parse().expect("Agent ID"),
            source_directory: directory.path().to_path_buf(),
            manifest_path: None,
            env_file: None,
            // Decoded without validation, which needs a harness this module does not name.
            agent: serde_yaml_ng::from_str(manifest).expect("manifest"),
            optional_mount_sources: std::collections::BTreeMap::new(),
            volume_revisions: std::collections::BTreeMap::new(),
            origin: crate::Origin::Local,
            lifetime: crate::Lifetime::UntilDeleted,
            bindings: std::collections::BTreeMap::new(),
            exposed_files: std::collections::BTreeSet::new(),
        }
    }

    #[tokio::test(flavor = "local")]
    async fn a_github_account_mediates_its_token_for_githubs_hosts_only() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, _database) = with_tokens(&directory).await;
        let record = github_record(&directory, GITHUB_MANIFEST);

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert_eq!(
            prepared.environment,
            BTreeMap::from([("GITHUB_TOKEN".into(), "github_pat_AGENT_MEDIATED_GITHUB_TOKEN".into())])
        );
        assert_eq!(
            prepared.accounts.git_identity.map(|identity| identity.email).as_deref(),
            Some("1+digdir-bot@users.noreply.github.com")
        );
        assert!(
            !prepared.environment.values().any(|value| value.contains("FAKE")),
            "no real token enters the guest: {:?}",
            prepared.environment
        );
        for host in ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"] {
            assert!(may_use(&policy, &record, "GITHUB_TOKEN", host).await, "{host}");
        }
        for host in [
            "x.github.com",
            "github.com.evil.example",
            "evil.example",
            "raw.githubusercontent.com",
        ] {
            assert!(!may_use(&policy, &record, "GITHUB_TOKEN", host).await, "{host}");
        }
    }

    #[tokio::test(flavor = "local")]
    async fn an_agent_without_accounts_gets_no_github_token_even_when_tokens_are_stored() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, _database) = with_tokens(&directory).await;
        let record = github_record(&directory, &GITHUB_MANIFEST.replace(ACCOUNTS, ""));

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert!(prepared.environment.is_empty(), "{:?}", prepared.environment);
        assert!(!may_use(&policy, &record, "GITHUB_TOKEN", "github.com").await);
    }

    /// A logout, or a token never stored, ends only GitHub access: preparation still succeeds, so
    /// the agent keeps its policy and every other host, and neither the binding nor the
    /// environment changes, so nothing restarts when a token comes, goes or changes account.
    #[tokio::test(flavor = "local")]
    async fn logging_out_ends_github_access_and_nothing_else() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, database) = with_tokens(&directory).await;
        let record = github_record(&directory, GITHUB_MANIFEST);
        let before = preparation.prepare(&record).await.expect("prepare");
        assert!(may_request(&policy, &record, "models.example").await);

        crate::github::Authentication::new(database.clone())
            .forget("digdir")
            .await
            .expect("logout")
            .expect("a token was stored");
        let prepared = preparation.prepare(&record).await.expect("prepare without a token");

        assert_eq!(prepared.environment, before.environment);
        assert_eq!(prepared.accounts.git_identity, None);
        for host in ["models.example", "github.com"] {
            assert!(may_request(&policy, &record, host).await, "{host}");
        }
        assert!(may_use(&policy, &record, "GITHUB_TOKEN", "github.com").await);
        let binding = crate::github::mediated_token("digdir").expect("binding");
        assert!(
            database.resolve(&binding.reference).await.is_err(),
            "nothing left to substitute"
        );

        store_token(&database, "digdir", None).await;
        let relogged = preparation.prepare(&record).await.expect("prepare after a login");
        assert_eq!(relogged.environment, before.environment);
        assert!(relogged.accounts.git_identity.is_some());
    }

    /// GitHub enforces a token's expiry itself, so a stored expiry that has passed changes nothing.
    #[tokio::test(flavor = "local")]
    async fn an_expired_token_still_binds() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, database) = with_tokens(&directory).await;
        store_token(&database, "digdir", Some("2001-09-09T01:46:40Z")).await;
        let record = github_record(&directory, GITHUB_MANIFEST);

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert!(
            prepared.environment.contains_key("GITHUB_TOKEN"),
            "{:?}",
            prepared.environment
        );
        assert!(may_use(&policy, &record, "GITHUB_TOKEN", "github.com").await);
        let binding = crate::github::mediated_token("digdir").expect("binding");
        let resolved = database.resolve(&binding.reference).await.expect("resolve");
        assert_eq!(resolved.expose(), b"github_pat_FAKE_digdir");
    }

    /// An agent without accounts whose `AIVAR_API_KEY` is the stored secret `Aivar`, sent to
    /// `api.aivar.no` and `models.example`.
    fn stored_secret_record(directory: &tempfile::TempDir, optional: bool) -> crate::control_plane::AgentRecord {
        let mut record = github_record(directory, &GITHUB_MANIFEST.replace(ACCOUNTS, ""));
        record.agent.spec.secrets.push(SecretSpec {
            environment: "AIVAR_API_KEY".into(),
            optional,
            placeholder: None,
            allowed_hosts: vec!["api.aivar.no".into(), "models.example".into()],
            source: None,
            name: Some("Aivar".into()),
        });
        record
    }

    /// A stored secret is read where it is stored, so replacing it reaches the agent without preparing
    /// it again, and it is used only at a host both its manifest and the secret allow. It needs no
    /// environment file.
    #[tokio::test(flavor = "local")]
    async fn a_stored_secret_is_used_where_the_manifest_and_the_secret_both_allow() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, database) = with_tokens(&directory).await;
        let secrets = crate::secrets::Secrets::new(database.clone());
        secrets
            .set(
                "aivar",
                Zeroizing::new("sk-FAKE-first".into()),
                Some(vec!["*.aivar.no".into()]),
            )
            .await
            .expect("store the secret");
        let record = stored_secret_record(&directory, false);

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert_eq!(
            prepared.environment,
            BTreeMap::from([("AIVAR_API_KEY".into(), "$AGENT_SECRET_AIVAR_API_KEY".into())])
        );
        assert!(may_use(&policy, &record, "AIVAR_API_KEY", "api.aivar.no").await);
        for host in ["models.example", "www.aivar.no", "evil.example"] {
            assert!(!may_use(&policy, &record, "AIVAR_API_KEY", host).await, "{host}");
        }
        let reference = crate::secrets::reference("aivar");
        assert_eq!(
            database.resolve(&reference).await.expect("resolve").expose(),
            b"sk-FAKE-first"
        );
        secrets
            .set("aivar", Zeroizing::new("sk-FAKE-second".into()), None)
            .await
            .expect("replace the value");
        assert_eq!(
            database.resolve(&reference).await.expect("resolve").expose(),
            b"sk-FAKE-second"
        );

        // A host the secret stops allowing gets no value from then on, before the agent is prepared
        // again, so a value set together with the change never reaches it.
        secrets
            .set(
                "aivar",
                Zeroizing::new("sk-FAKE-third".into()),
                Some(vec!["models.example".into()]),
            )
            .await
            .expect("move the secret");
        assert!(!may_use(&policy, &record, "AIVAR_API_KEY", "api.aivar.no").await);
        assert!(may_use(&policy, &record, "AIVAR_API_KEY", "models.example").await);
        secrets.remove("aivar").await.expect("remove").expect("it was stored");
        assert!(!may_use(&policy, &record, "AIVAR_API_KEY", "models.example").await);
    }

    /// A manifest that names no hosts for a stored secret leaves them to the secret.
    #[tokio::test(flavor = "local")]
    async fn a_stored_secret_without_hosts_in_the_manifest_goes_where_the_secret_allows() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, database) = with_tokens(&directory).await;
        let secrets = crate::secrets::Secrets::new(database);
        let mut record = stored_secret_record(&directory, false);
        record.agent.spec.secrets[0].allowed_hosts.clear();

        let Err(error) = preparation.prepare(&record).await else {
            panic!("a required secret that is not stored fails");
        };
        assert!(
            error.to_string().contains("`agentctl secret set aivar --hosts HOST`"),
            "{error}"
        );
        secrets
            .set(
                "aivar",
                Zeroizing::new("sk-FAKE".into()),
                Some(vec!["*.aivar.no".into()]),
            )
            .await
            .expect("store the secret");
        preparation.prepare(&record).await.expect("prepare");

        assert!(may_use(&policy, &record, "AIVAR_API_KEY", "api.aivar.no").await);
        assert!(!may_use(&policy, &record, "AIVAR_API_KEY", "models.example").await);
        secrets
            .set(
                "aivar",
                Zeroizing::new("sk-FAKE".into()),
                Some(vec!["models.example".into()]),
            )
            .await
            .expect("move the secret");
        assert!(!may_use(&policy, &record, "AIVAR_API_KEY", "api.aivar.no").await);
        assert!(may_use(&policy, &record, "AIVAR_API_KEY", "models.example").await);
    }

    #[tokio::test(flavor = "local")]
    async fn a_secret_that_is_not_stored_fails_a_required_binding_and_leaves_out_an_optional_one() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy, _database) = with_tokens(&directory).await;

        let error = preparation
            .prepare(&stored_secret_record(&directory, false))
            .await
            .err()
            .expect("a required secret that is not stored fails");
        assert!(
            error
                .to_string()
                .contains("run `agentctl secret set aivar --hosts api.aivar.no,models.example`"),
            "{error}"
        );

        let record = stored_secret_record(&directory, true);
        let prepared = preparation.prepare(&record).await.expect("prepare");
        assert!(prepared.environment.is_empty(), "{:?}", prepared.environment);
        assert!(!may_use(&policy, &record, "AIVAR_API_KEY", "api.aivar.no").await);
    }

    fn secret(optional: bool) -> SecretSpec {
        SecretSpec {
            environment: "API_TOKEN".into(),
            optional,
            placeholder: None,
            allowed_hosts: vec!["example.com".into()],
            source: None,
            name: None,
        }
    }

    #[test]
    fn an_optional_secret_omits_a_missing_or_empty_value_and_a_required_one_rejects_it() -> Result<(), crate::Error> {
        let mut environment = BTreeMap::new();
        assert_eq!(secret_value(&environment, &secret(true))?, None);
        assert!(
            matches!(secret_value(&environment, &secret(false)), Err(crate::Error::Invalid(message)) if message.contains("API_TOKEN"))
        );

        environment.insert("API_TOKEN".into(), Zeroizing::new(String::new()));
        assert_eq!(secret_value(&environment, &secret(true))?, None);

        environment.insert("API_TOKEN".into(), Zeroizing::new("token".into()));
        assert_eq!(secret_value(&environment, &secret(true))?, Some("token"));
        assert_eq!(secret_value(&environment, &secret(false))?, Some("token"));
        Ok(())
    }

    #[test]
    fn restarting_the_network_comes_before_revoking_its_connections() {
        use super::NetworkStep;
        assert_eq!(NetworkStep::of(true, true), NetworkStep::Restart);
        assert_eq!(NetworkStep::of(true, false), NetworkStep::Restart);
        assert_eq!(NetworkStep::of(false, true), NetworkStep::Revoke);
        assert_eq!(NetworkStep::of(false, false), NetworkStep::Nothing);
    }

    #[test]
    fn a_policy_change_stays_pending_until_its_connections_are_admitted_under_it() {
        let sandbox = ::sandbox::SandboxName::new("agent-test-id").expect("Sandbox name");
        let earlier = crate::NetworkSpec {
            deny: vec!["exfil.example".into()],
            ..crate::NetworkSpec::default()
        };
        let broader = crate::NetworkSpec::default();
        let mut admitted = super::AdmittedNetworks::default();
        assert!(
            !admitted.differ(&sandbox, &earlier),
            "a network not seen yet has nothing open"
        );

        admitted.install(&sandbox, &earlier);
        assert!(
            !admitted.differ(&sandbox, &earlier),
            "the first policy admitted what is open"
        );
        admitted.admit(&sandbox, &earlier);

        admitted.install(&sandbox, &broader);
        assert!(admitted.differ(&sandbox, &broader));
        // The pass fails before revoking, and the broader policy admits a connection. Changing back
        // must still close it.
        admitted.install(&sandbox, &earlier);
        assert!(admitted.differ(&sandbox, &earlier));

        admitted.admit(&sandbox, &earlier);
        assert!(!admitted.differ(&sandbox, &earlier));

        admitted.forget(&sandbox);
        assert!(!admitted.differ(&sandbox, &broader));
    }
}
