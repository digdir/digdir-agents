//! Secrets stored under a name, such as an API key that several agents use, which a manifest asks
//! for with `secrets[].secret`.
//!
//! A stored secret says which hosts its value may ever reach, and the manifest says which hosts the
//! agent sends it to. The network mediator substitutes it only for a host both allow, so neither a
//! manifest nor the person storing the secret can widen where it goes on their own. The mediator
//! reads the value on every use, so replacing it reaches every agent at once, without a restart.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{Agent, Error, authorization::host_pattern_covers, manifest::valid_host_pattern, names, persistence};

/// A stored secret, without its value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Secret {
    /// The name manifests ask for it by.
    pub name: String,
    /// The hosts its value may reach, as host patterns such as `api.example.com` or
    /// `*.example.com`.
    pub allowed_hosts: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl TryFrom<persistence::NamedSecret> for Secret {
    type Error = Error;

    fn try_from(stored: persistence::NamedSecret) -> Result<Self, Error> {
        let time = |seconds| {
            OffsetDateTime::from_unix_timestamp(seconds)
                .map_err(|error| Error::Database(format!("the secret {} has an invalid time: {error}", stored.name)))
        };
        Ok(Self {
            allowed_hosts: serde_json::from_str(&stored.allowed_hosts_json)?,
            created_at: time(stored.created_at)?,
            updated_at: time(stored.updated_at)?,
            name: stored.name,
        })
    }
}

/// Stores, lists and removes secrets.
pub struct Secrets {
    database: persistence::Database,
}

impl Secrets {
    /// Secrets stored in `database`.
    #[must_use]
    pub const fn new(database: persistence::Database) -> Self {
        Self { database }
    }

    /// Stores `value` as the secret `name`, for `allowed_hosts`. Without hosts, it replaces the
    /// value of a secret already stored and keeps its hosts.
    ///
    /// # Errors
    ///
    /// Returns an error when the name, value or a host is invalid, when a new secret has no hosts,
    /// or when it cannot be stored. No error contains the value.
    pub async fn set(
        &self,
        name: &str,
        value: Zeroizing<String>,
        allowed_hosts: Option<Vec<String>>,
    ) -> Result<Secret, Error> {
        let name = valid_name(name)?;
        if value.is_empty() {
            return Err(Error::Invalid("the secret's value is empty".into()));
        }
        let allowed_hosts_json = allowed_hosts
            .map(valid_hosts)
            .transpose()?
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let at = OffsetDateTime::now_utc().unix_timestamp();
        // The network policy reads the hosts at every use, and the value is read after it. New
        // hosts go first, so a host they leave out never gets the value set with them.
        if let Some(allowed_hosts_json) = &allowed_hosts_json {
            self.database
                .set_named_secret_hosts(&name, allowed_hosts_json.clone(), at)
                .await?;
        }
        let write = persistence::NamedSecretWrite {
            value: Zeroizing::new(value.as_bytes().to_vec()),
            allowed_hosts_json,
            at,
            name: name.clone(),
        };
        self.database
            .put_named_secret(write)
            .await?
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "no secret named {name} is stored yet, so it needs the hosts its value may reach"
                ))
            })?
            .try_into()
    }

    /// Every stored secret, by name, without its value.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or holds an unreadable secret.
    pub async fn list(&self) -> Result<Vec<Secret>, Error> {
        self.database
            .named_secrets()
            .await?
            .into_iter()
            .map(Secret::try_from)
            .collect()
    }

    /// Removes the secret `name`, returning it, or `None` when there was none.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is invalid or the database cannot be read or written.
    pub async fn remove(&self, name: &str) -> Result<Option<Secret>, Error> {
        let name = valid_name(name)?;
        self.database
            .delete_named_secret(&name)
            .await?
            .map(Secret::try_from)
            .transpose()
    }
}

/// The secret `name`, as stored in `database`, or `None` when none is.
pub(crate) async fn stored(database: &persistence::Database, name: &str) -> Result<Option<Secret>, Error> {
    let name = valid_name(name)?;
    database.named_secret(&name).await?.map(Secret::try_from).transpose()
}

