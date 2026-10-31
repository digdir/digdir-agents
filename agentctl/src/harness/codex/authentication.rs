//! Codex CLI host-side `ChatGPT` subscription authentication.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc, time::Instant, time::SystemTime};

use base64::Engine as _;
use sandbox::secret_store::{SecretMaterial, SecretStore as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    Error,
    harness::{Harness, ImportedAuthentication, LoginName, login_command},
    persistence,
};

use super::{ACCESS_PLACEHOLDER, ACCESS_SECRET, ACCOUNT_SECRET, LOGINS, PROVIDER, REFRESH_PLACEHOLDER, REFRESH_SECRET};

const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_AHEAD_SECONDS: i64 = 5 * 60;
const TRANSIENT_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Owns the `ChatGPT` OAuth grants used only by the agent stack, one per named login.
pub(in crate::harness) struct Authentication {
    database: persistence::Database,
    client: reqwest::Client,
    refresh_url: String,
    /// Kept for every login used since start, never removed, so each login has exactly one lock.
    refreshes: RefCell<BTreeMap<LoginName, Rc<Refresh>>>,
}

/// One login's refresh state. Each grant's refresh token rotates on use, so its refreshes must
/// not overlap, while another login's need not wait for them.
#[derive(Default)]
struct Refresh {
    lock: tokio::sync::Mutex<()>,
    failure: RefCell<Option<CachedRefreshFailure>>,
}

impl Authentication {
    #[must_use]
    pub(in crate::harness) fn new(database: persistence::Database) -> Self {
        Self {
            database,
            client: reqwest::Client::new(),
            refresh_url: REFRESH_URL.into(),
            refreshes: RefCell::default(),
        }
    }

    fn refresh_state(&self, login: &LoginName) -> Rc<Refresh> {
        Rc::clone(self.refreshes.borrow_mut().entry(login.clone()).or_default())
    }

