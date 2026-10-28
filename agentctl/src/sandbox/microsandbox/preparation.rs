//! Host mediation setup for the Microsandbox Network Backend.

use std::{collections::BTreeMap, rc::Rc};

use crate::{
    Error,
    authorization::AgentPolicyEngine,
    control_plane::{self, AgentStore as _},
    environment, harness, persistence,
};
use ::sandbox::{SandboxHandle, SandboxId, SandboxName, network::NetworkBackend as _};
use sandbox_microsandbox::{MicrosandboxNetworkBackend, SecretBinding};

/// Connects agent policy and host-owned secrets to the Microsandbox Network Backend.
pub(super) struct Preparation {
    database: persistence::Database,
    policy: Rc<AgentPolicyEngine>,
    network: Rc<MicrosandboxNetworkBackend>,
}

pub(super) struct PreparedNetwork {
    pub(super) environment: BTreeMap<String, String>,
    pub(super) harnesses: Vec<crate::Harness>,
    pub(super) logins: BTreeMap<crate::Harness, harness::LoginName>,
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
        policy: Rc<AgentPolicyEngine>,
        network: Rc<MicrosandboxNetworkBackend>,
    ) -> Self {
        Self {
            database,
            policy,
            network,
        }
    }
}

impl Preparation {
    /// Whether the sandbox's running Network must restart to apply the bindings prepared for it.
    pub(super) fn network_is_stale(&self, sandbox: &SandboxId) -> bool {
        self.network.has_stale_secret_bindings(sandbox)
    }

    pub(super) async fn restart_network(&self, sandbox: &SandboxHandle) -> Result<(), Error> {
        self.network.stop(sandbox.id()).await.map_err(Error::from)
    }

    pub(super) async fn prepare(&self, record: &control_plane::AgentRecord) -> Result<PreparedNetwork, Error> {
        let sandbox_name = record.sandbox_name()?;
        let result = async {
            let secrets = &record.agent.spec.secrets;
            let environment = if secrets.is_empty() {
                BTreeMap::new()
            } else if secrets.iter().all(|secret| secret.optional) {
                environment::read_or_empty(&record.env_file_path()).await?
            } else {
                environment::read(&record.env_file_path()).await?
            };
            let mut configured_secrets = Vec::with_capacity(secrets.len());
            let mut secret_writes = Vec::with_capacity(secrets.len());
            for secret in secrets {
                let value = secret_value(&environment, secret)?;
                let Some(value) = value else {
                    continue;
                };
                configured_secrets.push(secret);
                secret_writes.push(persistence::StoredSecret {
                    name: secret.environment.clone(),
                    value: zeroize::Zeroizing::new(value.as_bytes().to_vec()),
                });
            }
            let references = self.database.replace_agent_secrets(record.id, secret_writes).await?;
            let mut bindings = Vec::with_capacity(configured_secrets.len() + 1);
            for (secret, reference) in configured_secrets.into_iter().zip(references) {
                let binding = SecretBinding::with_placeholder(&secret.environment, secret.inert_value(), reference)?;
                bindings.push(binding);
            }
            let PreparedHarnesses {
                secrets: managed_secrets,
                installed,
                logins,
            } = self.prepare_harnesses(record).await?;
            self.policy.set_agent(
                &sandbox_name,
                &record.agent,
                managed_secrets
                    .iter()
                    .map(|secret| (secret.environment.into(), secret.allowed_hosts.clone())),
            );
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
            })
        }
        .await;
        if result.is_err() {
            self.remove(&sandbox_name);
        }
        result
    }

    /// The harness installations `record`'s sandbox gets, with the host login each mediated one uses
    /// and the mediated secrets they need.
    async fn prepare_harnesses(&self, record: &control_plane::AgentRecord) -> Result<PreparedHarnesses, Error> {
        let mut managed_secrets = Vec::new();
        let mut managed_environments = BTreeMap::new();
        let mut managed_placeholders = BTreeMap::new();
        let mut installed = Vec::with_capacity(record.agent.spec.harnesses.len());
        let mut logins = BTreeMap::new();
        let in_use = self.logins_in_use(record).await?;
        for installation in &record.agent.spec.harnesses {
            if installation.auth == harness::HarnessAuthMode::None {
                installed.push(installation.kind);
                continue;
            }
            // Re-evaluated every pass, so signing in later installs it with no manifest change.
            let Some(login) = self.choose_login(record, installation, &in_use).await? else {
                continue;
            };
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

    /// The host login `record`'s mediated `installation` uses, or `None` for an optional one with
    /// none of its logins stored.
    async fn choose_login(
        &self,
        record: &control_plane::AgentRecord,
        installation: &crate::HarnessSpec,
        in_use: &BTreeMap<crate::Harness, BTreeMap<harness::LoginName, usize>>,
    ) -> Result<Option<harness::LoginName>, Error> {
        let current = record
            .agent
            .status
            .sandbox
            .as_ref()
            .and_then(|assignment| assignment.login(installation.kind));
        let counts = in_use.get(&installation.kind).cloned().unwrap_or_default();
        match harness::choose_login(installation, current, &counts, &self.database).await? {
            None if !installation.optional => Err(harness::no_login_stored(installation.kind, &installation.login)),
            chosen => Ok(chosen),
        }
    }

    /// How many running agents other than `record`'s use each host login, by harness.
    async fn logins_in_use(
        &self,
        record: &control_plane::AgentRecord,
    ) -> Result<BTreeMap<crate::Harness, BTreeMap<harness::LoginName, usize>>, Error> {
        let mut in_use = BTreeMap::<_, BTreeMap<_, usize>>::new();
        for other in self.database.list().await? {
            // A stopped agent spends no quota, and one being deleted soon none.
            if other.id == record.id
                || other.agent.spec.is_stopped()
                || other.agent.metadata.deletion_timestamp.is_some()
            {
                continue;
            }
            let Some(crate::sandbox::Assignment::Materialized { logins, .. }) = &other.agent.status.sandbox else {
                continue;
            };
            for (harness, login) in logins {
                *in_use.entry(*harness).or_default().entry(login.clone()).or_default() += 1;
            }
        }
        Ok(in_use)
    }

    pub(super) fn remove(&self, sandbox: &SandboxName) {
        self.policy.remove_agent(sandbox);
        self.network.remove_secret_bindings(sandbox);
    }
}

fn secret_value<'a>(
    environment: &'a BTreeMap<String, zeroize::Zeroizing<String>>,
    secret: &crate::SecretSpec,
) -> Result<Option<&'a str>, Error> {
    if secret.optional {
        Ok(environment::optional(environment, secret.source()))
    } else {
        Ok(Some(environment::required(environment, secret.source())?))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::secret_value;
    use crate::{SecretSpec, control_plane::AgentStore as _, harness};
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
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        let preparation = super::Preparation::new(database.clone(), policy, network);
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

    fn secret(optional: bool) -> SecretSpec {
        SecretSpec {
            environment: "API_TOKEN".into(),
            optional,
            placeholder: None,
            allowed_hosts: vec!["example.com".into()],
            source: None,
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
}