/// Where the network mediator finds the value of the secret `name`.
pub(crate) fn reference(name: &str) -> sandbox::secret_store::SecretReference {
    sandbox::secret_store::SecretReference::from_opaque(persistence::named_secret_name(&name.to_ascii_lowercase()))
}

fn valid_name(name: &str) -> Result<String, Error> {
    let name = name.to_ascii_lowercase();
    if names::valid(&name) {
        Ok(name)
    } else {
        Err(Error::Invalid(names::RULE.into()))
    }
}

fn valid_hosts(hosts: Vec<String>) -> Result<Vec<String>, Error> {
    if hosts.is_empty() {
        return Err(Error::Invalid(
            "a secret needs at least one host its value may reach".into(),
        ));
    }
    let mut valid = Vec::with_capacity(hosts.len());
    for host in hosts {
        let host = host.trim().to_ascii_lowercase();
        if !valid_host_pattern(&host) {
            return Err(Error::Invalid(format!(
                "{host:?} is not a host pattern such as api.example.com or *.example.com"
            )));
        }
        if !valid.contains(&host) {
            valid.push(host);
        }
    }
    Ok(valid)
}

/// The stored secrets with the agents that use them, and the secrets agents ask for that are not
/// stored.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsReport {
    pub secrets: Vec<SecretReport>,
    pub missing: Vec<MissingSecret>,
}

/// A stored secret and the agents that use it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretReport {
    #[serde(flatten)]
    pub secret: Secret,
    pub used_by: Vec<SecretUse>,
}

/// A secret that agents ask for and that is not stored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingSecret {
    pub name: String,
    pub used_by: Vec<SecretUse>,
}

/// An agent asking for a secret.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretUse {
    pub agent: String,
    /// The environment variable that holds its placeholder.
    pub environment: String,
    /// Whether the agent runs without it while it is not stored, rather than failing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// The hosts the manifest sends it to that the stored secret does not allow, where it stays a
    /// placeholder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts_not_allowed: Vec<String>,
}

