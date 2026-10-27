//! GitHub accounts for Agents.
//!
//! agentd keeps GitHub tokens under names that manifests ask for, such as `altinn-studio`, separate
//! from any `gh` login on the host. Before keeping a token it asks GitHub which
//! account the token acts as and when it expires. The token stays in the host's secret store;
//! what is listed and returned about it never contains it.

mod gh;
mod inspect;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use gh::sign_in;
pub use inspect::SCOPES;

use crate::{Error, harness::MediatedSecret, persistence};
use sandbox::secret_store::SecretReference;

/// Provider account namespace; each token's account is `github/<name>`.
const PROVIDER: &str = "github";
/// Secret name prefix; each token is `github-access-token/<name>`.
const ACCESS_SECRET: &str = "github-access-token";
/// The variable that carries the inert token in the Sandbox.
pub(crate) const ACCESS_ENVIRONMENT: &str = "GITHUB_TOKEN";
/// `gh` attaches files only with a token it classifies by prefix, so the placeholder carries the
/// fine-grained token prefix.
const ACCESS_PLACEHOLDER: &str = "github_pat_AGENT_MEDIATED_GITHUB_TOKEN";
/// The hosts `gh` and Git over HTTPS reach; the token is substituted nowhere else.
const MEDIATED_HOSTS: [&str; 4] = ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"];
/// The longest login GitHub allows.
const OWNER_MAX_LENGTH: usize = 39;
/// The longest name a token is stored under.
const NAME_MAX_LENGTH: usize = 64;
/// What a token's name must be, said without repeating the value, which may be a misplaced token.
pub const NAME_RULE: &str = "the name must be letters, digits and inner hyphens, at most 64 characters";

/// The GitHub account a token acts as.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAccount {
    /// The account's login, such as `octocat`.
    pub login: String,
    /// Name for Git commits: the profile name, or the login when the profile has none.
    pub name: String,
    /// Email for Git commits: the public profile email, or the account's noreply address.
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

    /// The kind in words, such as `fine-grained`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FineGrained => "fine-grained",
            Self::Classic => "classic",
            Self::Oauth => "OAuth",
            Self::Other => "other",
        }
    }

    /// The GitHub settings page where a token of this kind is revoked, signed in as its account.
    #[must_use]
    pub const fn revoke_page(self) -> &'static str {
        match self {
            Self::FineGrained => "https://github.com/settings/personal-access-tokens",
            Self::Classic => "https://github.com/settings/tokens",
            Self::Oauth | Self::Other => "https://github.com/settings/applications",
        }
    }

    /// Whether the token reaches only the one owner it was created for, rather than every
    /// repository its account can.
    #[must_use]
    pub const fn single_owner(self) -> bool {
        matches!(self, Self::FineGrained)
    }
}

/// Whether `owner` can be a GitHub user or organization login: ASCII letters, digits and inner
/// hyphens, at most 39 characters. Owners name secrets and URL paths, so nothing else passes.
#[must_use]
pub fn valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= OWNER_MAX_LENGTH
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether `name` can name a stored token: ASCII letters, digits and inner hyphens, at most 64
/// characters. Names name secrets, so nothing else passes.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX_LENGTH
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// A token's name after `text`, such as a repository or folder name: in lowercase, with hyphens
/// for anything [`valid_name`] does not take, such as `app-lib-net` for `App_Lib.NET`; `None`
/// when nothing is left.
#[must_use]
pub fn name_after(text: &str) -> Option<String> {
    let mut name = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_lowercase());
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_end_matches('-');
    let name = name.get(..NAME_MAX_LENGTH).unwrap_or(name).trim_end_matches('-');
    valid_name(name).then(|| name.to_owned())
}