    /// Imports the independent `ChatGPT` grant produced in agentctl's private Codex home.
    ///
    /// An `auth.json` carrying the platform's own placeholders is the credential an agent
    /// sandbox already holds. Importing it makes this `agentd` a nested one: the tokens are
    /// stored verbatim, never refreshed, and the enclosing sandbox's mediator substitutes the
    /// real grant. No real credential exists at this level. An `imported` credential must be
    /// such a placeholder file: importing a user's real `auth.json` would let this `agentd` and
    /// the user's own Codex CLI rotate the same refresh token against each other.
    pub(in crate::harness) async fn login(
        &self,
        login: LoginName,
        credential: Zeroizing<String>,
        imported: bool,
    ) -> Result<ImportedAuthentication, Error> {
        let source: LoginFile = serde_json::from_str(&credential)
            .map_err(|_| Error::Invalid("Codex login did not produce valid authentication data".into()))?;
        if source.auth_mode.as_deref() != Some("chatgpt") {
            return Err(Error::Invalid(
                "Codex login did not produce a ChatGPT subscription grant".into(),
            ));
        }
        let tokens = source
            .tokens
            .ok_or_else(|| Error::Invalid("Codex login did not produce ChatGPT tokens".into()))?;
        let access_token = Zeroizing::new(tokens.access_token.trim().to_owned());
        let refresh_token = Zeroizing::new(tokens.refresh_token.trim().to_owned());
        let account_id = tokens
            .account_id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Error::Invalid("Codex login did not identify a ChatGPT account".into()))?;
        if access_token.is_empty() || refresh_token.is_empty() {
            return Err(Error::Invalid("Codex login produced incomplete ChatGPT tokens".into()));
        }
        let kind = if *refresh_token == REFRESH_PLACEHOLDER && *access_token == ACCESS_PLACEHOLDER {
            CredentialKind::Mediated
        } else if imported {
            return Err(Error::Invalid(
                "only an agent's mediated Codex credential file can be imported; run `agentctl codex login` on the host to sign in".into(),
            ));
        } else {
            CredentialKind::ChatgptOauth
        };
        let metadata = CodexMetadata {
            kind,
            account_id,
            expires_at: jwt_expiry(&access_token)?,
        };
        // A refresh in flight would store the grant this login replaces after it.
        let state = self.refresh_state(&login);
        let _refresh = state.lock.lock().await;
        self.store(&login, &access_token, &refresh_token, &metadata).await?;
        state.failure.borrow_mut().take();
        Ok(ImportedAuthentication {
            provider: PROVIDER.into(),
            login,
            ready: true,
        })
    }

    /// Forgets the login `login`, returning whether it was stored.
    pub(in crate::harness) async fn logout(&self, login: &LoginName) -> Result<bool, Error> {
        // A refresh in flight would store the grant again after it is forgotten. The state stays,
        // so callers already waiting for it and later ones keep sharing one lock.
        let state = self.refresh_state(login);
        let _refresh = state.lock.lock().await;
        let forgotten = LOGINS.delete(&self.database, login).await?;
        state.failure.borrow_mut().take();
        Ok(forgotten)
    }

    pub(in crate::harness) async fn resolve_access(&self, login: &LoginName) -> Result<SecretMaterial, sandbox::Error> {
        self.refresh_if_needed(login)
            .await
            .map_err(|error| sandbox::Error::Backend(error.to_string()))?;
        self.database.resolve(&LOGINS.secret(ACCESS_SECRET, login)).await
    }

    #[allow(clippy::option_if_let_else)]
    async fn refresh_if_needed(&self, login: &LoginName) -> Result<(), Error> {
        let state = self.refresh_state(login);
        let _refresh = state.lock.lock().await;
        let metadata = metadata(&self.database, login).await?;
        if matches!(metadata.kind, CredentialKind::Mediated)
            || metadata.expires_at > unix_time()?.saturating_add(REFRESH_AHEAD_SECONDS)
        {
            return Ok(());
        }
        if let Some(error) = cached_refresh_failure(&state) {
            return Err(error);
        }

        let result = self.refresh(login, &metadata).await;
        match &result {
            Ok(()) => {
                state.failure.borrow_mut().take();
            }
            Err(failure) => {
                let cached = match failure {
                    RefreshFailure::Permanent(message) => CachedRefreshFailure::Permanent(message.clone()),
                    RefreshFailure::Transient(message) => CachedRefreshFailure::Transient {
                        message: message.clone(),
                        retry_at: Instant::now() + TRANSIENT_FAILURE_COOLDOWN,
                    },
                };
                *state.failure.borrow_mut() = Some(cached);
            }
        }
        result.map_err(RefreshFailure::into_error)
    }

    async fn refresh(&self, login: &LoginName, metadata: &CodexMetadata) -> Result<(), RefreshFailure> {
        let again = login_command(Harness::Codex, login);
        let stored = self
            .database
            .resolve(&LOGINS.secret(REFRESH_SECRET, login))
            .await
            .map_err(|error| {
                RefreshFailure::permanent(format!(
                    "Codex refresh credential is unavailable: {error}; run {again} again"
                ))
            })?;
        let refresh_token = std::str::from_utf8(stored.expose()).map_err(|_| {
            RefreshFailure::permanent(format!("Codex refresh credential is invalid; run {again} again"))
        })?;
        let response = self
            .client
            .post(&self.refresh_url)
            .timeout(REQUEST_TIMEOUT)
            .json(&RefreshRequest {
                client_id: OAUTH_CLIENT_ID,
                grant_type: "refresh_token",
                refresh_token,
            })
            .send()
            .await
            .map_err(|_| RefreshFailure::transient("could not reach OpenAI to refresh Codex authentication"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let code = refresh_error_code(&body);
            let invalid_grant = status == reqwest::StatusCode::BAD_REQUEST
                && code
                    .as_deref()
                    .is_some_and(|code| code.eq_ignore_ascii_case("invalid_grant"));
            let permanent = status == reqwest::StatusCode::UNAUTHORIZED
                || invalid_grant
                || matches!(
                    code.as_deref(),
                    Some("refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated")
                );
            if permanent {
                let reason = match code.as_deref() {
                    Some("refresh_token_expired") => "the refresh token has expired",
                    Some("refresh_token_reused") => "the refresh token was already used",
                    Some("refresh_token_invalidated") => "the refresh token was revoked",
                    _ => "OpenAI rejected the refresh grant",
                };
                return Err(RefreshFailure::permanent(format!(
                    "Codex authentication can no longer be refreshed because {reason}; run {again} again"
                )));
            }
            return Err(RefreshFailure::transient(format!(
                "OpenAI temporarily failed to refresh Codex authentication (HTTP {status})"
            )));
        }
        let response: RefreshResponse = response
            .json()
            .await
            .map_err(|_| RefreshFailure::transient("OpenAI returned an invalid Codex refresh response"))?;
        let access_token = Zeroizing::new(response.access_token.trim().to_owned());
        if access_token.is_empty() {
            return Err(RefreshFailure::transient("OpenAI returned an empty Codex access token"));
        }
        let refresh_token = Zeroizing::new(
            response
                .refresh_token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(refresh_token)
                .to_owned(),
        );
        let replacement = CodexMetadata {
            kind: CredentialKind::ChatgptOauth,
            account_id: metadata.account_id.clone(),
            expires_at: jwt_expiry(&access_token)
                .map_err(|_| RefreshFailure::transient("OpenAI returned an invalid Codex access token"))?,
        };
        self.store(login, &access_token, &refresh_token, &replacement)
            .await
            .map_err(|_| RefreshFailure::transient("could not store refreshed Codex authentication"))
    }

    async fn store(
        &self,
        login: &LoginName,
        access_token: &str,
        refresh_token: &str,
        metadata: &CodexMetadata,
    ) -> Result<(), Error> {
        LOGINS
            .put(
                &self.database,
                login,
                vec![
                    (ACCESS_SECRET, Zeroizing::new(access_token.as_bytes().to_vec())),
                    (REFRESH_SECRET, Zeroizing::new(refresh_token.as_bytes().to_vec())),
                    (ACCOUNT_SECRET, Zeroizing::new(metadata.account_id.as_bytes().to_vec())),
                ],
                serde_json::to_string(metadata)?,
            )
            .await
    }

    #[cfg(test)]
    fn with_refresh_url(mut self, refresh_url: String) -> Self {
        self.refresh_url = refresh_url;
        self
    }
}

