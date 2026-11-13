//! Accounts agents act as, such as a GitHub account, each signed in once and kept under a name a
//! manifest asks for.
//!
//! Each type has a module of its own, such as [`github`], that knows how its credential is
//! checked, what is kept about it, where the network mediator may substitute it and what setup
//! configures in the guest. A manifest only names the account, and an agent has at most one of
//! each type. Credentials stay in the secret store under names that never change, and the mediator
//! reads them on every use, so a type whose credentials expire can refresh them by replacing the
//! stored value, without touching the agents that use it.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, Spec, github, harness::MediatedSecret, manifest::AccountSpec, persistence, sandbox::AccountSetup};

/// The types of account an agent can act as.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountType {
    /// A GitHub account, reached with a token.
    Github,
}

impl AccountType {
    /// The type as manifests write it, such as `github`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
        }
    }
}

impl std::fmt::Display for AccountType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A kept account, without its credential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Account {
    /// A GitHub account.
    Github(github::Account),
}

impl Account {
    /// The account's type.
    #[must_use]
    pub const fn account_type(&self) -> AccountType {
        match self {
            Self::Github(_) => AccountType::Github,
        }
    }

    /// The name the account is kept under.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Github(account) => &account.name,
        }
    }
}

/// What checking a credential found, as kept.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Login {
    /// A GitHub token.
    Github(github::Login),
}

impl Login {
    /// The account as kept.
    #[must_use]
    pub fn account(&self) -> Account {
        match self {
            Self::Github(login) => Account::Github(login.account.clone()),
        }
    }
}

/// Checks credentials and keeps them as logins, lists and forgets them, for every type.
pub struct Accounts {
    github: github::Authentication,
}

impl Accounts {
    /// Accounts kept in `database`.
    #[must_use]
    pub fn new(database: persistence::Database) -> Self {
        Self {
            github: github::Authentication::new(database),
        }
    }

    /// Checks `credential` and keeps it as the login `login` for an account of `account_type`,
    /// replacing an earlier one of that name.
    ///
    /// # Errors
    ///
    /// Returns an error when the login or credential is invalid, the credential is refused, or it
    /// cannot be kept. Nothing is kept then, and no error contains the credential.
    pub async fn login(
        &self,
        account_type: AccountType,
        login: &str,
        credential: Zeroizing<String>,
    ) -> Result<Login, Error> {
        match account_type {
            AccountType::Github => self.github.login(credential, login).await.map(Login::Github),
        }
    }

    /// Every kept login, by type, then name, without its credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or holds an unreadable login.
    pub async fn list(&self) -> Result<Vec<Account>, Error> {
        Ok(self.github.accounts().await?.into_iter().map(Account::Github).collect())
    }

    /// Forgets the login `login` for an account of `account_type`, returning it, or `None` when
    /// there was none.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or written.
    pub async fn logout(&self, account_type: AccountType, login: &str) -> Result<Option<Account>, Error> {
        match account_type {
            AccountType::Github => Ok(self.github.forget(login).await?.map(Account::Github)),
        }
    }
}

/// What an agent's accounts give its sandbox.
pub(crate) struct PreparedAccounts {
    /// The mediated credentials to bind, one per account, whether or not its login is kept, so a
    /// login or logout later changes no binding.
    pub(crate) secrets: Vec<MediatedSecret>,
    /// What setup configures in the guest.
    pub(crate) setup: AccountSetup,
}

/// Prepares the accounts `spec` asks for, as `database` keeps them.
///
/// # Errors
///
/// Returns an error when a login's name is invalid or the database cannot be read.
pub(crate) async fn prepare(database: &persistence::Database, spec: &Spec) -> Result<PreparedAccounts, Error> {
    let mut prepared = PreparedAccounts {
        secrets: Vec::new(),
        setup: AccountSetup::default(),
    };
    for account in &spec.accounts {
        match account {
            AccountSpec::Github { login } => {
                prepared.secrets.push(github::mediated_token(login)?);
                let kept = github::stored(database, login).await?;
                let setup = github::setup(login, kept.as_ref());
                prepared.setup.git_identity = prepared.setup.git_identity.take().or(setup.git_identity);
                prepared.setup.instructions.extend(setup.instructions);
                prepared.setup.missing_logins.extend(setup.missing_logins);
            }
        }
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn accounts_name_their_type_on_the_wire() {
        let account = Account::Github(github::Account {
            name: "altinn-studio".into(),
            identity: github::Identity {
                login: "digdir-bot".into(),
                name: "Digdir Bot".into(),
                email: "1+digdir-bot@users.noreply.github.com".into(),
            },
            kind: github::TokenKind::FineGrained,
            expires_at: None,
        });
        let wire = serde_json::to_value(&account).expect("serialized");
        assert_eq!(wire["type"], "github");
        assert_eq!(wire["name"], "altinn-studio");
        assert_eq!(serde_json::from_value::<Account>(wire).expect("parsed"), account);
        assert_eq!(account.account_type().to_string(), "github");
    }
}
