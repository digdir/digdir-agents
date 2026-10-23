//! Host mediation setup for the Microsandbox Network Backend.

use std::{collections::BTreeMap, rc::Rc};

use crate::{Error, authorization::AgentPolicyEngine, control_plane, environment, harness, persistence};
use ::sandbox::{SandboxHandle, SandboxId, SandboxName, network::NetworkBackend as _};
use sandbox_microsandbox::{MicrosandboxNetworkBackend, SecretBinding};

/// The plaintext Git identity Linux setup configures, here taken from the GitHub account.
const GIT_USER_NAME: &str = "GIT_USER_NAME";
const GIT_USER_EMAIL: &str = "GIT_USER_EMAIL";

/// Connects Agent policy and host-owned secrets to the Microsandbox Network Backend.
pub(super) struct Preparation {
    database: persistence::Database,
    policy: Rc<AgentPolicyEngine>,
    network: Rc<MicrosandboxNetworkBackend>,
}

pub(super) struct PreparedNetwork {
    pub(super) bindings_changed: bool,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) harnesses: Vec<crate::Harness>,
}

impl Preparation {
    /// Creates the Agent-side Microsandbox mediation adapter.
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
    pub(super) fn network_is_running(&self, sandbox: &SandboxId) -> bool {
        self.network.is_running(sandbox)
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
            let mut managed_secrets = Vec::new();
            let mut managed_environments = BTreeMap::new();
            let mut managed_placeholders = BTreeMap::new();
            let mut installed = Vec::with_capacity(record.agent.spec.harnesses.len());
            for installation in &record.agent.spec.harnesses {
                // Re-evaluated every pass, so signing in later installs it with no manifest change.
                if installation.optional && !harness::authentication_ready(installation.kind, &self.database).await? {
                    continue;
                }
                installed.push(installation.kind);
                for secret in harness::prepare(installation.kind, &self.database).await? {
                    if let Some(existing) = managed_environments.insert(secret.environment, installation.kind.as_str())
                    {
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
            }
            let identity = github_account(&self.database, &record.agent.spec, &mut managed_secrets).await?;
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
            let mut guest_environment: BTreeMap<String, String> = bindings
                .iter()
                .map(|binding| {
                    let (name, value) = binding.guest_environment();
                    (name.to_owned(), value.to_owned())
                })
                .collect();
            guest_environment.extend(identity);
            let bindings_changed = self.network.set_secret_bindings(sandbox_name.clone(), bindings)?;
            Ok(PreparedNetwork {
                bindings_changed,
                environment: guest_environment,
                harnesses: installed,
            })
        }
        .await;
        if result.is_err() {
            self.remove(&sandbox_name);
        }
        result
    }

    pub(super) fn remove(&self, sandbox: &SandboxName) {
        self.policy.remove_agent(sandbox);
        self.network.remove_secret_bindings(sandbox);
    }
}

/// Adds the GitHub account's mediated token for an Agent that has one, returning the account's
/// Git identity for the guest unless the manifest sets its own.
async fn github_account(
    database: &persistence::Database,
    spec: &crate::Spec,
    managed_secrets: &mut Vec<harness::MediatedSecret>,
) -> Result<Vec<(String, String)>, Error> {
    let Some(owner) = spec.github_owner() else {
        return Ok(Vec::new());
    };
    let (secret, account) = crate::github::prepare(database, owner, time::OffsetDateTime::now_utc()).await?;
    managed_secrets.push(secret);
    // Validation requires GIT_USER_NAME and GIT_USER_EMAIL together, so one stands for both.
    if spec.environment.iter().any(|variable| variable.name == GIT_USER_NAME) {
        return Ok(Vec::new());
    }
    Ok(vec![
        (GIT_USER_NAME.into(), account.name),
        (GIT_USER_EMAIL.into(), account.email),
    ])
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
    use crate::SecretSpec;
    use std::{collections::BTreeMap, rc::Rc};
    use zeroize::Zeroizing;

    const MANIFEST: &str = "apiVersion: agents.platform/v1alpha1
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
      owner: Digdir
";
    const ACCOUNTS: &str = "  accounts:\n    - type: github\n      owner: Digdir\n";

    /// A Preparation over a database holding GitHub tokens for `digdir` and `altinn`.
    async fn with_tokens(
        directory: &tempfile::TempDir,
    ) -> (super::Preparation, Rc<crate::authorization::AgentPolicyEngine>) {
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        for owner in ["digdir", "altinn"] {
            database
                .put_provider_account(crate::persistence::ProviderAccountWrite {
                    provider: format!("github/{owner}"),
                    credentials: vec![crate::persistence::StoredSecret {
                        name: format!("github-access-token/{owner}"),
                        value: Zeroizing::new(format!("github_pat_FAKE_{owner}").into_bytes()),
                    }],
                    metadata_json: format!(
                        r#"{{"owner":"{owner}","login":"{owner}-bot","name":"{owner} bot","email":"1+{owner}-bot@users.noreply.github.com","kind":"fineGrained"}}"#
                    ),
                })
                .await
                .expect("store token");
        }
        let policy = Rc::new(crate::authorization::AgentPolicyEngine::new());
        let network = Rc::new(sandbox_microsandbox::MicrosandboxNetworkBackend::new(policy.clone()));
        (super::Preparation::new(database, policy.clone(), network), policy)
    }

    /// Whether the policy lets the Agent's Sandbox use `secret` in a request to `host`.
    async fn may_use(
        policy: &crate::authorization::AgentPolicyEngine,
        record: &crate::control_plane::AgentRecord,
        secret: &str,
        host: &str,
    ) -> bool {
        use sandbox_authorization::{
            Action, AuthorizationContext, AuthorizationDecision, AuthorizationRequest, PolicyEngine as _, Principal,
            Resource,
            vocabulary::{action, context, principal_kind, resource_kind},
        };
        let sandbox = record.sandbox_name().expect("Sandbox name");
        let decision = policy
            .evaluate(AuthorizationRequest {
                principal: Principal::new(principal_kind::SANDBOX, "sandbox-id"),
                action: Action::new(action::SECRET_USE),
                resource: Resource::new(resource_kind::SECRET, secret),
                context: AuthorizationContext::new()
                    .with_attribute(context::SANDBOX_NAME, sandbox.as_str())
                    .with_attribute(context::HTTP_AUTHORITY, host),
            })
            .await
            .expect("policy decision");
        decision == AuthorizationDecision::Allow
    }

    fn record(directory: &tempfile::TempDir, manifest: &str) -> crate::control_plane::AgentRecord {
        crate::control_plane::AgentRecord {
            id: "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c".parse().expect("Agent ID"),
            source_directory: directory.path().to_path_buf(),
            manifest_path: None,
            env_file: None,
            // Decoded without validation, which needs a harness this module does not name.
            agent: serde_yaml_ng::from_str(manifest).expect("manifest"),
        }
    }

    #[tokio::test(flavor = "local")]
    async fn a_github_account_mediates_its_token_for_githubs_hosts_only() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy) = with_tokens(&directory).await;
        let record = record(&directory, MANIFEST);

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert_eq!(
            prepared.environment,
            BTreeMap::from([
                ("GITHUB_TOKEN".into(), "github_pat_AGENT_MEDIATED_GITHUB_TOKEN".into()),
                ("GIT_USER_NAME".into(), "digdir bot".into()),
                ("GIT_USER_EMAIL".into(), "1+digdir-bot@users.noreply.github.com".into()),
            ])
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
    async fn a_manifest_git_identity_wins_over_the_github_account() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, _policy) = with_tokens(&directory).await;
        let manifest = format!("{MANIFEST}  environment:\n    - name: GIT_USER_NAME\n    - name: GIT_USER_EMAIL\n");