fn cached_refresh_failure(state: &Refresh) -> Option<Error> {
    let mut cached = state.failure.borrow_mut();
    match cached.as_ref() {
        Some(CachedRefreshFailure::Permanent(message)) => Some(Error::Invalid(message.clone())),
        Some(CachedRefreshFailure::Transient { message, retry_at }) if Instant::now() < *retry_at => {
            Some(Error::Invalid(message.clone()))
        }
        Some(CachedRefreshFailure::Transient { .. }) | None => {
            cached.take();
            None
        }
    }
}

async fn metadata(database: &persistence::Database, login: &LoginName) -> Result<CodexMetadata, Error> {
    let metadata = LOGINS
        .metadata(database, login)
        .await?
        .ok_or_else(|| crate::harness::login_missing(Harness::Codex, login))?;
    serde_json::from_str(&metadata).map_err(|_| {
        Error::Invalid(format!(
            "stored Codex authentication metadata is invalid; run {} again",
            login_command(Harness::Codex, login)
        ))
    })
}

pub(super) async fn selected_account_id(database: &persistence::Database, login: &LoginName) -> Result<String, Error> {
    Ok(metadata(database, login).await?.account_id)
}

#[derive(Clone)]
enum RefreshFailure {
    Permanent(String),
    Transient(String),
}

impl RefreshFailure {
    fn permanent(message: impl Into<String>) -> Self {
        Self::Permanent(message.into())
    }

    fn transient(message: impl Into<String>) -> Self {
        Self::Transient(message.into())
    }

    fn into_error(self) -> Error {
        Error::Invalid(match self {
            Self::Permanent(message) | Self::Transient(message) => message,
        })
    }
}

enum CachedRefreshFailure {
    Permanent(String),
    Transient { message: String, retry_at: Instant },
}

#[derive(Deserialize)]
struct LoginFile {
    auth_mode: Option<String>,
    tokens: Option<LoginTokens>,
}