/// How long before its expiry a token is called out at login.
pub const EXPIRY_WARNING: time::Duration = time::Duration::days(7);
/// Days a token created from [`token_form_url`] lives, unless the owner's policy allows less.
pub const TOKEN_LIFETIME_DAYS: u32 = 90;
/// Fine-grained repository permissions an Agent needs: push code, work issues and pull requests,
/// run workflows and read their logs, change workflow files, and read commit statuses.
pub const PERMISSIONS: [(&str, &str); 6] = [
    ("contents", "write"),
    ("issues", "write"),
    ("pull_requests", "write"),
    ("actions", "write"),
    ("workflows", "write"),
    ("statuses", "read"),
];
const TOKEN_FORM: &str = "https://github.com/settings/personal-access-tokens/new";

/// GitHub's form creating a fine-grained token for `owner`, pre-filled.
///
/// `name`, `description`, [`PERMISSIONS`] and [`TOKEN_LIFETIME_DAYS`] are filled in. GitHub takes
/// no parameter for the repositories, which the person picks in the form.
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

/// A GitHub repository, shown as `owner/name`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    /// The user or organization that owns it, lowercased.
    pub owner: String,
    /// The repository name.
    pub name: String,
}

impl Repository {
    /// The GitHub repository a Git remote URL names, in its HTTPS, SSH or `scp`-like form; `None`
    /// for any other host, or a URL carrying credentials.
    #[must_use]
    pub fn from_remote(url: &str) -> Option<Self> {
        let url = url.trim();
        let path = ["https://github.com/", "ssh://git@github.com/", "git@github.com:"]
            .iter()
            .find_map(|prefix| url.strip_prefix(prefix))?;
        let path = path.trim_end_matches('/');
        let (owner, name) = path.strip_suffix(".git").unwrap_or(path).split_once('/')?;
        let valid_repository = !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        (valid_owner(owner) && valid_repository).then(|| Self {
            owner: owner.to_ascii_lowercase(),
            name: name.to_owned(),
        })
    }

    /// The name a token for this repository is suggested under, as [`name_after`] makes it.
    #[must_use]
    pub fn token_name(&self) -> Option<String> {
        name_after(&self.name)
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.name)
    }
}

/// Whether `token` can see `repository` on GitHub.
///
/// # Errors
///
/// Returns an error, never containing the token, when GitHub cannot be reached or answers
/// unexpectedly.
pub async fn can_see(token: &str, repository: &Repository) -> Result<bool, Error> {
    inspect::can_see_at(inspect::API, token.trim(), repository).await
}

/// A stored GitHub token, without the token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubToken {
    /// The name the token is stored under and manifests ask for, lowercased.
    pub name: String,
    /// The account the token acts as.
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

/// What agentd keeps about a token beside it; the name is the key it is kept under.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(flatten)]
    account: GithubAccount,
    kind: TokenKind,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    expires_at: Option<time::OffsetDateTime>,
}

impl Record {
    fn of(token: &GithubToken) -> Self {
        Self {
            account: token.account.clone(),
            kind: token.kind,
            expires_at: token.expires_at,
        }
    }

    fn named(self, name: &str) -> GithubToken {
        GithubToken {
            name: name.into(),
            account: self.account,
            kind: self.kind,
            expires_at: self.expires_at,
        }
    }
}

impl GithubToken {
    /// Whether the token has expired at `now`.
    #[must_use]
    pub fn expired(&self, now: time::OffsetDateTime) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

/// A token as GitHub described it at login, and the recommended [`SCOPES`] it lacks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubLogin {
    /// The token, as it is or would be stored.
    pub token: GithubToken,
    /// Recommended [`SCOPES`] a classic or OAuth token lacks; fine-grained tokens report none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_scopes: Vec<String>,
}

/// The provider account and secret names of the token named `name`. `name` must be valid.
fn names(name: &str) -> (String, String) {
    (format!("{PROVIDER}/{name}"), format!("{ACCESS_SECRET}/{name}"))
}

/// The token named `name` read from its provider account's metadata.
fn read(name: &str, metadata: &str) -> Result<GithubToken, Error> {
    Ok(serde_json::from_str::<Record>(metadata)?.named(name))
}

