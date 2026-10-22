//! GitHub accounts for Agents.
//!
//! agentd keeps one GitHub token per owner, separate from any `gh` login on the host, so Agents
//! can act as a separate account such as a bot. A fine-grained token belongs to the one owner it
//! was created for; a classic or OAuth token reaches every owner and is kept as the fallback. An
//! Agent opts in with `spec.accounts: [{type: github, owner: …}]`; its Sandbox then sees an inert
//! `GITHUB_TOKEN` that the network mediator substitutes only in requests to GitHub's own hosts,
//! and Git commits carry the account's name and email.

mod gh;
mod inspect;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use gh::{gh_installed, host_login, token_from_gh, token_from_gh_sign_in};
pub use inspect::{Inspection, OwnerKind, SCOPES, inspect, owner_kind};

use crate::{Error, harness::MediatedSecret, persistence};
use sandbox::secret_store::SecretReference;

/// Provider account name of the fallback token; owner tokens append `/<owner>`.
const PROVIDER: &str = "github";
/// Secret name of the fallback token; owner tokens append `/<owner>`.
const ACCESS_SECRET: &str = "github-access-token";
pub(crate) const ACCESS_ENVIRONMENT: &str = "GITHUB_TOKEN";
/// `gh` attaches files only with a token it classifies by prefix, so the placeholder carries the
/// fine-grained PAT prefix. Manifests that bind their own GitHub secret use the same value.
const ACCESS_PLACEHOLDER: &str = "github_pat_AGENT_MEDIATED_GITHUB_TOKEN";
/// The hosts `gh` and Git over HTTPS reach; the token is never substituted anywhere else.
const MEDIATED_HOSTS: [&str; 4] = ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"];
/// Where a token that reaches every owner is used, in words.
pub const EVERY_OTHER_ORGANIZATION: &str = "every organization without a token of its own";
/// How long before expiry a token is reported as expiring soon.
pub const EXPIRY_WARNING: time::Duration = time::Duration::days(7);
/// Days a token created from [`token_form_url`] lives, unless the owner's policy allows less.
pub const TOKEN_LIFETIME_DAYS: u32 = 90;
/// Fine-grained repository permissions an Agent needs.
///
/// They let it push code, work issues and pull requests, run workflows and read their logs,
/// change workflow files, and read commit statuses for `gh pr checks`. Metadata read access comes
/// with any of them.
pub const PERMISSIONS: [(&str, &str); 6] = [
    ("contents", "write"),
    ("issues", "write"),
    ("pull_requests", "write"),
    ("actions", "write"),
    ("workflows", "write"),
    ("statuses", "read"),
];
const TOKEN_FORM: &str = "https://github.com/settings/personal-access-tokens/new";

/// The GitHub form that creates a fine-grained token for `owner`, pre-filled.
///
/// [`PERMISSIONS`] and [`TOKEN_LIFETIME_DAYS`] are filled in. GitHub offers no parameter for the
/// repositories, which the person picks in the form.
#[must_use]
pub fn token_form_url(owner: &str, name: &str, description: &str) -> String {
    let lifetime = TOKEN_LIFETIME_DAYS.to_string();
    let parameters = [
        ("name", name),
        ("description", description),
        ("target_name", owner),
        ("expires_in", lifetime.as_str()),
    ]
    .into_iter()
    .chain(PERMISSIONS);
    reqwest::Url::parse_with_params(TOKEN_FORM, parameters).map_or_else(|_| TOKEN_FORM.to_owned(), String::from)
}

/// The GitHub account a token acts as.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAccount {
    /// The account's login, such as `octocat`.
    pub login: String,
    /// Name used for Git commits: the profile name, or the login when the profile has none.
    pub name: String,
    /// Email used for Git commits: the public profile email, or the account's noreply address.
    pub email: String,
}

/// What kind of GitHub token a value is, by its documented prefix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenKind {
    /// A fine-grained personal access token: one owner, selected repositories and permissions.
    FineGrained,
    /// A classic personal access token, limited only by its scopes.
    Classic,
    /// An OAuth token, such as the GitHub CLI's own.
    Oauth,
    /// Any other token GitHub accepted.
    Other,
}