#[derive(Deserialize)]
struct LoginTokens {
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum CredentialKind {
    /// A real `ChatGPT` OAuth grant, refreshed by this `agentd`.
    ChatgptOauth,
    /// Placeholders from an enclosing sandbox; an outer mediator holds the real grant.
    Mediated,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexMetadata {
    kind: CredentialKind,
    account_id: String,
    expires_at: i64,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
}

fn refresh_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let code = match value.get("error") {
        Some(serde_json::Value::String(code)) => Some(code.clone()),
        Some(serde_json::Value::Object(error)) => error.get("code")?.as_str().map(str::to_owned),
        _ => value.get("code")?.as_str().map(str::to_owned),
    }?;
    Some(code.to_ascii_lowercase())
}

fn jwt_expiry(token: &str) -> Result<i64, Error> {
    let encoded = token
        .split('.')
        .nth(1)
        .ok_or_else(|| Error::Invalid("Codex access token is not a JWT".into()))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Error::Invalid("Codex access token has an invalid JWT payload".into()))?;
    let claims: JwtClaims = serde_json::from_slice(&payload)
        .map_err(|_| Error::Invalid("Codex access token has invalid JWT claims".into()))?;
    Ok(claims.exp)
}

fn unix_time() -> Result<i64, Error> {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Error::Invalid("host clock is before the Unix epoch".into()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::Invalid("host clock cannot be represented".into()))
}

#[derive(Deserialize)]
struct JwtClaims {
    exp: i64,
}