/// The mediated `GITHUB_TOKEN` an Agent asking for `name` receives, bound to that token only.
///
/// The binding does not depend on a token being stored: the mediator looks the token up on every
/// request, so a login or logout takes effect at once, and GitHub itself rejects an expired one.
/// Requests that carry the placeholder while no token is stored are blocked; nothing else is.
///
/// # Errors
///
/// Returns an error when `name` is not a valid name.
pub(crate) fn mediated_token(name: &str) -> Result<MediatedSecret, Error> {
    let name = name.to_ascii_lowercase();
    if !valid_name(&name) {
        return Err(Error::Invalid(NAME_RULE.into()));
    }
    let (_, secret) = names(&name);
    Ok(MediatedSecret {
        environment: ACCESS_ENVIRONMENT,
        placeholder: ACCESS_PLACEHOLDER.into(),
        reference: SecretReference::from_opaque(secret),
        allowed_hosts: MEDIATED_HOSTS.iter().map(|host| (*host).to_owned()).collect(),
    })
}

/// The token stored as `name`, without the token, expired or not; `None` when none is stored or
/// `name` is not a valid name.
///
/// # Errors
///
/// Returns an error when the database cannot be read.
pub(crate) async fn stored(database: &persistence::Database, name: &str) -> Result<Option<GithubToken>, Error> {
    let name = name.to_ascii_lowercase();
    if !valid_name(&name) {
        return Ok(None);
    }
    let (provider, _) = names(&name);
    let Some(metadata) = database.provider_account_metadata(&provider).await? else {
        return Ok(None);
    };
    Ok(Some(read(&name, &metadata)?))
}

/// Whether a manifest secret would collide with the binding the GitHub account owns. Mediation
/// needs placeholders that never contain one another, so overlapping ones are refused.
pub(crate) fn conflicts_with_managed_secret(name: &str, placeholder: &str) -> bool {
    name == ACCESS_ENVIRONMENT || placeholder.contains(ACCESS_PLACEHOLDER) || ACCESS_PLACEHOLDER.contains(placeholder)
}

/// Asks GitHub what `token` would be as `name`, as a login would, without storing anything, so a
/// person can see it before agentd keeps it.
///
/// # Errors
///
/// Returns an error when the token is empty, the name is not valid, GitHub rejects the token or
/// cannot be reached, or the token has expired. No error contains the token.
pub async fn check(token: &str, name: &str) -> Result<GithubLogin, Error> {
    check_at(inspect::API, token, name).await
}

async fn check_at(api: &str, token: &str, name: &str) -> Result<GithubLogin, Error> {
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::Invalid("no GitHub token was provided".into()));
    }
    // The name is not repeated: a token pasted in its place must not end up in the error.
    if !valid_name(name) {
        return Err(Error::Invalid(NAME_RULE.into()));
    }
    let inspection = inspect::inspect_at(api, token).await?;
    let checked = GithubToken {
        name: name.to_ascii_lowercase(),
        account: inspection.account,
        kind: TokenKind::of(token),
        expires_at: inspection.expires_at,
    };
    if checked.expired(time::OffsetDateTime::now_utc()) {
        return Err(Error::Invalid("the GitHub token has already expired".into()));
    }
    Ok(GithubLogin {
        token: checked,
        missing_scopes: inspection.missing_scopes,
    })
}