impl TokenKind {
    /// The kind of `token`.
    #[must_use]
    pub fn of(token: &str) -> Self {
        let token = token.trim();
        if token.starts_with("github_pat_") {
            Self::FineGrained
        } else if token.starts_with("ghp_") {
            Self::Classic
        } else if token.starts_with("gho_") {
            Self::Oauth
        } else {
            Self::Other
        }
    }

    /// Whether the token is bound to the one owner it was created for.
    #[must_use]
    pub const fn single_owner(self) -> bool {
        matches!(self, Self::FineGrained)
    }

    /// What to show as the expiry when GitHub named none: tokens that cannot expire never do, and
    /// a fine-grained token without a stated expiry is unknown.
    #[must_use]
    pub const fn unstated_expiry(self) -> &'static str {
        if self.single_owner() { "unknown" } else { "never" }
    }

    /// A short label for listings.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FineGrained => "fine-grained",
            Self::Classic => "classic",
            Self::Oauth => "OAuth",
            Self::Other => "other",
        }
    }

    /// How to revoke a token of this kind on GitHub, signed in as `login`, which forgetting it in
    /// agentctl does not do; `None` for the GitHub CLI's sign-in, whose token no one else ever held.
    #[must_use]
    pub fn revoke_hint(self, login: &str) -> Option<String> {
        Some(match self {
            Self::FineGrained => format!(
                "The fine-grained token stays valid on GitHub until it expires or you revoke it, signed in as {login}: \
                 https://github.com/settings/personal-access-tokens"
            ),
            Self::Classic => format!(
                "The classic token stays valid on GitHub until it expires or you revoke it, signed in as {login}: \
                 https://github.com/settings/tokens"
            ),
            // agentctl read it from a GitHub CLI configuration it deleted at once, so the only
            // copy was agentd's.
            Self::Oauth => return None,
            Self::Other => format!(
                "The token stays valid on GitHub until you revoke it where it was made, signed in as {login}: \
                 https://github.com/settings/applications"
            ),
        })
    }
}

/// A GitHub repository, as `owner/name`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Repository {
    /// The user or organization that owns it, lowercased.
    pub owner: String,
    /// The repository name.
    pub name: String,
}

impl Repository {
    /// A repository of `owner`, which is normalized to lowercase.
    #[must_use]
    pub fn new(owner: &str, name: &str) -> Self {
        Self {
            owner: owner.to_ascii_lowercase(),
            name: name.to_owned(),
        }
    }

    /// The GitHub repository a Git remote URL names, in its HTTPS, SSH or `scp`-like form.
    #[must_use]
    pub fn from_remote(url: &str) -> Option<Self> {
        let url = url.trim();
        let path = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
            .or_else(|| url.strip_prefix("ssh://git@github.com/"))
            .or_else(|| url.strip_prefix("git@github.com:"))?;
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        (valid_owner(owner) && !name.is_empty() && !name.contains('/')).then(|| Self::new(owner, name))
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.name)
    }
}

/// Whether `owner` can be a GitHub user or organization login.
#[must_use]
pub fn valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// A stored GitHub token, without the token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubToken {
    /// The owner the token is kept for, lowercased; `None` for the fallback used by every owner
    /// without one of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The account the token acts as.
    #[serde(flatten)]
    pub account: GithubAccount,
    /// What kind of token it is.
    pub kind: TokenKind,
    /// When GitHub said the token expires, at login; `None` when it does not, or did not say.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub expires_at: Option<time::OffsetDateTime>,
}

impl GithubToken {
    /// Whether the token has expired at `now`.
    #[must_use]
    pub fn expired(&self, now: time::OffsetDateTime) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    /// Whether the token expires within [`EXPIRY_WARNING`] of `now`, or already has.
    #[must_use]
    pub fn expires_soon(&self, now: time::OffsetDateTime) -> bool {
        self.expires_at
            .is_some_and(|expires_at| expires_at <= now + EXPIRY_WARNING)
    }

