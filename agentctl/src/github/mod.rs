//! GitHub accounts for Agents.
//!
//! agentd keeps one GitHub token per owner, the user or organization whose repositories Agents
//! work on, separate from any `gh` login on the host. Before keeping a token it asks GitHub which
//! account the token acts as and when it expires. The token stays in the host's secret store;
//! what is listed and returned about it never contains it.

mod inspect;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use inspect::SCOPES;

use crate::{Error, harness::MediatedSecret, persistence};
use sandbox::secret_store::SecretReference;

/// Provider account namespace; each owner's account is `github/<owner>`.
const PROVIDER: &str = "github";
/// Secret name prefix; each owner's token is `github-access-token/<owner>`.
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

/// A stored GitHub token, without the token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubToken {
    /// The owner the token is kept for, lowercased.
    pub owner: String,
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
}

/// A token just stored, and the recommended [`SCOPES`] it lacks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubLogin {
    /// The stored token.
    pub token: GithubToken,
    /// Recommended [`SCOPES`] a classic or OAuth token lacks; fine-grained tokens report none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_scopes: Vec<String>,
}

/// The provider account and secret names of `owner`'s token. `owner` must be valid.
fn names(owner: &str) -> (String, String) {
    (format!("{PROVIDER}/{owner}"), format!("{ACCESS_SECRET}/{owner}"))
}

/// The mediated `GITHUB_TOKEN` an Agent of `owner` receives, bound to that owner's token only,
/// and the account the token acts as.
///
/// # Errors
///
/// Returns an error naming `agentctl github login --owner <owner>` when no token is stored for
/// the owner or it has expired at `now`, and when the database cannot be read.
pub(crate) async fn prepare(
    database: &persistence::Database,
    owner: &str,
    now: time::OffsetDateTime,
) -> Result<(MediatedSecret, GithubAccount), Error> {
    let owner = owner.to_ascii_lowercase();
    if !valid_owner(&owner) {
        return Err(Error::Invalid(
            "the GitHub account's owner is not a GitHub login".into(),
        ));
    }
    let login = format!("run `agentctl github login --owner {owner}`");
    let (provider, secret) = names(&owner);
    let Some(metadata) = database.provider_account_metadata(&provider).await? else {
        return Err(Error::Invalid(format!(
            "no GitHub token is stored for {owner}; {login}"
        )));
    };
    let token: GithubToken = serde_json::from_str(&metadata)?;
    if let Some(expires_at) = token.expires_at.filter(|_| token.expired(now)) {
        return Err(Error::Invalid(format!(
            "the GitHub token for {owner} expired on {}; {login}",
            expires_at.date()
        )));
    }
    let secret = MediatedSecret {
        environment: ACCESS_ENVIRONMENT,
        placeholder: ACCESS_PLACEHOLDER.into(),
        reference: SecretReference::from_opaque(secret),
        allowed_hosts: MEDIATED_HOSTS.iter().map(|host| (*host).to_owned()).collect(),
    };
    Ok((secret, token.account))
}

/// Whether a manifest secret would collide with the binding the GitHub account owns. Mediation
/// needs placeholders that never contain one another, so overlapping ones are refused.
pub(crate) fn conflicts_with_managed_secret(name: &str, placeholder: &str) -> bool {
    name == ACCESS_ENVIRONMENT || placeholder.contains(ACCESS_PLACEHOLDER) || ACCESS_PLACEHOLDER.contains(placeholder)
}