pub(super) async fn is_ready(database: &persistence::Database, login: &LoginName) -> Result<bool, Error> {
    let Some(metadata) = LOGINS.metadata(database, login).await? else {
        return Ok(false);
    };
    if serde_json::from_str::<CodexMetadata>(&metadata).is_err() {
        return Ok(false);
    }
    for secret in [ACCESS_SECRET, REFRESH_SECRET, ACCOUNT_SECRET] {
        if database.resolve(&LOGINS.secret(secret, login)).await.is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{cell::Cell, rc::Rc};

    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    fn jwt(exp: i64) -> String {
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::json!({ "exp": exp }).to_string());
        format!("header.{payload}.signature")
    }

    fn login_file(access_token: &str, refresh_token: &str) -> Zeroizing<String> {
        Zeroizing::new(
            serde_json::json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": null,
                "tokens": {
                    "id_token": "header.payload.signature",
                    "access_token": access_token,
                    "refresh_token": refresh_token,
                    "account_id": "account-canary"
                },
                "last_refresh": "2026-08-24T00:00:00Z"
            })
            .to_string(),
        )
    }

    /// Reads one whole HTTP request from `stream`, so a test server answers only once it has all of
    /// it: one answering earlier can close the connection on a body still being sent.
    async fn read_test_request(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt as _;

        let mut received = Vec::new();
        let mut buffer = [0; 4_096];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            received.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&received).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                assert_ne!(read, 0, "the request ended before its head");
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>())
                })
                .and_then(Result::ok)
                .unwrap_or_default();
            if body.len() >= length || read == 0 {
                return text;
            }
        }
    }

    async fn serve_refresh(access_token: String, refresh_token: Option<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refresh endpoint");
        let endpoint = format!("http://{}/oauth/token", listener.local_addr().expect("local address"));
        tokio::task::spawn_local(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let request = read_test_request(&mut stream).await;
            assert!(request.contains("refresh-canary"));
            assert!(request.contains(OAUTH_CLIENT_ID));
            let body = serde_json::json!({
                "access_token": access_token,
                "refresh_token": refresh_token,
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response");
        });
        endpoint
    }

    async fn serve_refresh_failure(status: &str, body: &'static str) -> (String, Rc<Cell<usize>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refresh endpoint");
        let endpoint = format!("http://{}/oauth/token", listener.local_addr().expect("local address"));
        let requests = Rc::new(Cell::new(0));
        let observed = Rc::clone(&requests);
        let status = status.to_owned();
        tokio::task::spawn_local(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                read_test_request(&mut stream).await;
                observed.set(observed.get() + 1);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .expect("write response");
            }
        });
        (endpoint, requests)
    }

    #[tokio::test(flavor = "local")]
    async fn imports_only_the_required_chatgpt_grant_state() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let manager = Authentication::new(database.clone());
        let access_token = jwt(unix_time().expect("time") + 3_600);

        let imported = manager
            .login(LoginName::default(), login_file(&access_token, "refresh-canary"), false)
            .await
            .expect("login");

        assert_eq!(imported.provider, "codex");
        assert!(imported.ready);
        assert_eq!(
            database
                .resolve(&LOGINS.secret(ACCESS_SECRET, &LoginName::default()))
                .await
                .expect("access token")
                .expose(),
            access_token.as_bytes()
        );
        assert_eq!(
            database
                .resolve(&LOGINS.secret(REFRESH_SECRET, &LoginName::default()))
                .await
                .expect("refresh token")
                .expose(),
            b"refresh-canary"
        );
        assert_eq!(
            database
                .resolve(&LOGINS.secret(ACCOUNT_SECRET, &LoginName::default()))
                .await
                .expect("account ID")
                .expose(),
            b"account-canary"
        );
        assert!(is_ready(&database, &LoginName::default()).await.expect("readiness"));
    }

    #[tokio::test(flavor = "local")]
    async fn rejects_api_key_authentication_data() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let manager = Authentication::new(database.clone());

        let error = manager
            .login(
                LoginName::default(),
                Zeroizing::new(
                    serde_json::json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "secret-canary" }).to_string(),
                ),
                false,
            )
            .await
            .expect_err("reject API key");

        assert!(error.to_string().contains("ChatGPT subscription grant"));
        assert!(!error.to_string().contains("secret-canary"));
        assert!(!is_ready(&database, &LoginName::default()).await.expect("readiness"));
    }

    #[tokio::test(flavor = "local")]
    async fn placeholder_credentials_are_stored_verbatim_and_never_refreshed() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let (endpoint, refresh_calls) = serve_refresh_failure("500 Internal Server Error", "{}").await;
        let manager = Authentication::new(database.clone()).with_refresh_url(endpoint);
        let credential = Zeroizing::new(
            serde_json::json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": null,
                "tokens": {
                    "id_token": ACCESS_PLACEHOLDER,
                    "access_token": ACCESS_PLACEHOLDER,
                    "refresh_token": REFRESH_PLACEHOLDER,
                    "account_id": "account-test"
                },
                "last_refresh": "2026-08-24T00:00:00Z"
            })
            .to_string(),
        );
        manager
            .login(LoginName::default(), credential, true)
            .await
            .expect("placeholder login");

        let resolved = manager
            .resolve_access(&LoginName::default())
            .await
            .expect("placeholder access token");

        assert_eq!(resolved.expose(), ACCESS_PLACEHOLDER.as_bytes());
        assert_eq!(
            selected_account_id(&database, &LoginName::default())
                .await
                .expect("account ID"),
            "account-test"
        );
        assert_eq!(refresh_calls.get(), 0);
        assert!(is_ready(&database, &LoginName::default()).await.expect("readiness"));
    }

    #[tokio::test(flavor = "local")]
    async fn a_real_credential_file_cannot_be_imported() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let manager = Authentication::new(database.clone());
        let access = jwt(unix_time().expect("time") + 3_600);

        let error = manager
            .login(LoginName::default(), login_file(&access, "refresh-canary"), true)
            .await
            .expect_err("a real ChatGPT grant must not be imported");

        assert!(error.to_string().contains("agentctl codex login"));
        assert!(!error.to_string().contains("refresh-canary"));
        assert!(!is_ready(&database, &LoginName::default()).await.expect("readiness"));
    }

    #[tokio::test(flavor = "local")]
    async fn refreshes_and_rotates_before_resolving_the_access_token() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let old_access = jwt(unix_time().expect("time") - 1);
        let new_access = jwt(unix_time().expect("time") + 3_600);
        let endpoint = serve_refresh(new_access.clone(), Some("rotated-refresh")).await;
        let manager = Authentication::new(database.clone()).with_refresh_url(endpoint);
        manager
            .login(LoginName::default(), login_file(&old_access, "refresh-canary"), false)
            .await
            .expect("login");

        let login = LoginName::default();
        let (first, second) = tokio::join!(manager.resolve_access(&login), manager.resolve_access(&login));
        let first = first.expect("first resolved token");
        let second = second.expect("second resolved token");

        assert_eq!(first.expose(), new_access.as_bytes());
        assert_eq!(second.expose(), new_access.as_bytes());
        assert_eq!(
            database
                .resolve(&LOGINS.secret(REFRESH_SECRET, &LoginName::default()))
                .await
                .expect("rotated refresh token")
                .expose(),
            b"rotated-refresh"
        );
    }

    #[tokio::test(flavor = "local")]
    async fn each_login_refreshes_its_own_grant_and_a_logout_forgets_only_its_own() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let (endpoint, requests) =
            serve_refresh_failure("400 Bad Request", r#"{"error":{"code":"refresh_token_reused"}}"#).await;
        let manager = Authentication::new(database.clone()).with_refresh_url(endpoint);
        let (work, private) = (
            LoginName::new("work").expect("name"),
            LoginName::new("private").expect("name"),
        );
        let current = jwt(unix_time().expect("time") + 3_600);
        manager
            .login(
                work.clone(),
                login_file(&jwt(unix_time().expect("time") - 1), "refresh-canary"),
                false,
            )
            .await
            .expect("work login");
        manager
            .login(private.clone(), login_file(&current, "refresh-canary"), false)
            .await
            .expect("private login");

        let failed = manager.resolve_access(&work).await.expect_err("work cannot refresh");
        let resolved = manager
            .resolve_access(&private)
            .await
            .expect("private needs no refresh");

        assert!(failed.to_string().contains("agentctl codex login work"));
        assert_eq!(resolved.expose(), current.as_bytes());
        assert_eq!(requests.get(), 1);
        let state = manager.refresh_state(&work);
        assert!(manager.logout(&work).await.expect("logout"));
        assert!(
            Rc::ptr_eq(&state, &manager.refresh_state(&work)),
            "callers queued behind a logout and later ones share one lock"
        );
        assert!(!manager.logout(&work).await.expect("second logout"));
        assert!(!is_ready(&database, &work).await.expect("readiness"));
        assert!(is_ready(&database, &private).await.expect("readiness"));
        assert_eq!(LOGINS.logins(&database).await.expect("logins"), [private]);
    }

    #[tokio::test(flavor = "local")]
    async fn transient_refresh_failures_have_a_cooldown_without_login_guidance() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let expired_access = jwt(unix_time().expect("time") - 1);
        let (endpoint, requests) = serve_refresh_failure("502 Bad Gateway", r#"{"error":"upstream_error"}"#).await;
        let manager = Authentication::new(database).with_refresh_url(endpoint);
        manager
            .login(
                LoginName::default(),
                login_file(&expired_access, "refresh-canary"),
                false,
            )
            .await
            .expect("login");

        let first = manager
            .resolve_access(&LoginName::default())
            .await
            .expect_err("first refresh fails");
        let second = manager
            .resolve_access(&LoginName::default())
            .await
            .expect_err("cooldown retains failure");

        assert_eq!(requests.get(), 1);
        assert!(first.to_string().contains("temporarily failed"));
        assert!(!first.to_string().contains("codex login"));
        assert_eq!(first.to_string(), second.to_string());
    }

    #[tokio::test(flavor = "local")]
    async fn terminal_refresh_failures_require_login_and_are_not_retried() {
        let directory = TempDir::new().expect("temporary directory");
        let database = persistence::Database::open(&directory.path().join("agent.db")).expect("database");
        let expired_access = jwt(unix_time().expect("time") - 1);
        let (endpoint, requests) =
            serve_refresh_failure("400 Bad Request", r#"{"error":{"code":"refresh_token_reused"}}"#).await;
        let manager = Authentication::new(database).with_refresh_url(endpoint);
        manager
            .login(
                LoginName::default(),
                login_file(&expired_access, "refresh-canary"),
                false,
            )
            .await
            .expect("login");

        let first = manager
            .resolve_access(&LoginName::default())
            .await
            .expect_err("first refresh fails");
        let second = manager
            .resolve_access(&LoginName::default())
            .await
            .expect_err("terminal failure retained");

        assert_eq!(requests.get(), 1);
        assert!(first.to_string().contains("already used"));
        assert!(first.to_string().contains("agentctl codex login"));
        assert_eq!(first.to_string(), second.to_string());
    }
}