    /// `owner`, or `every organization without a token of its own` for a token that reaches every
    /// owner.
    #[must_use]
    pub fn scope(&self) -> &str {
        self.owner.as_deref().unwrap_or(EVERY_OTHER_ORGANIZATION)
    }
}

/// A stored token and what GitHub reports about it now.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubCheck {
    /// The stored token an Agent of the owner asked about would use.
    pub token: GithubToken,
    /// What GitHub reports about that token.
    pub inspection: Inspection,
}

/// Which stored tokens `agentctl github logout` forgets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Logout {
    /// The token kept for this owner.
    Owner(String),
    /// The token that reaches every owner without one of its own.
    AnyOther,
    /// Every stored token.
    Everywhere,
}

/// Where a token for `owner`, or the fallback, is kept.
fn names(owner: Option<&str>) -> (String, String) {
    owner.map_or_else(
        || (PROVIDER.to_owned(), ACCESS_SECRET.to_owned()),
        |owner| (format!("{PROVIDER}/{owner}"), format!("{ACCESS_SECRET}/{owner}")),
    )
}

/// Validates and stores GitHub tokens.
pub(crate) struct Authentication {
    database: persistence::Database,
    api: String,
}

impl Authentication {
    pub(crate) fn new(database: persistence::Database) -> Self {
        Self {
            database,
            api: inspect::API.into(),
        }
    }

    /// Asks GitHub about `token`, then keeps it for `owner`, or as the fallback for every owner.
    ///
    /// A fine-grained token reaches only the owner it was created for, so it needs `owner`.
    ///
    /// # Errors
    ///
    /// Returns an error when the token or owner is invalid, GitHub rejects the token or cannot be
    /// reached, the token has expired, or it cannot be persisted.
    pub(crate) async fn login(&self, token: Zeroizing<String>, owner: Option<&str>) -> Result<GithubToken, Error> {
        let token = Zeroizing::new(token.trim().to_owned());
        if token.is_empty() {
            return Err(Error::Invalid("no GitHub token was provided".into()));
        }
        let owner = owner.map(str::to_ascii_lowercase);
        if let Some(owner) = owner.as_deref().filter(|owner| !valid_owner(owner)) {
            return Err(Error::Invalid(format!(
                "{owner:?} is not a GitHub user or organization"
            )));
        }
        let kind = TokenKind::of(&token);
        if kind.single_owner() && owner.is_none() {
            return Err(Error::Invalid(
                "a fine-grained token belongs to one owner; name it with `--owner`".into(),
            ));
        }
        let inspection = inspect::inspect_at(&self.api, &token, None).await?;
        let stored = GithubToken {
            owner,
            account: inspection.account,
            kind,
            expires_at: inspection.expires_at,
        };
        if stored.expired(time::OffsetDateTime::now_utc()) {
            return Err(Error::Invalid("the GitHub token has already expired".into()));
        }
        let (provider, secret) = names(stored.owner.as_deref());
        self.database
            .put_provider_account(persistence::ProviderAccountWrite {
                provider,
                credentials: vec![persistence::StoredSecret {
                    name: secret,
                    value: Zeroizing::new(token.as_bytes().to_vec()),
                }],
                metadata_json: serde_json::to_string(&stored)?,
            })
            .await?;
        Ok(stored)
    }

    /// Asks GitHub about the token an Agent of `owner` would use: whether it still works, when it
    /// expires, and whether it can see `repository`.
    ///
    /// # Errors
    ///
    /// Returns an error when no stored token applies, or GitHub rejects it or cannot be reached.
    pub(crate) async fn check(
        &self,
        owner: Option<&str>,
        repository: Option<&Repository>,
    ) -> Result<GithubCheck, Error> {
        use sandbox::secret_store::SecretStore as _;

        let (stored, reference) = select(&self.database, owner).await?;
        let token = self.database.resolve(&reference).await?;
        let token = Zeroizing::new(String::from_utf8_lossy(token.expose()).into_owned());
        let inspection = inspect::inspect_at(&self.api, &token, repository).await?;
        Ok(GithubCheck {
            token: stored,
            inspection,
        })
    }

