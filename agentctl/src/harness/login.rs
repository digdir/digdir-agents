//! Harness logins kept by name.

use std::collections::BTreeMap;

use sandbox::secret_store::SecretReference;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, persistence};

/// The name a harness login is stored under, such as `work`, so one host can keep a login per
/// subscription and each agent pick one.
///
/// It names stored secrets, so only letters, digits and inner hyphens pass, and never `any`, which
/// a manifest uses for every login ([`LoginSet`]).
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
    /// Returns an error unless the name is 1-64 ASCII letters, digits and inner hyphens, other
    /// than `any`.
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.eq_ignore_ascii_case(LoginSet::ANY) {
            return Err(Error::Invalid(
                "a login cannot be named \"any\", which manifests use for any login".into(),
            ));
        }
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

/// The logins a harness installation may use: named ones, in order of preference, or any login
/// stored for the harness. A manifest writes one name, a list of names, or `any`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginSet(Logins);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Logins {
    /// Never empty, and without repeats.
    Named(Vec<LoginName>),
    Any,
}

impl LoginSet {
    const ANY: &str = "any";

    /// The set of only `login`.
    #[must_use]
    pub fn one(login: LoginName) -> Self {
        Self(Logins::Named(vec![login]))
    }

    /// The set of `logins`, preferred in that order.
    ///
    /// # Errors
    ///
    /// Returns an error when `logins` is empty or names a login twice.
    pub fn among(logins: Vec<LoginName>) -> Result<Self, Error> {
        let unique = logins.iter().collect::<std::collections::BTreeSet<_>>();
        if logins.is_empty() || unique.len() != logins.len() {
            return Err(Error::Invalid(
                "a list of logins must name at least one, each once".into(),
            ));
        }
        Ok(Self(Logins::Named(logins)))
    }

    /// The set of every stored login.
    #[must_use]
    pub const fn any() -> Self {
        Self(Logins::Any)
    }

    /// Whether this is the set used when a manifest names none: only the login `default`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(&self.0, Logins::Named(logins) if logins.len() == 1 && logins[0].is_default())
    }

    /// Whether this set allows `login`.
    #[must_use]
    pub fn allows(&self, login: &LoginName) -> bool {
        match &self.0 {
            Logins::Named(logins) => logins.contains(login),
            Logins::Any => true,
        }
    }

    /// The login this set prefers, unless it is `any`.
    #[must_use]
    pub fn first(&self) -> Option<&LoginName> {
        match &self.0 {
            Logins::Named(logins) => logins.first(),
            Logins::Any => None,
        }
    }

    /// The only login this set allows, when it allows just one.
    #[must_use]
    pub fn single(&self) -> Option<&LoginName> {
        match &self.0 {
            Logins::Named(logins) if logins.len() == 1 => logins.first(),
            Logins::Named(_) | Logins::Any => None,
        }
    }

    /// Picks the login an installation of this set uses among the `stored` ones.
    ///
    /// The `current` login stays while it is in the set, stored and not used up, or while every
    /// other is used up too, because moving a working agent costs it: its harness loses its prompt
    /// cache, or, for some harnesses, its sandbox restarts. Otherwise it is the login with the most
    /// of its limits left by `usage` at `now`, in steps of [`USAGE_STEP`] so that a slightly fuller
    /// login still takes agents, then the one the fewest other running agents use by `in_use`, then
    /// the earlier in the set, or by name for `any`. A login without a reading counts as unused.
    /// `None` when no login of the set is stored.
    #[must_use]
    pub fn choose(
        &self,
        stored: &[LoginName],
        current: Option<&LoginName>,
        in_use: &BTreeMap<LoginName, usize>,
        usage: &BTreeMap<LoginName, Usage>,
        now: time::OffsetDateTime,
    ) -> Option<LoginName> {
        let mut candidates = match &self.0 {
            Logins::Named(logins) => logins.iter().filter(|login| stored.contains(login)).collect::<Vec<_>>(),
            Logins::Any => {
                let mut stored = stored.iter().collect::<Vec<_>>();
                stored.sort();
                stored
            }
        };
        let used = |login: &LoginName| usage.get(login).map_or(0, |usage| usage.used_percent(now));
        let used_up = |login: &LoginName| used(login) >= 100;
        if let Some(current) = current.filter(|current| candidates.contains(current))
            && (!used_up(current) || candidates.iter().all(|login| used_up(login)))
        {
            return Some(current.clone());
        }
        // A stable sort keeps the set's order among equally used logins.
        candidates.sort_by_key(|login| {
            (
                used_up(login),
                used(login) / USAGE_STEP,
                in_use.get(*login).copied().unwrap_or_default(),
            )
        });
        candidates.first().map(|login| (*login).clone())
    }
}

