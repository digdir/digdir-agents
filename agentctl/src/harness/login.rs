//! Harness logins kept by name.

use sandbox::secret_store::SecretReference;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, persistence};

/// The name a harness login is stored under, such as `work`, so one host can keep a login per
/// subscription and each agent pick one.
///
/// It names stored secrets, so only letters, digits and inner hyphens pass.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct LoginName(String);

impl LoginName {
    const DEFAULT: &str = "default";
    const MAX_LENGTH: usize = 64;

    /// Creates a validated login name.
    ///
    /// # Errors
    ///
    /// Returns an error unless the name is 1-64 ASCII letters, digits and inner hyphens.
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty()
            || value.len() > Self::MAX_LENGTH
            || value.starts_with('-')
            || value.ends_with('-')
            || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::Invalid(
                "a login name must be 1-64 ASCII letters, digits and inner hyphens".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the login used when none is named.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }
}

impl Default for LoginName {
    fn default() -> Self {
        Self(Self::DEFAULT.into())
    }
}

impl TryFrom<String> for LoginName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<LoginName> for String {
    fn from(value: LoginName) -> Self {
        value.0
    }
}

impl std::str::FromStr for LoginName {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl std::fmt::Display for LoginName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How one harness keeps its named logins: a provider account `<provider>/<login>` holding the
/// login's metadata, and each of its secrets as `<secret>/<login>`.
pub(super) struct LoginStore {
    provider: &'static str,
    secrets: &'static [&'static str],
}

impl LoginStore {
    pub(super) const fn new(provider: &'static str, secrets: &'static [&'static str]) -> Self {
        Self { provider, secrets }
    }

    /// The reference to `login`'s `secret`, one of this store's secrets.
    pub(super) fn secret(&self, secret: &str, login: &LoginName) -> SecretReference {
        debug_assert!(self.secrets.contains(&secret));
        SecretReference::from_opaque(format!("{secret}/{login}"))
    }

    /// The login whose `secret` `reference` names, when it names one.
    pub(super) fn login_of(&self, secret: &str, reference: &SecretReference) -> Option<LoginName> {
        debug_assert!(self.secrets.contains(&secret));
        let name = reference.as_str().strip_prefix(secret)?.strip_prefix('/')?;
        LoginName::new(name).ok()
    }

    /// Stores `login`'s secrets, as `(secret, value)` pairs, and metadata together, replacing them.
    pub(super) async fn put(
        &self,
        database: &persistence::Database,
        login: &LoginName,
        secrets: Vec<(&str, Zeroizing<Vec<u8>>)>,
        metadata_json: String,
    ) -> Result<(), Error> {
        database
            .put_provider_account(persistence::ProviderAccountWrite {
                provider: self.account(login),
                credentials: secrets
                    .into_iter()
                    .map(|(secret, value)| persistence::StoredSecret {
                        name: self.secret(secret, login).as_str().to_owned(),
                        value,
                    })
                    .collect(),
                metadata_json,
            })
            .await
    }

    /// The metadata stored with `login`, or `None` when there is no such login.
    pub(super) async fn metadata(
        &self,
        database: &persistence::Database,
        login: &LoginName,
    ) -> Result<Option<String>, Error> {
        database.provider_account_metadata(&self.account(login)).await
    }

    /// The names of the stored logins, in order.
    pub(super) async fn logins(&self, database: &persistence::Database) -> Result<Vec<LoginName>, Error> {
        let prefix = format!("{}/", self.provider);
        Ok(database
            .provider_accounts(self.provider)
            .await?
            .into_iter()
            .filter_map(|(account, _)| LoginName::new(account.strip_prefix(&prefix)?).ok())
            .collect())
    }

    /// Forgets `login` and its secrets, returning whether it was stored.
    pub(super) async fn delete(&self, database: &persistence::Database, login: &LoginName) -> Result<bool, Error> {
        let account = self.account(login);
        if !database.provider_account_exists(&account).await? {
            return Ok(false);
        }
        let secrets = self
            .secrets
            .iter()
            .map(|secret| self.secret(secret, login).as_str().to_owned())
            .collect();
        database.delete_provider_account(&account, secrets).await?;
        Ok(true)
    }

    fn account(&self, login: &LoginName) -> String {
        format!("{}/{login}", self.provider)
    }
}