    #[cfg(test)]
    fn with_api(mut self, api: String) -> Self {
        self.api = api;
        self
    }
}

/// Forgets the stored tokens `logout` names, returning them. Their Agents lose GitHub access on
/// their next reconciliation, which then fails naming the login to run. The tokens stay valid on
/// GitHub until revoked there.
///
/// # Errors
///
/// Returns an error when the database cannot be read or written.
pub(crate) async fn forget(database: &persistence::Database, logout: &Logout) -> Result<Vec<GithubToken>, Error> {
    let forgotten: Vec<GithubToken> = tokens(database)
        .await?
        .into_iter()
        .filter(|token| match logout {
            Logout::Owner(owner) => token.owner.as_deref() == Some(owner.to_ascii_lowercase().as_str()),
            Logout::AnyOther => token.owner.is_none(),
            Logout::Everywhere => true,
        })
        .collect();
    for token in &forgotten {
        let (provider, secret) = names(token.owner.as_deref());
        database.delete_provider_account(&provider, vec![secret]).await?;
    }
    Ok(forgotten)
}

/// Every stored GitHub token, owners first and the fallback last.
///
/// # Errors
///
/// Returns an error when the database cannot be read or holds an unreadable token record.
pub(crate) async fn tokens(database: &persistence::Database) -> Result<Vec<GithubToken>, Error> {
    let owned = format!("{PROVIDER}/");
    let mut tokens = database
        .provider_accounts(PROVIDER)
        .await?
        .into_iter()
        .filter(|(provider, _)| provider == PROVIDER || provider.starts_with(&owned))
        .map(|(_, metadata)| serde_json::from_str::<GithubToken>(&metadata).map_err(Error::from))
        .collect::<Result<Vec<_>, _>>()?;
    tokens.sort_by(|left, right| (left.owner.is_none(), &left.owner).cmp(&(right.owner.is_none(), &right.owner)));
    Ok(tokens)
}

/// The stored token an Agent of `owner` uses: the owner's own, else the fallback. Without an
/// owner, the only stored token.
async fn select(
    database: &persistence::Database,
    owner: Option<&str>,
) -> Result<(GithubToken, SecretReference), Error> {
    let tokens = tokens(database).await?;
    let selected = match owner.map(str::to_ascii_lowercase) {
        Some(owner) => tokens
            .iter()
            .find(|token| token.owner.as_deref() == Some(owner.as_str()))
            .or_else(|| tokens.iter().find(|token| token.owner.is_none()))
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "no GitHub token for {owner}; run `agentctl github login --owner {owner}`"
                ))
            })?,
        None => match tokens.as_slice() {
            [token] => token,
            [] => {
                return Err(Error::Invalid(
                    "the agent uses a GitHub account, but no token is stored; run `agentctl github login`".into(),
                ));
            }
            _ => {
                return Err(Error::Invalid(
                    "several GitHub tokens are stored; name the owner with `accounts: [{type: github, owner: …}]`"
                        .into(),
                ));
            }
        },
    };
    let (_, secret) = names(selected.owner.as_deref());
    Ok((selected.clone(), SecretReference::from_opaque(secret)))
}

/// The mediated `GITHUB_TOKEN` an Agent of `owner` receives, and the account it acts as.
///
/// # Errors
///
/// Returns an error naming `agentctl github login` when no token applies or it has expired.
pub(crate) async fn prepare(
    database: &persistence::Database,
    owner: Option<&str>,
    now: time::OffsetDateTime,
) -> Result<(MediatedSecret, GithubAccount), Error> {
    let (token, reference) = select(database, owner).await?;
    if let Some(expires_at) = token.expires_at.filter(|_| token.expired(now)) {
        let login = token.owner.as_deref().map_or_else(
            || "agentctl github login".to_owned(),
            |owner| format!("agentctl github login --owner {owner}"),
        );
        return Err(Error::Invalid(format!(
            "the GitHub token for {} expired on {}; run `{login}`",
            token.scope(),
            expires_at.date()
        )));
    }
    let secret = MediatedSecret {
        environment: ACCESS_ENVIRONMENT,
        placeholder: ACCESS_PLACEHOLDER.into(),
        reference,
        allowed_hosts: MEDIATED_HOSTS.iter().map(|host| (*host).to_owned()).collect(),
    };
    Ok((secret, token.account))
}

