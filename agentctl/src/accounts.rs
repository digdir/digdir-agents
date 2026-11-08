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

use crate::github;

/// The longest name an account is kept under.
pub(crate) const NAME_MAX_LENGTH: usize = 64;
/// What an account's name must be, said without repeating the value, which may be a misplaced
/// credential.
pub const NAME_RULE: &str = "the name must be letters, digits and inner hyphens, at most 64 characters";

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

/// Whether `name` can name an account: ASCII letters, digits and inner hyphens, at most 64
/// characters. Names become secret names, so nothing else passes. Names are compared in lowercase.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX_LENGTH
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn names_are_letters_digits_and_inner_hyphens() {
        for name in ["digdir", "Digdir", "a", "a-b", "x2", "altinn-studio", &"a".repeat(64)] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "../x",
            "..",
            "a/b",
            "-a",
            "a-",
            "a_b",
            "a.b",
            "a b",
            "a%2fb",
            "æ",
            &"a".repeat(65),
        ] {
            assert!(!valid_name(name), "{name}");
        }
    }

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