/// Checks GitHub tokens with GitHub and keeps them by name.
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

    /// Asks GitHub about `token`, then keeps it as `name`, replacing an earlier token of that name.
    ///
    /// # Errors
    ///
    /// Returns an error as [`check`] does, or when the token cannot be persisted. Nothing is
    /// stored then, and no error contains the token.
    pub(crate) async fn login(&self, token: Zeroizing<String>, name: &str) -> Result<GithubLogin, Error> {
        let token = Zeroizing::new(token.trim().to_owned());
        let checked = check_at(&self.api, &token, name).await?;
        let (provider, secret) = names(&checked.token.name);
        self.database
            .put_provider_account(persistence::ProviderAccountWrite {
                provider,
                credentials: vec![persistence::StoredSecret {
                    name: secret,
                    value: Zeroizing::new(token.as_bytes().to_vec()),
                }],
                metadata_json: serde_json::to_string(&Record::of(&checked.token))?,
            })
            .await?;
        Ok(checked)
    }

    /// Every stored GitHub token, by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or holds an unreadable token record.
    pub(crate) async fn tokens(&self) -> Result<Vec<GithubToken>, Error> {
        self.database
            .provider_accounts(PROVIDER)
            .await?
            .into_iter()
            .map(|(provider, metadata)| {
                let name = provider.strip_prefix(&format!("{PROVIDER}/")).unwrap_or(&provider);
                read(name, &metadata)
            })
            .collect()
    }

    /// Forgets the token named `name`, returning it, or `None` when there was none. The
    /// token stays valid on GitHub until it expires or is revoked there.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or written.
    pub(crate) async fn forget(&self, name: &str) -> Result<Option<GithubToken>, Error> {
        let name = name.to_ascii_lowercase();
        if !valid_name(&name) {
            return Ok(None);
        }
        let (provider, secret) = names(&name);
        let Some(metadata) = self.database.provider_account_metadata(&provider).await? else {
            return Ok(None);
        };
        self.database.delete_provider_account(&provider, vec![secret]).await?;
        Ok(Some(read(&name, &metadata)?))
    }

    #[cfg(test)]
    fn with_api(mut self, api: String) -> Self {
        self.api = api;
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{cell::RefCell, rc::Rc};

    use sandbox::secret_store::{SecretReference, SecretStore as _};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// One canned answer of the fake GitHub API: the status line, extra headers each ending in
    /// CRLF, and the body.
    struct Answer {
        status: &'static str,
        headers: &'static str,
        body: &'static str,
    }

    const BOT: Answer = Answer {
        status: "200 OK",
        headers: "",
        body: r#"{"login":"digdir-bot","id":42,"name":"Digdir Bot","email":"bot@example.com"}"#,
    };

    /// Answers every request with `answer` until the test ends, returning the API base URL and
    /// every request received. Tests never reach the real GitHub.
    async fn serve(answer: Answer) -> (String, Rc<RefCell<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind API endpoint");
        let api = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let seen = requests.clone();
        tokio::task::spawn_local(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut chunk = [0_u8; 4_096];
                let read = stream.read(&mut chunk).await.unwrap_or_default();
                seen.borrow_mut()
                    .push(String::from_utf8_lossy(&chunk[..read]).into_owned());
                let Answer { status, headers, body } = answer;
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        (api, requests)
    }

    struct Fixture {
        _directory: TempDir,
        database: persistence::Database,
        requests: Rc<RefCell<Vec<String>>>,
        github: Authentication,
    }

    async fn fixture(answer: Answer) -> Fixture {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let (api, requests) = serve(answer).await;
        Fixture {
            _directory: directory,
            github: Authentication::new(database.clone()).with_api(api),
            database,
            requests,
        }
    }

    impl Fixture {
        async fn login(&self, token: &str, name: &str) -> Result<GithubLogin, Error> {
            self.github.login(Zeroizing::new(token.into()), name).await
        }

        async fn stored(&self, name: &str) -> Option<Vec<u8>> {
            let (_, secret) = names(name);
            self.database
                .resolve(&SecretReference::from_opaque(secret))
                .await
                .ok()
                .map(|value| value.expose().to_vec())
        }

        async fn token_names(&self) -> Vec<String> {
            let tokens = self.github.tokens().await.expect("tokens");
            tokens.into_iter().map(|token| token.name).collect()
        }
    }

    #[tokio::test(flavor = "local")]
    async fn tokens_are_kept_per_owner_and_a_new_login_replaces_the_owners_token() {
        let github = fixture(BOT).await;
        let login = github
            .login(" github_pat_FAKE_digdir\n", "Digdir")
            .await
            .expect("digdir");
        github.login("ghp_FAKE_altinn", "altinn").await.expect("altinn");

        assert_eq!(login.token.name, "digdir");
        assert_eq!(login.token.kind, TokenKind::FineGrained);
        assert_eq!(login.token.account.login, "digdir-bot");
        assert_eq!(github.token_names().await, ["altinn", "digdir"]);
        assert_eq!(
            github.stored("digdir").await.as_deref(),
            Some(&b"github_pat_FAKE_digdir"[..])
        );
        assert!(
            github.requests.borrow()[0].contains("authorization: Bearer github_pat_FAKE_digdir\r\n"),
            "{:?}",
            github.requests.borrow()
        );

        github.login("github_pat_FAKE_newer", "DIGDIR").await.expect("again");
        assert_eq!(github.token_names().await, ["altinn", "digdir"]);
        assert_eq!(
            github.stored("digdir").await.as_deref(),
            Some(&b"github_pat_FAKE_newer"[..])
        );
        assert_eq!(github.stored("altinn").await.as_deref(), Some(&b"ghp_FAKE_altinn"[..]));
    }

    #[tokio::test(flavor = "local")]
    async fn the_token_is_in_no_record_result_or_debug_output() {
        const TOKEN: &str = "github_pat_FAKE_canary";
        let github = fixture(BOT).await;
        let login = github.login(TOKEN, "digdir").await.expect("login");
        let listed = github.github.tokens().await.expect("tokens");

        let metadata = github.database.provider_accounts(PROVIDER).await.expect("accounts");
        for (index, shown) in [
            format!("{metadata:?}"),
            format!("{login:?}"),
            serde_json::to_string(&login).expect("serialized"),
            format!("{listed:?}"),
            serde_json::to_string(&listed).expect("serialized"),
        ]
        .iter()
        .enumerate()
        {
            // The messages name only the form, never its text: a failure must not print what leaked.
            assert!(!shown.contains("FAKE_canary"), "form {index} holds the token");
            assert!(shown.contains("digdir-bot"), "form {index} lacks the account");
        }
    }

    #[tokio::test(flavor = "local")]
    async fn failed_checks_store_nothing_and_never_name_the_token() {
        const TOKEN: &str = "ghp_FAKE_canary";
        let unreachable = Authentication::new(fixture(BOT).await.database).with_api("http://127.0.0.1:1".into());
        let mut errors = vec![
            unreachable
                .login(Zeroizing::new(TOKEN.into()), "digdir")
                .await
                .expect_err("unreachable"),
        ];
        for answer in [
            Answer {
                status: "401 Unauthorized",
                headers: "",
                body: r#"{"message":"Bad credentials"}"#,
            },
            Answer {
                status: "500 Internal Server Error",
                headers: "",
                body: "{}",
            },
            // A profile that quotes the token where a number belongs.
            Answer {
                status: "200 OK",
                headers: "",
                body: r#"{"login":"digdir-bot","id":"ghp_FAKE_canary"}"#,
            },
            Answer {
                headers: "GitHub-Authentication-Token-Expiration: 2001-01-01 00:00:00 UTC\r\n",
                ..BOT
            },
        ] {
            let github = fixture(answer).await;
            errors.push(github.login(TOKEN, "digdir").await.expect_err("refused"));
            assert_eq!(github.token_names().await, Vec::<String>::new());
            assert_eq!(github.stored("digdir").await, None);
        }
        let github = fixture(BOT).await;
        errors.push(
            github
                .login(TOKEN, TOKEN)
                .await
                .expect_err("a token pasted as the name"),
        );
        errors.push(github.login("  \n", "digdir").await.expect_err("no token"));

        let expected = [
            "could not reach GitHub",
            "rejected the token",
            "answered 500",
            "unexpected user profile",
            "already expired",
            "the name must be letters, digits and inner hyphens",
            "no GitHub token",
        ];
        assert_eq!(errors.len(), expected.len());
        for (error, expected) in errors.iter().zip(expected) {
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("FAKE_canary"), "{message}");
        }
    }

    #[tokio::test(flavor = "local")]
    async fn an_expired_login_keeps_the_earlier_token_of_that_name() {
        let github = fixture(Answer {
            headers: "GitHub-Authentication-Token-Expiration: 2099-01-01 00:00:00 UTC\r\n",
            ..BOT
        })
        .await;
        let login = github
            .login("github_pat_FAKE_current", "digdir")
            .await
            .expect("current");
        let expires_at = login.token.expires_at.expect("expiry");
        assert!(!login.token.expired(expires_at - time::Duration::seconds(1)));
        assert!(login.token.expired(expires_at));

        let expired = Authentication::new(github.database.clone()).with_api(
            serve(Answer {
                headers: "GitHub-Authentication-Token-Expiration: 2001-01-01 00:00:00 UTC\r\n",
                ..BOT
            })
            .await
            .0,
        );
        expired
            .login(Zeroizing::new("github_pat_FAKE_old".into()), "digdir")
            .await
            .expect_err("already expired");
        assert_eq!(
            github.stored("digdir").await.as_deref(),
            Some(&b"github_pat_FAKE_current"[..])
        );
    }

    #[tokio::test(flavor = "local")]
    async fn names_are_checked_before_the_token_is_sent() {
        for name in ["digdir", "Digdir", "a", "a-b", "x2", "altinn-studio", &"a".repeat(64)] {
            assert!(valid_name(name), "{name}");
        }
        let long = "a".repeat(65);
        let invalid = [
            "", "../x", "..", "a/b", "-a", "a-", "a_b", "a.b", "a b", "a%2fb", "æ", &long,
        ];
        for name in invalid {
            assert!(!valid_name(name), "{name}");
        }
        assert!(!valid_owner(&"a".repeat(40)));

        let github = fixture(BOT).await;
        for name in invalid {
            github.login("ghp_FAKE", name).await.expect_err("invalid name");
            assert_eq!(github.github.forget(name).await.expect("forget"), None);
        }
        assert!(github.requests.borrow().is_empty(), "the token went to GitHub");
        assert_eq!(github.token_names().await, Vec::<String>::new());
    }

    #[tokio::test(flavor = "local")]
    async fn logging_out_deletes_the_named_token_itself() {
        let github = fixture(BOT).await;
        github.login("github_pat_FAKE_digdir", "digdir").await.expect("digdir");
        github.login("github_pat_FAKE_altinn", "altinn").await.expect("altinn");

        let forgotten = github.github.forget("DigDir").await.expect("forget");

        assert_eq!(forgotten.map(|token| token.name).as_deref(), Some("digdir"));
        assert_eq!(github.stored("digdir").await, None, "the secret row is gone");
        assert_eq!(github.token_names().await, ["altinn"]);
        assert_eq!(
            github.stored("altinn").await.as_deref(),
            Some(&b"github_pat_FAKE_altinn"[..])
        );
        assert_eq!(github.github.forget("digdir").await.expect("again"), None);
    }

    #[tokio::test(flavor = "local")]
    async fn a_classic_token_reports_the_scopes_it_lacks_and_profiles_fill_in_git_identity() {
        let github = fixture(Answer {
            headers: "X-OAuth-Scopes: repo, gist, admin:org\r\n",
            body: r#"{"login":"digdir-bot","id":42,"name":" ","email":null}"#,
            ..BOT
        })
        .await;

        let login = github.login("ghp_FAKE", "digdir").await.expect("login");

        assert_eq!(login.token.kind, TokenKind::Classic);
        assert!(!login.token.kind.single_owner());
        assert_eq!(login.missing_scopes, ["workflow", "read:project"]);
        assert_eq!(login.token.account.name, "digdir-bot");
        assert_eq!(login.token.account.email, "42+digdir-bot@users.noreply.github.com");
        assert_eq!(login.token.expires_at, None);
        let fine_grained = fixture(BOT)
            .await
            .login("github_pat_FAKE", "digdir")
            .await
            .expect("login");
        assert_eq!(fine_grained.missing_scopes, Vec::<String>::new());
    }

    /// Stores `token` as `name` as a login would, expiring at `expires_at`.
    async fn store(
        database: &persistence::Database,
        name: &str,
        token: &str,
        expires_at: Option<time::OffsetDateTime>,
    ) {
        let (provider, secret) = names(name);
        let stored = Record {
            account: GithubAccount {
                login: format!("{name}-bot"),
                name: format!("{name} bot"),
                email: format!("1+{name}-bot@users.noreply.github.com"),
            },
            kind: TokenKind::of(token),
            expires_at,
        };
        database
            .put_provider_account(persistence::ProviderAccountWrite {
                provider,
                credentials: vec![persistence::StoredSecret {
                    name: secret,
                    value: Zeroizing::new(token.as_bytes().to_vec()),
                }],
                metadata_json: serde_json::to_string(&stored).expect("metadata"),
            })
            .await
            .expect("store");
    }

    #[tokio::test(flavor = "local")]
    async fn an_agent_resolves_only_the_token_it_names_for_githubs_hosts() {
        let github = fixture(BOT).await;
        store(&github.database, "digdir", "github_pat_FAKE_digdir", None).await;
        store(&github.database, "altinn", "github_pat_FAKE_altinn", None).await;

        for (name, expected) in [
            ("Digdir", "github_pat_FAKE_digdir"),
            ("altinn", "github_pat_FAKE_altinn"),
        ] {
            let secret = mediated_token(name).expect("binding");
            let resolved = github.database.resolve(&secret.reference).await.expect("resolve");
            assert_eq!(resolved.expose(), expected.as_bytes(), "{name}");
            assert_eq!(secret.environment, "GITHUB_TOKEN");
            assert_eq!(secret.placeholder, ACCESS_PLACEHOLDER);
            assert_eq!(
                secret.allowed_hosts,
                ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"]
            );
        }

        let unstored = mediated_token("digdir-other").expect("bound without a token");
        assert!(github.database.resolve(&unstored.reference).await.is_err());
        assert!(mediated_token("not a name").is_err());
    }

    #[tokio::test(flavor = "local")]
    async fn the_account_is_known_while_a_token_is_stored_expired_or_not() {
        let github = fixture(BOT).await;
        // 2001-09-09 01:46 UTC, long past.
        let expired = time::OffsetDateTime::from_unix_timestamp(1_000_000_000).expect("timestamp");
        store(&github.database, "digdir", "github_pat_FAKE_canary", Some(expired)).await;

        let known = stored(&github.database, "Digdir").await.expect("account");
        assert_eq!(known.map(|token| token.account.login).as_deref(), Some("digdir-bot"));
        assert_eq!(stored(&github.database, "altinn").await.expect("account"), None);
        assert_eq!(stored(&github.database, "not a name").await.expect("account"), None);
    }

    #[test]
    fn manifest_secrets_may_not_overlap_the_accounts_binding() {
        for (name, placeholder) in [
            ("GITHUB_TOKEN", "anything"),
            ("GH_TOKEN", ACCESS_PLACEHOLDER),
            ("GH_TOKEN", "github_pat_AGENT"),
            ("GH_TOKEN", "x_github_pat_AGENT_MEDIATED_GITHUB_TOKEN_x"),
        ] {
            assert!(conflicts_with_managed_secret(name, placeholder), "{name} {placeholder}");
        }
        assert!(!conflicts_with_managed_secret("GH_TOKEN", "$AGENT_SECRET_GH_TOKEN"));
    }

    #[test]
    fn token_kinds_follow_githubs_prefixes() {
        assert_eq!(TokenKind::of("github_pat_x"), TokenKind::FineGrained);
        assert_eq!(TokenKind::of("ghp_x"), TokenKind::Classic);
        assert_eq!(TokenKind::of("gho_x"), TokenKind::Oauth);
        assert_eq!(TokenKind::of("ghu_x"), TokenKind::Other);
        assert!(TokenKind::FineGrained.single_owner());
    }

    #[test]
    fn a_repositorys_token_name_is_its_name_in_lowercase_with_hyphens() {
        let name = |repository: &str| {
            Repository {
                owner: "digdir".into(),
                name: repository.into(),
            }
            .token_name()
        };
        assert_eq!(name("altinn-studio").as_deref(), Some("altinn-studio"));
        assert_eq!(name("App_Lib.NET").as_deref(), Some("app-lib-net"));
        assert_eq!(name("_.x").as_deref(), Some("x"));
        assert_eq!(name(&"a".repeat(70)), Some("a".repeat(64)));
        assert_eq!(name("__"), None);
    }

    #[test]
    fn the_token_form_is_prefilled_with_the_owner_lifetime_and_permissions_and_nothing_secret() {
        let url = reqwest::Url::parse(&token_form_url(
            "digdir",
            "agentctl digdir on my-mac",
            "Agents for digdir on my-mac, stored by agentctl",
        ))
        .expect("valid URL");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("github.com"));
        assert_eq!(url.path(), "/settings/personal-access-tokens/new");
        let mut query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        query.sort();
        let mut expected: Vec<(String, String)> = [
            ("name", "agentctl digdir on my-mac"),
            ("description", "Agents for digdir on my-mac, stored by agentctl"),
            ("target_name", "digdir"),
            ("expires_in", "90"),
            ("contents", "write"),
            ("issues", "write"),
            ("pull_requests", "write"),
            ("actions", "write"),
            ("workflows", "write"),
            ("statuses", "read"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .to_vec();
        expected.sort();
        assert_eq!(query, expected, "exactly these parameters, so no token or credential");
        assert_eq!(url.username(), "");
        assert_eq!(url.password(), None);
        assert_eq!(url.fragment(), None);
    }

    #[test]
    fn remotes_name_a_github_repository_in_https_ssh_or_scp_form() {
        let expected = Some(Repository {
            owner: "digdir".into(),
            name: "case-web".into(),
        });
        for remote in [
            "https://github.com/digdir/case-web.git",
            "https://github.com/Digdir/case-web",
            "https://github.com/digdir/case-web/",
            "ssh://git@github.com/digdir/case-web.git",
            "git@github.com:digdir/case-web.git",
            "git@github.com:digdir/case-web",
        ] {
            assert_eq!(Repository::from_remote(remote), expected, "{remote}");
        }
        assert_eq!(
            expected.map(|repository| repository.to_string()).as_deref(),
            Some("digdir/case-web")
        );
        for remote in [
            "https://gitlab.com/digdir/case-web.git",
            "git@gitlab.com:digdir/case-web.git",
            "https://github.com.evil.example/digdir/case-web",
            "https://x-access-token:ghp_FAKE@github.com/digdir/case-web.git",
            "https://github.com/digdir",
            "https://github.com/digdir/case-web/tree/main",
            "https://github.com/-digdir/case-web",
            "https://github.com/digdir/..",
            "https://github.com/digdir/a%2fb",
            "/srv/git/case-web.git",
            "",
        ] {
            assert_eq!(Repository::from_remote(remote), None, "{remote}");
        }
    }

    #[tokio::test(flavor = "local")]
    async fn whether_a_token_sees_a_repository_is_asked_of_github_with_it() {
        let repository = Repository::from_remote("git@github.com:digdir/case-web.git").expect("repository");
        let (api, requests) = serve(BOT).await;
        assert!(
            inspect::can_see_at(&api, "github_pat_FAKE_canary", &repository)
                .await
                .expect("visible")
        );
        let request = requests.borrow()[0].to_ascii_lowercase();
        assert!(request.starts_with("get /repos/digdir/case-web "), "{request}");
        assert!(
            request.contains("authorization: bearer github_pat_fake_canary"),
            "{request}"
        );

        for status in ["404 Not Found", "403 Forbidden"] {
            let (api, _) = serve(Answer { status, ..BOT }).await;
            assert!(
                !inspect::can_see_at(&api, "github_pat_FAKE_canary", &repository)
                    .await
                    .expect("answered")
            );
        }
        let (api, _) = serve(Answer {
            status: "500 Internal Server Error",
            ..BOT
        })
        .await;
        let error = inspect::can_see_at(&api, "github_pat_FAKE_canary", &repository)
            .await
            .expect_err("unexpected answer");
        let message = format!("{error} {error:?}");
        assert!(message.contains("500"), "{message}");
        assert!(!message.contains("FAKE_canary"), "{message}");
    }
}