/// Whether a manifest secret would collide with the binding the GitHub account owns. Mediation
/// needs placeholders that never contain one another, so overlapping ones are refused at apply.
pub(crate) fn conflicts_with_managed_secret(name: &str, placeholder: Option<&str>) -> bool {
    name == ACCESS_ENVIRONMENT
        || placeholder.is_some_and(|placeholder| {
            placeholder.contains(ACCESS_PLACEHOLDER) || ACCESS_PLACEHOLDER.contains(placeholder)
        })
}

/// Whether `login` follows the convention of naming automation accounts `…-bot` or `…-agent`,
/// or is a GitHub App's `…[bot]`. A hint for people, not a rule GitHub enforces.
#[must_use]
pub fn looks_like_automation(login: &str) -> bool {
    let login = login.to_ascii_lowercase();
    login.contains("-bot") || login.contains("-agent") || login.ends_with("[bot]")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use sandbox::secret_store::SecretStore as _;
    use tempfile::TempDir;

    use super::inspect::tests::{Answer, BOT, serve};
    use super::*;

    fn database(directory: &TempDir) -> persistence::Database {
        persistence::Database::open(&directory.path().join("agent.db")).expect("database")
    }

    async fn api_with(headers: &'static str) -> String {
        let (api, _requests) = serve(vec![Answer {
            path: "/user",
            status: "200 OK",
            headers,
            body: BOT,
        }])
        .await;
        api
    }

    fn at(seconds: i64) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(seconds).expect("timestamp")
    }

    #[tokio::test(flavor = "local")]
    async fn tokens_are_kept_per_owner_and_agents_get_their_owners_or_the_fallback() {
        let directory = TempDir::new().expect("temporary directory");
        let database = database(&directory);
        let manager = Authentication::new(database.clone()).with_api(api_with("").await);
        manager
            .login(Zeroizing::new("github_pat_digdir".into()), Some("Digdir"))
            .await
            .expect("digdir token");
        manager
            .login(Zeroizing::new("github_pat_altinn".into()), Some("altinn"))
            .await
            .expect("altinn token");

        let listed = tokens(&database).await.expect("tokens");
        assert_eq!(
            listed.iter().map(|token| token.owner.as_deref()).collect::<Vec<_>>(),
            [Some("altinn"), Some("digdir")]
        );
        let now = at(0);
        for (owner, expected) in [("digdir", "github_pat_digdir"), ("ALTINN", "github_pat_altinn")] {
            let (secret, account) = prepare(&database, Some(owner), now).await.expect("prepare");
            assert_eq!(account.login, "digdir-bot");
            let value = database.resolve(&secret.reference).await.expect("resolve");
            assert_eq!(value.expose(), expected.as_bytes(), "{owner}");
        }
        assert!(
            prepare(&database, Some("elsewhere"), now)
                .await
                .is_err_and(|error| error.to_string().contains("--owner elsewhere"))
        );
        assert!(
            prepare(&database, None, now)
                .await
                .is_err_and(|error| error.to_string().contains("several GitHub tokens"))
        );

        manager
            .login(Zeroizing::new("ghp_everywhere".into()), None)
            .await
            .expect("fallback token");
        let (secret, _) = prepare(&database, Some("elsewhere"), now).await.expect("fallback");
        assert_eq!(
            database.resolve(&secret.reference).await.expect("resolve").expose(),
            b"ghp_everywhere"
        );
        assert_eq!(
            tokens(&database)
                .await
                .expect("tokens")
                .last()
                .and_then(|token| token.owner.clone()),
            None,
            "the fallback lists last"
        );
    }

    #[tokio::test(flavor = "local")]
    async fn a_fine_grained_token_needs_its_owner_and_owners_must_be_github_names() {
        let directory = TempDir::new().expect("temporary directory");
        let manager = Authentication::new(database(&directory)).with_api(api_with("").await);
        assert!(
            manager
                .login(Zeroizing::new("github_pat_x".into()), None)
                .await
                .is_err_and(|error| error.to_string().contains("--owner"))
        );
        manager
            .login(Zeroizing::new("ghp_x".into()), Some("not/an/owner"))
            .await
            .expect_err("invalid owner");
    }

    #[tokio::test(flavor = "local")]
    async fn an_expired_token_is_refused_at_login_and_blocks_its_agents() {
        let directory = TempDir::new().expect("temporary directory");
        let database = database(&directory);
        let past = Authentication::new(database.clone())
            .with_api(api_with("GitHub-Authentication-Token-Expiration: 2001-01-01 00:00:00 UTC\r\n").await);
        past.login(Zeroizing::new("github_pat_old".into()), Some("digdir"))
            .await
            .expect_err("already expired");
        assert!(tokens(&database).await.expect("tokens").is_empty());

        let future = Authentication::new(database.clone())
            .with_api(api_with("GitHub-Authentication-Token-Expiration: 2099-01-01 00:00:00 UTC\r\n").await);
        let stored = future
            .login(Zeroizing::new("github_pat_new".into()), Some("digdir"))
            .await
            .expect("valid until 2099");
        let expires_at = stored.expires_at.expect("expiry");

        prepare(&database, Some("digdir"), expires_at - time::Duration::seconds(1))
            .await
            .expect("usable until it expires");
        let Err(error) = prepare(&database, Some("digdir"), expires_at).await else {
            panic!("an expired token must not be mediated");
        };
        assert!(
            error.to_string().contains("expired on 2099-01-01")
                && error.to_string().contains("agentctl github login --owner digdir"),
            "{error}"
        );
        assert!(stored.expires_soon(expires_at - time::Duration::days(6)));
        assert!(!stored.expires_soon(expires_at - time::Duration::days(8)));
    }

    #[tokio::test(flavor = "local")]
    async fn checking_a_stored_token_asks_github_with_it() {
        let directory = TempDir::new().expect("temporary directory");
        let database = database(&directory);
        let (api, requests) = serve(vec![
            Answer {
                path: "/user",
                status: "200 OK",
                headers: "",
                body: BOT,
            },
            Answer {
                path: "/repos/digdir/agents",
                status: "200 OK",
                headers: "",
                body: "{}",
            },
        ])
        .await;
        let manager = Authentication::new(database).with_api(api);
        manager
            .login(Zeroizing::new("github_pat_stored".into()), Some("digdir"))
            .await
            .expect("login");

        let check = manager
            .check(Some("digdir"), Some(&Repository::new("digdir", "agents")))
            .await
            .expect("check");

        assert_eq!(check.token.owner.as_deref(), Some("digdir"));
        assert_eq!(check.inspection.repository.map(|(_, visible)| visible), Some(true));
        assert!(
            requests
                .borrow()
                .iter()
                .any(|request| request.starts_with("GET /repos/digdir/agents ")
                    && request.contains("Bearer github_pat_stored")),
            "{:?}",
            requests.borrow()
        );
    }

    #[tokio::test(flavor = "local")]
    async fn the_mediated_token_reaches_only_github_hosts() {
        let directory = TempDir::new().expect("temporary directory");
        let database = database(&directory);
        Authentication::new(database.clone())
            .with_api(api_with("").await)
            .login(Zeroizing::new("ghp_token".into()), None)
            .await
            .expect("login");

        let (secret, account) = prepare(&database, None, at(0)).await.expect("prepare");

        assert_eq!(account.login, "digdir-bot");
        assert_eq!(secret.environment, "GITHUB_TOKEN");
        assert_eq!(secret.placeholder, ACCESS_PLACEHOLDER);
        assert_eq!(
            secret.allowed_hosts,
            ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"]
        );
    }

    #[tokio::test(flavor = "local")]
    async fn logging_out_forgets_one_owner_any_other_or_everything() {
        let directory = TempDir::new().expect("temporary directory");
        let database = database(&directory);
        let manager = Authentication::new(database.clone()).with_api(api_with("").await);
        for (token, owner) in [
            ("github_pat_digdir", Some("digdir")),
            ("github_pat_altinn", Some("altinn")),
            ("ghp_everywhere", None),
        ] {
            manager.login(Zeroizing::new(token.into()), owner).await.expect("login");
        }
        let owners = |tokens: Vec<GithubToken>| tokens.into_iter().map(|token| token.owner).collect::<Vec<_>>();
        let digdir_secret = SecretReference::from_opaque("github-access-token/digdir");

        assert_eq!(
            owners(forget(&database, &Logout::Owner("DigDir".into())).await.expect("owner")),
            [Some("digdir".to_owned())]
        );
        database
            .resolve(&digdir_secret)
            .await
            .expect_err("the token itself is gone");
        let (secret, _) = prepare(&database, Some("digdir"), at(0)).await.expect("any other");
        assert_eq!(
            database.resolve(&secret.reference).await.expect("resolve").expose(),
            b"ghp_everywhere"
        );

        assert_eq!(
            owners(forget(&database, &Logout::AnyOther).await.expect("any other")),
            [None]
        );
        assert!(
            prepare(&database, Some("digdir"), at(0))
                .await
                .is_err_and(|error| error.to_string().contains("agentctl github login --owner digdir"))
        );

        assert_eq!(
            owners(forget(&database, &Logout::Everywhere).await.expect("everywhere")),
            [Some("altinn".to_owned())]
        );
        assert!(tokens(&database).await.expect("tokens").is_empty());
        assert!(
            forget(&database, &Logout::Everywhere)
                .await
                .expect("nothing left")
                .is_empty()
        );
    }

    #[test]
    fn the_token_form_is_prefilled_with_the_owner_lifetime_and_permissions() {
        let url = reqwest::Url::parse(&token_form_url("digdir", "agentctl digdir-agents", "Agents on my-mac"))
            .expect("valid URL");
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let value = |key: &str| {
            query
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(url.path(), "/settings/personal-access-tokens/new");
        assert_eq!(value("name"), Some("agentctl digdir-agents"));
        assert_eq!(value("target_name"), Some("digdir"));
        assert_eq!(value("expires_in"), Some("90"));
        for (permission, access) in PERMISSIONS {
            assert_eq!(value(permission), Some(access), "{permission}");
        }
    }

    #[test]
    fn repositories_are_read_from_every_form_of_github_remote() {
        let expected = Some(Repository::new("digdir", "digdir-agents"));
        for remote in [
            "https://github.com/digdir/digdir-agents.git",
            "https://github.com/Digdir/digdir-agents",
            "git@github.com:digdir/digdir-agents.git",
            "ssh://git@github.com/digdir/digdir-agents.git",
        ] {
            assert_eq!(Repository::from_remote(remote), expected, "{remote}");
        }
        for remote in [
            "https://gitlab.com/digdir/digdir-agents.git",
            "https://github.com/digdir",
            "git@github.com:-bad/repo.git",
        ] {
            assert_eq!(Repository::from_remote(remote), None, "{remote}");
        }
    }

    #[test]
    fn token_kinds_follow_githubs_prefixes() {
        assert_eq!(TokenKind::of("github_pat_x"), TokenKind::FineGrained);
        assert_eq!(TokenKind::of("ghp_x"), TokenKind::Classic);
        assert_eq!(TokenKind::of("gho_x"), TokenKind::Oauth);
        assert_eq!(TokenKind::of("ghu_x"), TokenKind::Other);
        assert!(TokenKind::FineGrained.single_owner());
        assert!(!TokenKind::Oauth.single_owner());
    }

    #[test]
    fn automation_accounts_are_recognized_by_convention() {
        for login in ["digdir-bot", "altinn-agent-1", "renovate[bot]", "Team-Bot"] {
            assert!(looks_like_automation(login), "{login}");
        }
        for login in ["danielskovli", "robot", "agentsmith"] {
            assert!(!looks_like_automation(login), "{login}");
        }
    }
}