/// The step in which [`LoginSet::choose`] compares how much of logins' limits is used, in percent.
pub const USAGE_STEP: u8 = 10;

/// How much of a login's subscription limits is used, as its provider last reported it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Usage {
    /// Each rolling limit, such as five hours or a week.
    pub windows: Vec<UsageWindow>,
}

/// One rolling limit of a login's subscription.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageWindow {
    /// The share of the limit used, from 0 to 100.
    pub used_percent: u8,
    /// When the window starts over, and its use with it.
    #[serde(with = "time::serde::rfc3339")]
    pub resets_at: time::OffsetDateTime,
}

impl Usage {
    /// The used share of the fullest window not yet reset at `now`, which decides when the login
    /// runs out.
    #[must_use]
    pub fn used_percent(&self, now: time::OffsetDateTime) -> u8 {
        self.windows
            .iter()
            .filter(|window| window.resets_at > now)
            .map(|window| window.used_percent.min(100))
            .max()
            .unwrap_or_default()
    }

    /// When the fullest window not yet reset at `now` resets, if any has.
    #[must_use]
    pub fn resets_at(&self, now: time::OffsetDateTime) -> Option<time::OffsetDateTime> {
        self.windows
            .iter()
            .filter(|window| window.resets_at > now)
            .max_by_key(|window| (window.used_percent, std::cmp::Reverse(window.resets_at)))
            .map(|window| window.resets_at)
    }
}

impl Default for LoginSet {
    fn default() -> Self {
        Self::one(LoginName::default())
    }
}

impl std::fmt::Display for LoginSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Logins::Named(logins) => {
                let names = logins.iter().map(LoginName::as_str).collect::<Vec<_>>();
                formatter.write_str(&names.join(", "))
            }
            Logins::Any => formatter.write_str(Self::ANY),
        }
    }
}

impl Serialize for LoginSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Logins::Named(logins) if logins.len() == 1 => logins[0].serialize(serializer),
            Logins::Named(logins) => logins.serialize(serializer),
            Logins::Any => serializer.serialize_str(Self::ANY),
        }
    }
}