/// Reports `secrets` with the `agents` that use them, and the secrets those agents ask for that
/// are not among them. Agents being deleted are left out.
#[must_use]
pub fn report(secrets: Vec<Secret>, agents: &[Agent]) -> SecretsReport {
    let mut report = SecretsReport {
        secrets: secrets
            .into_iter()
            .map(|secret| SecretReport {
                secret,
                used_by: Vec::new(),
            })
            .collect(),
        missing: Vec::new(),
    };
    for agent in agents
        .iter()
        .filter(|agent| agent.metadata.deletion_timestamp.is_none())
    {
        for spec in &agent.spec.secrets {
            let Some(name) = &spec.secret else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            let use_of = |allowed: &[String]| SecretUse {
                agent: agent.metadata.name.clone(),
                environment: spec.environment.clone(),
                optional: spec.optional,
                hosts_not_allowed: spec
                    .allowed_hosts
                    .iter()
                    .filter(|host| !allowed.iter().any(|pattern| host_pattern_covers(pattern, host)))
                    .cloned()
                    .collect(),
            };
            if let Some(stored) = report.secrets.iter_mut().find(|stored| stored.secret.name == name) {
                let used = use_of(&stored.secret.allowed_hosts);
                stored.used_by.push(used);
                continue;
            }
            let mut used = use_of(&[]);
            used.hosts_not_allowed.clear();
            match report.missing.iter_mut().find(|missing| missing.name == name) {
                Some(missing) => missing.used_by.push(used),
                None => report.missing.push(MissingSecret {
                    name,
                    used_by: vec![used],
                }),
            }
        }
    }
    report.missing.sort_by(|left, right| left.name.cmp(&right.name));
    report
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use zeroize::Zeroizing;

    use super::{MissingSecret, SecretUse, Secrets, report};

    fn agent(name: &str, secrets: &str) -> crate::Agent {
        serde_yaml_ng::from_str(&format!(
            "apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: {name}
spec:
  sandbox:
    image: {{type: reference, reference: example.com/agent:latest}}
    platform: {{os: linux}}
    resources: {{cpu: '1', memory: 1Gi, rootFilesystem: {{capacity: 8Gi, mode: layered}}}}
  harnesses: []
  secrets: {secrets}
"
        ))
        .expect("agent")
    }

    #[tokio::test(flavor = "local")]
    async fn a_secret_is_stored_with_its_hosts_which_a_new_value_keeps() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = crate::persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let secrets = Secrets::new(database);
        let value = || Zeroizing::new("sk-FAKE".to_owned());

        let error = secrets
            .set("aivar", value(), None)
            .await
            .expect_err("a new secret needs hosts");
        assert!(error.to_string().contains("needs the hosts"), "{error}");
        for (name, hosts, refused) in [
            ("aivar", vec![], "at least one host"),
            ("aivar", vec!["https://api.aivar.no".to_owned()], "not a host pattern"),
            ("a/b", vec!["api.aivar.no".to_owned()], crate::names::RULE),
        ] {
            let error = secrets.set(name, value(), Some(hosts)).await.expect_err("refused");
            assert!(error.to_string().contains(refused), "{error}");
        }
        let error = secrets
            .set(
                "aivar",
                Zeroizing::new(String::new()),
                Some(vec!["api.aivar.no".into()]),
            )
            .await
            .expect_err("an empty value is refused");
        assert!(error.to_string().contains("empty"), "{error}");
        assert!(secrets.list().await.expect("list").is_empty(), "nothing was stored");

        let first = secrets
            .set(
                "AIVAR",
                value(),
                Some(vec!["API.aivar.no".into(), "api.aivar.no".into()]),
            )
            .await
            .expect("stored");
        assert_eq!(
            (first.name.as_str(), first.allowed_hosts.as_slice()),
            ("aivar", &["api.aivar.no".to_owned()][..])
        );
        let second = secrets.set("aivar", value(), None).await.expect("replaced");
        assert_eq!(second.allowed_hosts, first.allowed_hosts);
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(secrets.list().await.expect("list"), std::slice::from_ref(&second));

        assert_eq!(secrets.remove("Aivar").await.expect("remove"), Some(second));
        assert_eq!(secrets.remove("aivar").await.expect("remove"), None);
    }

    #[test]
    fn the_report_names_users_missing_secrets_and_hosts_a_secret_does_not_allow() {
        let stored = super::Secret {
            name: "aivar".into(),
            allowed_hosts: vec!["*.aivar.no".into()],
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let mut deleted = agent(
            "deleted",
            "[{environment: KEY, secret: models, allowedHosts: [api.models.example]}]",
        );
        deleted.metadata.deletion_timestamp = Some(time::OffsetDateTime::UNIX_EPOCH);
        let agents = [
            agent(
                "pi",
                "[{environment: AIVAR_API_KEY, secret: Aivar, allowedHosts: [api.aivar.no, models.example]}, \
                 {environment: MODELS_API_KEY, secret: models, optional: true, allowedHosts: [api.models.example]}, \
                 {environment: OTHER, allowedHosts: [other.example]}]",
            ),
            deleted,
        ];

        let report = report(vec![stored.clone()], &agents);

        assert_eq!(report.secrets.len(), 1);
        assert_eq!(report.secrets[0].secret, stored);
        assert_eq!(
            report.secrets[0].used_by,
            [SecretUse {
                agent: "pi".into(),
                environment: "AIVAR_API_KEY".into(),
                optional: false,
                hosts_not_allowed: vec!["models.example".into()],
            }]
        );
        assert_eq!(
            report.missing,
            [MissingSecret {
                name: "models".into(),
                used_by: vec![SecretUse {
                    agent: "pi".into(),
                    environment: "MODELS_API_KEY".into(),
                    optional: true,
                    hosts_not_allowed: Vec::new(),
                }],
            }]
        );
    }
}
