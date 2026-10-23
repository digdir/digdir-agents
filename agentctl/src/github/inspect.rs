//! Asking GitHub what a token is: whose it is, when it expires and which scopes it lacks.

use serde::Deserialize;

use super::GithubAccount;
use crate::Error;

pub(super) const API: &str = "https://api.github.com";
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Scopes a classic or OAuth token needs for what Agents do with `gh` and Git.
///
/// `repo` covers code, issues, pull requests and Actions, and `workflow` changing workflow files.
/// Without `read:org` and `read:project`, some `gh` commands, such as `gh pr edit`, fail.
pub const SCOPES: [&str; 4] = ["repo", "workflow", "read:org", "read:project"];

/// What GitHub reports about a token, without the token.
pub(super) struct Inspection {
    pub(super) account: GithubAccount,
    pub(super) expires_at: Option<time::OffsetDateTime>,
    pub(super) missing_scopes: Vec<String>,
}

/// Asks GitHub at `api` which account `token` acts as, when it expires and which [`SCOPES`] it
/// lacks.
///
/// Errors never contain the token: they name the status, or the request that failed, which
/// carries the token only in a header.
pub(super) async fn inspect_at(api: &str, token: &str) -> Result<Inspection, Error> {
    let response = request(&format!("{api}/user"), token)
        .await
        .map_err(|error| Error::Invalid(format!("could not reach GitHub to check the token: {error}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::Invalid("GitHub rejected the token".into()));
    }
    if !status.is_success() {
        return Err(Error::Invalid(format!(
            "GitHub answered {status} when asked which account the token belongs to"
        )));
    }
    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
    let missing_scopes = header("x-oauth-scopes").map(missing_scopes).unwrap_or_default();
    let expires_at = header("github-authentication-token-expiration").and_then(parse_expiry);
    // The parser's message would quote the profile, which is not ours to trust.
    let user: User = response
        .json()
        .await
        .map_err(|_| Error::Invalid("GitHub returned an unexpected user profile".into()))?;
    Ok(Inspection {
        account: user.into_account(),
        expires_at,
        missing_scopes,
    })
}

/// Whether `token` can see `repository`, asking GitHub at `api`. GitHub answers an unselected or
/// unapproved private repository as missing.
///
/// # Errors
///
/// Returns an error, never containing the token, when GitHub cannot be reached or answers
/// anything but success, not found or forbidden.
pub(super) async fn can_see_at(api: &str, token: &str, repository: &super::Repository) -> Result<bool, Error> {
    let response = request(&format!("{api}/repos/{repository}"), token)
        .await
        .map_err(|error| Error::Invalid(format!("could not reach GitHub to check the token: {error}")))?;
    match response.status() {
        status if status.is_success() => Ok(true),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::FORBIDDEN => Ok(false),
        status => Err(Error::Invalid(format!(
            "GitHub answered {status} when asked whether the token can see {repository}"
        ))),
    }
}

async fn request(url: &str, token: &str) -> reqwest::Result<reqwest::Response> {
    reqwest::Client::new()
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

/// The [`SCOPES`] that `granted`, GitHub's comma-separated `X-OAuth-Scopes`, lacks. A broader
/// scope covers its read form: `project` covers `read:project`, `write:org` or `admin:org` cover
/// `read:org`.
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
mod tests {
    use super::*;

    #[test]
    fn broader_scopes_cover_their_read_forms() {
        assert_eq!(
            missing_scopes("repo,workflow,read:org,read:project"),
            Vec::<String>::new()
        );
        assert_eq!(
            missing_scopes("repo, write:org, gist, workflow, project"),
            Vec::<String>::new()
        );
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