impl<'de> Deserialize<'de> for LoginSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Declared {
            One(String),
            Among(Vec<String>),
        }
        let set = match Declared::deserialize(deserializer)? {
            Declared::One(name) if name == Self::ANY => Ok(Self::any()),
            Declared::One(name) => LoginName::new(name).map(Self::one),
            Declared::Among(names) => names
                .into_iter()
                .map(LoginName::new)
                .collect::<Result<Vec<_>, _>>()
                .and_then(Self::among),
        };
        set.map_err(serde::de::Error::custom)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn names(names: &[&str]) -> Vec<LoginName> {
        names
            .iter()
            .map(|name| LoginName::new(*name).expect("login name"))
            .collect()
    }

    fn set(yaml: &str) -> LoginSet {
        serde_yaml_ng::from_str(yaml).expect("login set")
    }

    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(1_793_188_800).expect("time")
    }

    /// Usage of one window resetting a day after [`now`], or none for `None`.
    fn readings(used: &[(&str, u8)]) -> BTreeMap<LoginName, Usage> {
        used.iter()
            .map(|(name, used_percent)| {
                (
                    LoginName::new(*name).expect("login name"),
                    Usage {
                        windows: vec![UsageWindow {
                            used_percent: *used_percent,
                            resets_at: now() + time::Duration::DAY,
                        }],
                    },
                )
            })
            .collect()
    }

    fn uses(counts: &[(&str, usize)]) -> BTreeMap<LoginName, usize> {
        counts
            .iter()
            .map(|(name, count)| (LoginName::new(*name).expect("login name"), *count))
            .collect()
    }

    #[test]
    fn a_set_is_one_name_a_list_or_any_and_reads_back_as_written() {
        for yaml in ["work", "[work, private]", "any"] {
            let set = set(yaml);
            let written = serde_yaml_ng::to_string(&set).expect("serialize");
            assert_eq!(super::tests::set(&written), set, "{yaml}");
        }
        assert!(LoginSet::default().is_default());
        assert!(!set("[default, work]").is_default());
        for invalid in ["[]", "[work, work]", "[work, any]", "work/other", "[work/other]"] {
            assert!(serde_yaml_ng::from_str::<LoginSet>(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn an_agent_keeps_its_login_while_the_set_allows_it_and_it_is_stored() {
        let stored = names(&["private", "work"]);
        let current = LoginName::new("work").expect("login name");
        let busy = uses(&[("work", 5)]);

        assert_eq!(
            set("any").choose(&stored, Some(&current), &busy, &BTreeMap::new(), now()),
            Some(current.clone())
        );
        assert_eq!(
            set("[private, work]").choose(&stored, Some(&current), &busy, &BTreeMap::new(), now()),
            Some(current.clone()),
            "a working agent is not moved to even out the count"
        );
        assert_eq!(
            set("private").choose(&stored, Some(&current), &busy, &BTreeMap::new(), now()),
            names(&["private"]).pop(),
            "a login the set no longer allows is replaced"
        );
        assert_eq!(
            set("any").choose(&names(&["private"]), Some(&current), &busy, &BTreeMap::new(), now()),
            names(&["private"]).pop(),
            "a forgotten login is replaced"
        );
    }

    #[test]
    fn a_new_agent_gets_the_least_used_login_and_on_a_tie_the_preferred_one() {
        let stored = names(&["a", "b", "c"]);

        assert_eq!(
            set("[c, b, a]").choose(
                &stored,
                None,
                &uses(&[("c", 2), ("b", 1), ("a", 1)]),
                &BTreeMap::new(),
                now()
            ),
            names(&["b"]).pop(),
            "on a tie the set's order decides"
        );
        assert_eq!(
            set("any").choose(&stored, None, &uses(&[("a", 1)]), &BTreeMap::new(), now()),
            names(&["b"]).pop(),
            "for any, names decide a tie"
        );
        assert_eq!(
            set("[d, c]").choose(&stored, None, &BTreeMap::new(), &BTreeMap::new(), now()),
            names(&["c"]).pop()
        );
        assert_eq!(
            set("d").choose(&stored, None, &BTreeMap::new(), &BTreeMap::new(), now()),
            None
        );
        assert_eq!(
            set("any").choose(&[], None, &BTreeMap::new(), &BTreeMap::new(), now()),
            None
        );
    }

    #[test]
    fn a_new_agent_gets_the_login_with_the_most_left_and_the_count_decides_within_a_step() {
        let stored = names(&["a", "b", "c"]);
        let any = set("any");

        assert_eq!(
            any.choose(
                &stored,
                None,
                &BTreeMap::new(),
                &readings(&[("a", 60), ("b", 30), ("c", 45)]),
                now()
            ),
            names(&["b"]).pop()
        );
        assert_eq!(
            any.choose(
                &stored,
                None,
                &uses(&[("b", 2)]),
                &readings(&[("a", 34), ("b", 31), ("c", 90)]),
                now()
            ),
            names(&["a"]).pop(),
            "a and b are in one step, and b has more agents"
        );
        assert_eq!(
            any.choose(
                &stored,
                None,
                &BTreeMap::new(),
                &readings(&[("a", 20), ("b", 20)]),
                now()
            ),
            names(&["c"]).pop(),
            "a login without a reading counts as unused"
        );
    }

    #[test]
    fn an_agent_moves_only_once_its_login_is_used_up_and_another_is_not() {
        let stored = names(&["a", "b"]);
        let current = LoginName::new("a").expect("login name");
        let any = set("any");

        assert_eq!(
            any.choose(
                &stored,
                Some(&current),
                &BTreeMap::new(),
                &readings(&[("a", 99), ("b", 0)]),
                now()
            ),
            Some(current.clone()),
            "nearly used up is not used up"
        );
        assert_eq!(
            any.choose(
                &stored,
                Some(&current),
                &BTreeMap::new(),
                &readings(&[("a", 100), ("b", 80)]),
                now()
            ),
            names(&["b"]).pop()
        );
        assert_eq!(
            any.choose(
                &stored,
                Some(&current),
                &BTreeMap::new(),
                &readings(&[("a", 100), ("b", 100)]),
                now()
            ),
            Some(current),
            "with every login used up, moving gains nothing"
        );
    }

    #[test]
    fn a_window_counts_until_it_resets() {
        let usage = Usage {
            windows: vec![
                UsageWindow {
                    used_percent: 100,
                    resets_at: now() - time::Duration::MINUTE,
                },
                UsageWindow {
                    used_percent: 40,
                    resets_at: now() + time::Duration::DAY,
                },
            ],
        };

        assert_eq!(usage.used_percent(now()), 40, "the full window has reset");
        assert_eq!(usage.resets_at(now()), Some(now() + time::Duration::DAY));
        assert_eq!(usage.used_percent(now() + time::Duration::WEEK), 0);
    }
}
