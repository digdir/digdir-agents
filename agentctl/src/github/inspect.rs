//! Asking GitHub what a token is: whose it is, when it expires, and what it can see.

use serde::{Deserialize, Serialize};

use super::{GithubAccount, Repository, TokenKind};
use crate::Error;

pub(super) const API: &str = "https://api.github.com";
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Classic and OAuth scopes matching the fine-grained `PERMISSIONS`.
///
/// `repo` covers code, issues, pull requests and Actions, and `workflow` changing workflow files.
/// Without `read:org` and `read:project`, some `gh` commands, such as `gh pr edit`, fail.
pub const SCOPES: [&str; 4] = ["repo", "workflow", "read:org", "read:project"];

/// What GitHub reports about a token, without the token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    /// The account the token acts as.
    pub account: GithubAccount,
    /// What kind of token it is, from its prefix.
    pub kind: TokenKind,
    /// When GitHub says the token expires; `None` when it does not, or did not say.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub expires_at: Option<time::OffsetDateTime>,
    /// Recommended [`SCOPES`] a classic or OAuth token lacks. Fine-grained tokens report none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_scopes: Vec<String>,
    /// Whether the token can see the repository asked about, if one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<(Repository, bool)>,
}

/// Asks GitHub which account `token` belongs to, when it expires and, when `repository` is
/// given, whether it can see that repository.
///
/// # Errors
///
/// Returns an error when GitHub rejects the token, cannot be reached, or answers unexpectedly.
pub async fn inspect(token: &str, repository: Option<&Repository>) -> Result<Inspection, Error> {
    inspect_at(API, token, repository).await
}

pub(super) async fn inspect_at(api: &str, token: &str, repository: Option<&Repository>) -> Result<Inspection, Error> {
    let client = reqwest::Client::new();
    let response = get(&client, &format!("{api}/user"), token).await?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::Invalid("GitHub rejected the token".into()));
    }
    if !status.is_success() {
        return Err(Error::Invalid(format!(
            "GitHub answered {status} when asked which account the token belongs to"
        )));
    }
    let missing_scopes = header(&response, "x-oauth-scopes")
        .map(missing_scopes)
        .unwrap_or_default();
    let expires_at = header(&response, "github-authentication-token-expiration").and_then(parse_expiry);
    let user: User = response
        .json()
        .await
        .map_err(|error| Error::Invalid(format!("GitHub returned an unexpected user profile: {error}")))?;
    let repository = match repository {
        Some(repository) => {
            let response = get(
                &client,
                &format!("{api}/repos/{}/{}", repository.owner, repository.name),
                token,
            )
            .await?;
            let visible = match response.status() {
                status if status.is_success() => true,
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::FORBIDDEN => false,
                status => {
                    return Err(Error::Invalid(format!(
                        "GitHub answered {status} when asked whether the token can see {repository}"
                    )));
                }
            };
            Some((repository.clone(), visible))
        }
        None => None,
    };
    Ok(Inspection {
        account: user.into_account(),
        kind: TokenKind::of(token),
        expires_at,
        missing_scopes,
        repository,
    })
}

/// What kind of GitHub account owns repositories under a name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerKind {
    /// An organization.
    Organization,
    /// A personal account.
    User,
}

/// Which kind of account GitHub has under `owner`, asked with `token`, or `None` when there is
/// none.
///
/// # Errors
///
/// Returns an error when GitHub cannot be reached or answers unexpectedly.
pub async fn owner_kind(token: &str, owner: &str) -> Result<Option<OwnerKind>, Error> {
    owner_kind_at(API, token, owner).await
}

async fn owner_kind_at(api: &str, token: &str, owner: &str) -> Result<Option<OwnerKind>, Error> {
    #[derive(Deserialize)]
    struct Account {
        #[serde(rename = "type")]
        kind: String,
    }

    let response = get(&reqwest::Client::new(), &format!("{api}/users/{owner}"), token).await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(Error::Invalid(format!(
            "GitHub answered {status} when asked about {owner}"
        )));
    }
    let account: Account = response
        .json()
        .await
        .map_err(|error| Error::Invalid(format!("GitHub returned an unexpected account for {owner}: {error}")))?;
    Ok(Some(if account.kind == "Organization" {
        OwnerKind::Organization
    } else {
        OwnerKind::User
    }))
}

