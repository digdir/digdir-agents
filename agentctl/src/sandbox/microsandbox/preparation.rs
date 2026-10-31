//! Host mediation setup for the Microsandbox Network Backend.

use std::{collections::BTreeMap, rc::Rc};

use crate::{Error, authorization::AgentPolicyEngine, control_plane, environment, harness, persistence};
use ::sandbox::{SandboxHandle, SandboxId, SandboxName, network::NetworkBackend as _};
use sandbox_microsandbox::{MicrosandboxNetworkBackend, SecretBinding};

/// Connects agent policy and host-owned secrets to the Microsandbox Network Backend.
pub(super) struct Preparation {
    database: persistence::Database,
    credentials: Rc<harness::AuthenticationManager>,
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
        credentials: Rc<harness::AuthenticationManager>,
        policy: Rc<AgentPolicyEngine>,
        network: Rc<MicrosandboxNetworkBackend>,
    ) -> Self {
        Self {
            database,
            credentials,
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

    /// Forgets the logins a failed pass chose for `record`; see
    /// [`harness::AuthenticationManager::release_logins`].
    pub(super) fn release_logins(&self, record: &control_plane::AgentRecord) {
        self.credentials.release_logins(record.id);
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
        // A later pass for the same agent only updates its status.
        if database.get(record.id).await.is_err() {
            database.put(record.clone(), 0).await.expect("store agent");
        }
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
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let preparation = super::Preparation::new(database.clone(), credentials, policy, network);
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

    /// A harness that reports how much of its login is used steers the next agent to another.
    #[tokio::test(flavor = "local")]
    async fn usage_a_session_reports_counts_against_its_agents_login() {
        use crate::sessions::SessionStore as _;

        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let preparation = super::Preparation::new(database.clone(), credentials.clone(), policy, network);
        let login = |prepared: &super::PreparedNetwork| prepared.logins[&harness::test_harness()].to_string();
        let first = record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c");
        let second = record("second", "7b4d2a1f-3c8e-4f6a-8d95-1a2b3c4d5e6f");
        for agent in [&first, &second] {
            let prepared = preparation.prepare(agent).await.expect("prepare");
            store_materialized(&database, agent.clone(), &prepared).await;
        }
        let session = database
            .ensure_session(
                "first",
                &crate::sessions::SessionName::new("s1").expect("session name"),
                crate::sessions::NewSession::for_harness(harness::test_harness()),
            )
            .await
            .expect("session");
        let token: crate::sessions::LaunchToken = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().expect("token");
        database
            .record_session_launch(
                session.id,
                crate::sessions::LaunchRecord {
                    token: token.clone(),
                    sandbox: "sandbox".into(),
                    launched_at: 0,
                    attempts: 1,
                },
            )
            .await
            .expect("record launch");
        let usage = harness::Usage {
            windows: vec![harness::UsageWindow {
                used_percent: 90,
                resets_at: time::OffsetDateTime::now_utc() + time::Duration::DAY,
            }],
        };
        let stale: crate::sessions::LaunchToken = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().expect("token");
        assert!(matches!(
            credentials
                .record_session_usage(session.id, &stale, usage.clone())
                .await,
            Err(crate::Error::NotFound)
        ));

        credentials
            .record_session_usage(session.id, &token, usage)
            .await
            .expect("record usage");

        let third = record("third", "8c5e3b2a-4d9f-4a7b-9ea6-2b3c4d5e6f70");
        let prepared = preparation.prepare(&third).await.expect("prepare third");
        assert_eq!(
            login(&prepared),
            "b",
            "a and b have one agent each, and the first agent's session reported a nearly used up"
        );
    }

    /// A session reports for the login its agent's sandbox runs with, which its stored status
    /// says, rather than one a pass has chosen but not yet started it with.
    #[tokio::test(flavor = "local")]
    async fn a_reading_counts_for_the_login_the_sandbox_runs_with_until_its_credential_changes() {
        use crate::sessions::SessionStore as _;

        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let preparation = super::Preparation::new(database.clone(), credentials.clone(), policy, network);
        let agent = record("first", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c");
        let prepared = preparation.prepare(&agent).await.expect("prepare");
        assert_eq!(prepared.logins[&harness::test_harness()].as_str(), "a");
        store_materialized(&database, agent.clone(), &prepared).await;
        let session = database
            .ensure_session(
                "first",
                &crate::sessions::SessionName::new("s1").expect("session name"),
                crate::sessions::NewSession::for_harness(harness::test_harness()),
            )
            .await
            .expect("session");
        let token: crate::sessions::LaunchToken = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().expect("token");
        database
            .record_session_launch(
                session.id,
                crate::sessions::LaunchRecord {
                    token: token.clone(),
                    sandbox: "sandbox".into(),
                    launched_at: 0,
                    attempts: 1,
                },
            )
            .await
            .expect("record launch");
        let resets_at = time::OffsetDateTime::now_utc() + time::Duration::DAY;
        let reading = |used_percent| harness::Usage {
            windows: vec![harness::UsageWindow {
                used_percent,
                resets_at,
            }],
        };
        let report = |usage| credentials.record_session_usage(session.id, &token, usage);
        let used = || async {
            credentials
                .logins()
                .await
                .expect("logins")
                .into_iter()
                .map(|login| {
                    (
                        login.login.to_string(),
                        login.usage.map(|usage| usage.windows[0].used_percent),
                    )
                })
                .collect::<Vec<_>>()
        };
        let names = |logins: &[(&str, Option<u8>)]| {
            logins
                .iter()
                .map(|(login, used)| ((*login).to_owned(), *used))
                .collect::<Vec<_>>()
        };

        report(reading(100)).await.expect("report");
        let moved = preparation.prepare(&agent).await.expect("prepare again");
        assert_eq!(moved.logins[&harness::test_harness()].as_str(), "b", "a is used up");
        report(reading(60)).await.expect("report");
        assert_eq!(
            used().await,
            names(&[("a", Some(100)), ("b", None)]),
            "the sandbox still runs with a until the pass that chose b has started it, and a late \
             reading of a window does not lower it"
        );

        store_materialized(&database, agent.clone(), &moved).await;
        report(reading(60)).await.expect("report");
        assert_eq!(
            used().await,
            names(&[("a", Some(100)), ("b", None)]),
            "the reading the session repeats after moving still describes a"
        );
        report(reading(30)).await.expect("report");
        assert_eq!(used().await, names(&[("a", Some(100)), ("b", Some(30))]));

        assert!(
            credentials
                .logout(
                    harness::test_harness(),
                    &harness::LoginName::new("b").expect("login name")
                )
                .await
                .expect("logout")
        );
        harness::store_test_login("b", &database).await;
        report(reading(30)).await.expect("report");
        assert_eq!(
            used().await,
            names(&[("a", Some(100)), ("b", None)]),
            "b may be another account now, which the repeated reading does not describe"
        );
    }

    /// A pass that fails after choosing a login has not started its sandbox with it, so the choice
    /// stops counting for other agents.
    #[tokio::test(flavor = "local")]
    async fn a_failed_pass_gives_back_the_logins_it_chose() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for name in ["a", "b"] {
            harness::store_test_login(name, &database).await;
        }
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let preparation = super::Preparation::new(database.clone(), credentials, policy, network);
        let login = |prepared: &super::PreparedNetwork| prepared.logins[&harness::test_harness()].to_string();
        let failed = record("failed", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c");
        database.put(failed.clone(), 0).await.expect("store agent");
        let prepared = preparation.prepare(&failed).await.expect("prepare");
        assert_eq!(login(&prepared), "a");

        preparation.release_logins(&failed);

        let other = record("other", "7b4d2a1f-3c8e-4f6a-8d95-1a2b3c4d5e6f");
        let prepared = preparation.prepare(&other).await.expect("prepare other");
        assert_eq!(login(&prepared), "a", "the failed agent's choice no longer counts");
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
        let credentials = Rc::new(harness::AuthenticationManager::new(database.clone()));
        let preparation = super::Preparation::new(database.clone(), credentials, policy, network);
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