        let prepared = preparation
            .prepare(&record(&directory, &manifest))
            .await
            .expect("prepare");

        assert_eq!(prepared.environment.keys().collect::<Vec<_>>(), ["GITHUB_TOKEN"]);
    }

    #[tokio::test(flavor = "local")]
    async fn an_agent_without_accounts_gets_no_github_token_even_when_tokens_are_stored() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy) = with_tokens(&directory).await;
        let record = record(&directory, &MANIFEST.replace(ACCOUNTS, ""));

        let prepared = preparation.prepare(&record).await.expect("prepare");

        assert!(prepared.environment.is_empty(), "{:?}", prepared.environment);
        assert!(!may_use(&policy, &record, "GITHUB_TOKEN", "github.com").await);
    }

    #[tokio::test(flavor = "local")]
    async fn an_owner_without_a_token_fails_naming_the_login() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let (preparation, policy) = with_tokens(&directory).await;
        let record = record(&directory, &MANIFEST.replace("owner: Digdir", "owner: elsewhere"));

        let Err(crate::Error::Invalid(message)) = preparation.prepare(&record).await else {
            panic!("a permanent failure");
        };

        assert!(
            message.contains("`agentctl github login --owner elsewhere`"),
            "{message}"
        );
        assert!(!message.contains("FAKE"), "{message}");
        assert!(!may_use(&policy, &record, "GITHUB_TOKEN", "github.com").await);
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