async fn get(client: &reqwest::Client, url: &str, token: &str) -> Result<reqwest::Response, Error> {
    client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("agentctl/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| Error::Invalid(format!("could not reach GitHub to check the token: {error}")))
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

#[derive(Deserialize)]
struct User {
    login: String,
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl User {
    fn into_account(self) -> GithubAccount {
        let name = self
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.login.clone());
        let email = self
            .email
            .filter(|email| !email.trim().is_empty())
            .unwrap_or_else(|| format!("{}+{}@users.noreply.github.com", self.id, self.login));
        GithubAccount {
            login: self.login,
            name,
            email,
        }
    }
}

/// Parses GitHub's `GitHub-Authentication-Token-Expiration`, such as `2027-01-13 10:00:00 UTC`
/// or `2027-01-13 10:00:00 +0100`. An unrecognized value counts as unknown, not as an error.
fn parse_expiry(value: &str) -> Option<time::OffsetDateTime> {
    let value = value.trim();
    let utc =
        time::format_description::parse_borrowed::<1>("[year]-[month]-[day] [hour]:[minute]:[second] UTC").ok()?;
    if let Ok(expiry) = time::PrimitiveDateTime::parse(value, &utc) {
        return Some(expiry.assume_utc());
    }
    let offset = time::format_description::parse_borrowed::<1>(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]",
    )
    .ok()?;
    time::OffsetDateTime::parse(value, &offset).ok()
}

/// The recommended [`SCOPES`] that `granted`, GitHub's comma-separated `X-OAuth-Scopes`, lacks.
/// A broader scope covers its narrower forms: `project` covers `read:project`, and `admin:org`
/// or `write:org` cover `read:org`.
fn missing_scopes(granted: &str) -> Vec<String> {
    let granted: Vec<&str> = granted
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .collect();
    SCOPES
        .iter()
        .filter(|required| {
            !granted.iter().any(|scope| {
                scope == *required
                    || required.strip_prefix("read:").is_some_and(|resource| {
                        *scope == resource
                            || *scope == format!("write:{resource}")
                            || *scope == format!("admin:{resource}")
                    })
            })
        })
        .map(|scope| (*scope).to_owned())
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(clippy::expect_used)]

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// One canned answer: the request path it is for, the status line, extra headers each
    /// ending in CRLF, and the body.
    pub(in crate::github) struct Answer {
        pub path: &'static str,
        pub status: &'static str,
        pub headers: &'static str,
        pub body: &'static str,
    }

    /// Serves `answers` by request path until the test ends, returning the API base URL and
    /// every request received.
    pub(in crate::github) async fn serve(
        answers: Vec<Answer>,
    ) -> (String, std::rc::Rc<std::cell::RefCell<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind API endpoint");
        let api = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = requests.clone();
        tokio::task::spawn_local(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut chunk = [0_u8; 4_096];
                let read = stream.read(&mut chunk).await.unwrap_or_default();
                let request = String::from_utf8_lossy(&chunk[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or_default().to_owned();
                seen.borrow_mut().push(request);
                let answer = answers.iter().find(|answer| answer.path == path);
                let (status, headers, body) = answer.map_or(("404 Not Found", "", "{}"), |answer| {
                    (answer.status, answer.headers, answer.body)
                });
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

    pub(in crate::github) const BOT: &str =
        r#"{"login":"digdir-bot","id":42,"name":"Digdir Bot","email":"bot@example.com"}"#;

    #[tokio::test(flavor = "local")]
    async fn reports_the_account_its_expiry_and_what_it_can_see() {
        let (api, requests) = serve(vec![
            Answer {
                path: "/user",
                status: "200 OK",
                headers: "GitHub-Authentication-Token-Expiration: 2027-01-13 10:00:00 UTC\r\n",
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
        let repository = Repository::new("digdir", "agents");

        let inspection = inspect_at(&api, "github_pat_canary", Some(&repository))
            .await
            .expect("inspection");

        assert_eq!(inspection.account.login, "digdir-bot");
        assert_eq!(inspection.kind, TokenKind::FineGrained);
        assert_eq!(
            inspection.expires_at,
            time::OffsetDateTime::from_unix_timestamp(1_799_834_400).ok()
        );
        assert_eq!(inspection.repository, Some((repository, true)));
        assert!(inspection.missing_scopes.is_empty());
        assert!(
            requests.borrow()[0].contains("authorization: Bearer github_pat_canary"),
            "{:?}",
            requests.borrow()
        );
    }

    #[tokio::test(flavor = "local")]
    async fn an_unselected_repository_and_an_unknown_expiry_are_reported_not_refused() {
        let (api, _requests) = serve(vec![Answer {
            path: "/user",
            status: "200 OK",
            headers: "",
            body: r#"{"login":"digdir-bot","id":42,"name":null,"email":null}"#,
        }])
        .await;

        let inspection = inspect_at(&api, "github_pat_x", Some(&Repository::new("digdir", "secret")))
            .await
            .expect("inspection");

        assert_eq!(inspection.repository.map(|(_, visible)| visible), Some(false));
        assert_eq!(inspection.expires_at, None);
        assert_eq!(inspection.account.name, "digdir-bot");
        assert_eq!(inspection.account.email, "42+digdir-bot@users.noreply.github.com");
    }

    #[tokio::test(flavor = "local")]
    async fn an_owner_is_an_organization_a_user_or_missing() {
        let (api, _requests) = serve(vec![
            Answer {
                path: "/users/digdir",
                status: "200 OK",
                headers: "",
                body: r#"{"login":"digdir","type":"Organization"}"#,
            },
            Answer {
                path: "/users/someone",
                status: "200 OK",
                headers: "",
                body: r#"{"login":"someone","type":"User"}"#,
            },
            Answer {
                path: "/users/gone",
                status: "500 Internal Server Error",
                headers: "",
                body: "{}",
            },
        ])
        .await;
        let kind = |owner: &'static str| owner_kind_at(&api, "github_pat_x", owner);
        assert_eq!(kind("digdir").await.expect("found"), Some(OwnerKind::Organization));
        assert_eq!(kind("someone").await.expect("found"), Some(OwnerKind::User));
        assert_eq!(kind("doesntexist").await.expect("not found"), None);
        kind("gone").await.expect_err("an unexpected answer");
    }

    #[tokio::test(flavor = "local")]
    async fn a_classic_token_reports_the_scopes_it_lacks() {
        let (api, _requests) = serve(vec![Answer {
            path: "/user",
            status: "200 OK",
            headers: "X-OAuth-Scopes: repo, gist, admin:org, project\r\n",
            body: BOT,
        }])
        .await;

        let inspection = inspect_at(&api, "ghp_narrow", None).await.expect("inspection");

        assert_eq!(inspection.kind, TokenKind::Classic);
        assert_eq!(inspection.missing_scopes, ["workflow"]);
    }

    #[tokio::test(flavor = "local")]
    async fn a_rejected_or_unreachable_token_is_an_error_without_the_token() {
        let (api, _requests) = serve(vec![Answer {
            path: "/user",
            status: "401 Unauthorized",
            headers: "",
            body: r#"{"message":"Bad credentials"}"#,
        }])
        .await;
        let error = inspect_at(&api, "ghp_rejected", None).await.expect_err("rejected");
        assert!(!error.to_string().contains("ghp_rejected"), "{error}");
        inspect_at("http://127.0.0.1:1", "ghp_unreachable", None)
            .await
            .expect_err("unreachable");
    }

    #[test]
    fn broader_scopes_cover_their_read_forms() {
        assert!(missing_scopes("repo,workflow,read:org,read:project").is_empty());
        assert!(missing_scopes("repo, write:org, gist, workflow, project").is_empty());
        assert_eq!(missing_scopes(""), SCOPES);
        assert_eq!(missing_scopes("public_repo"), SCOPES);
    }

    #[test]
    fn expiry_headers_parse_in_utc_or_with_an_offset() {
        assert_eq!(
            parse_expiry("2027-01-13 10:00:00 UTC"),
            time::OffsetDateTime::from_unix_timestamp(1_799_834_400).ok()
        );
        assert_eq!(
            parse_expiry("2027-01-13 11:00:00 +0100"),
            time::OffsetDateTime::from_unix_timestamp(1_799_834_400).ok()
        );
        assert_eq!(parse_expiry("next tuesday"), None);
    }
}