/// Checks GitHub tokens with GitHub and keeps them per owner.
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

    /// Asks GitHub about `token`, then keeps it for `owner`, replacing the owner's earlier token.
    ///
    /// # Errors
    ///
    /// Returns an error when the token is empty, the owner is not a GitHub login, GitHub rejects
    /// the token or cannot be reached, the token has expired, or it cannot be persisted. Nothing
    /// is stored then, and no error contains the token.
    pub(crate) async fn login(&self, token: Zeroizing<String>, owner: &str) -> Result<GithubLogin, Error> {
        let token = Zeroizing::new(token.trim().to_owned());
        if token.is_empty() {
            return Err(Error::Invalid("no GitHub token was provided".into()));
        }
        // The owner is not repeated: a token pasted in its place must not end up in the error.
        if !valid_owner(owner) {
            return Err(Error::Invalid(
                "the owner must be a GitHub user or organization name: letters, digits and inner hyphens, \
                 at most 39 characters"
                    .into(),
            ));
        }
        let owner = owner.to_ascii_lowercase();
        let inspection = inspect::inspect_at(&self.api, &token).await?;
        let stored = GithubToken {
            owner,
            account: inspection.account,
            kind: TokenKind::of(&token),
            expires_at: inspection.expires_at,
        };
        if stored.expired(time::OffsetDateTime::now_utc()) {
            return Err(Error::Invalid("the GitHub token has already expired".into()));
        }
        let (provider, secret) = names(&stored.owner);
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
        Ok(GithubLogin {
            token: stored,
            missing_scopes: inspection.missing_scopes,
        })
    }

    /// Every stored GitHub token, by owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or holds an unreadable token record.
    pub(crate) async fn tokens(&self) -> Result<Vec<GithubToken>, Error> {
        self.database
            .provider_accounts(PROVIDER)
            .await?
            .into_iter()
            .map(|(_, metadata)| serde_json::from_str(&metadata).map_err(Error::from))
            .collect()
    }

    /// Forgets the token kept for `owner`, returning it, or `None` when there was none. The
    /// token stays valid on GitHub until it expires or is revoked there.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or written.
    pub(crate) async fn forget(&self, owner: &str) -> Result<Option<GithubToken>, Error> {
        if !valid_owner(owner) {
            return Ok(None);
        }
        let (provider, secret) = names(&owner.to_ascii_lowercase());
        let Some(metadata) = self.database.provider_account_metadata(&provider).await? else {
            return Ok(None);
        };
        self.database.delete_provider_account(&provider, vec![secret]).await?;
        Ok(Some(serde_json::from_str(&metadata)?))
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
        async fn login(&self, token: &str, owner: &str) -> Result<GithubLogin, Error> {
            self.github.login(Zeroizing::new(token.into()), owner).await
        }

        async fn stored(&self, owner: &str) -> Option<Vec<u8>> {
            let (_, secret) = names(owner);
            self.database
                .resolve(&SecretReference::from_opaque(secret))
                .await
                .ok()
                .map(|value| value.expose().to_vec())
        }

        async fn owners(&self) -> Vec<String> {
            let tokens = self.github.tokens().await.expect("tokens");
            tokens.into_iter().map(|token| token.owner).collect()
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

        assert_eq!(login.token.owner, "digdir");
        assert_eq!(login.token.kind, TokenKind::FineGrained);
        assert_eq!(login.token.account.login, "digdir-bot");
        assert_eq!(github.owners().await, ["altinn", "digdir"]);
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
        assert_eq!(github.owners().await, ["altinn", "digdir"]);
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
            assert_eq!(github.owners().await, Vec::<String>::new());
            assert_eq!(github.stored("digdir").await, None);
        }
        let github = fixture(BOT).await;
        errors.push(
            github
                .login(TOKEN, TOKEN)
                .await
                .expect_err("a token pasted as the owner"),
        );
        errors.push(github.login("  \n", "digdir").await.expect_err("no token"));

        let expected = [
            "could not reach GitHub",
            "rejected the token",
            "answered 500",
            "unexpected user profile",
            "already expired",
            "must be a GitHub user or organization",
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
    async fn an_expired_login_keeps_the_owners_earlier_token() {
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
    async fn owners_are_github_logins_checked_before_the_token_is_sent() {
        for owner in ["digdir", "Digdir", "a", "a-b", "x2", &"a".repeat(39)] {
            assert!(valid_owner(owner), "{owner}");
        }
        let long = "a".repeat(40);
        let invalid = [
            "", "../x", "..", "a/b", "-a", "a-", "a_b", "a.b", "a b", "a%2fb", "æ", &long,
        ];
        for owner in invalid {
            assert!(!valid_owner(owner), "{owner}");
        }

        let github = fixture(BOT).await;
        for owner in invalid {
            github.login("ghp_FAKE", owner).await.expect_err("invalid owner");
            assert_eq!(github.github.forget(owner).await.expect("forget"), None);
        }
        assert!(github.requests.borrow().is_empty(), "the token went to GitHub");
        assert_eq!(github.owners().await, Vec::<String>::new());
    }

    #[tokio::test(flavor = "local")]
    async fn logging_out_deletes_the_owners_token_itself() {
        let github = fixture(BOT).await;
        github.login("github_pat_FAKE_digdir", "digdir").await.expect("digdir");
        github.login("github_pat_FAKE_altinn", "altinn").await.expect("altinn");

        let forgotten = github.github.forget("DigDir").await.expect("forget");

        assert_eq!(forgotten.map(|token| token.owner).as_deref(), Some("digdir"));
        assert_eq!(github.stored("digdir").await, None, "the secret row is gone");
        assert_eq!(github.owners().await, ["altinn"]);
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

    /// Stores `token` for `owner` as a login would, expiring at `expires_at`.
    async fn store(
        database: &persistence::Database,
        owner: &str,
        token: &str,
        expires_at: Option<time::OffsetDateTime>,
    ) {
        let (provider, secret) = names(owner);
        let stored = GithubToken {
            owner: owner.into(),
            account: GithubAccount {
                login: format!("{owner}-bot"),
                name: format!("{owner} bot"),
                email: format!("1+{owner}-bot@users.noreply.github.com"),
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
    async fn an_agent_resolves_only_its_own_owners_token_for_githubs_hosts() {
        let github = fixture(BOT).await;
        store(&github.database, "digdir", "github_pat_FAKE_digdir", None).await;
        store(&github.database, "altinn", "github_pat_FAKE_altinn", None).await;
        let now = time::OffsetDateTime::now_utc();

        for (owner, expected) in [
            ("Digdir", "github_pat_FAKE_digdir"),
            ("altinn", "github_pat_FAKE_altinn"),
        ] {
            let (secret, account) = prepare(&github.database, owner, now).await.expect("prepare");
            let resolved = github.database.resolve(&secret.reference).await.expect("resolve");
            assert_eq!(resolved.expose(), expected.as_bytes(), "{owner}");
            assert_eq!(account.login, format!("{}-bot", owner.to_ascii_lowercase()));
            assert_eq!(secret.environment, "GITHUB_TOKEN");
            assert_eq!(secret.placeholder, ACCESS_PLACEHOLDER);
            assert_eq!(
                secret.allowed_hosts,
                ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"]
            );
        }

        let Err(error) = prepare(&github.database, "digdir-other", now).await else {
            panic!("no token for this owner");
        };
        let error = error.to_string();
        assert!(
            error.contains(
                "no GitHub token is stored for digdir-other; run `agentctl github login --owner digdir-other`"
            ),
            "{error}"
        );
        assert!(!error.contains("FAKE"), "{error}");
    }

    #[tokio::test(flavor = "local")]
    async fn an_expired_token_fails_naming_the_login_and_never_the_token() {
        let github = fixture(BOT).await;
        // 2027-01-13 10:00 UTC.
        let expires_at = time::OffsetDateTime::from_unix_timestamp(1_799_834_400).expect("timestamp");
        store(&github.database, "digdir", "github_pat_FAKE_canary", Some(expires_at)).await;

        prepare(&github.database, "digdir", expires_at - time::Duration::seconds(1))
            .await
            .expect("not yet expired");
        let Err(error) = prepare(&github.database, "digdir", expires_at).await else {
            panic!("expired");
        };

        assert!(matches!(error, Error::Invalid(_)), "a permanent failure: {error:?}");
        let message = error.to_string();
        assert!(
            message.contains(
                "the GitHub token for digdir expired on 2027-01-13; run `agentctl github login --owner digdir`"
            ),
            "{message}"
        );
        assert!(!message.contains("FAKE_canary"), "{message}");
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
}
