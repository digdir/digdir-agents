//! `agentctl github`: the GitHub tokens Agents use, one per owner.

use std::fmt::Write as _;

use agent::{
    Error,
    control_api::Client,
    github::{GithubLogin, GithubToken, Repository},
};
use time::OffsetDateTime;

use crate::{CommandResult, prompt};

/// Has agentd keep a GitHub token for Agents of `owner`.
///
/// # Errors
///
/// Returns an error when there is no terminal to paste on without `from_stdin`, no owner is given
/// or found, no token is given, GitHub rejects it, or agentd cannot keep it.
pub(crate) async fn login(client: &Client, owner: Option<&str>, from_stdin: bool) -> CommandResult<()> {
    let owner = owner.map(github_owner).transpose()?;
    let repository = this_repository();
    if from_stdin {
        let owner = match (owner, &repository) {
            (Some(owner), _) => owner,
            (None, Some(repository)) => {
                eprintln!("Storing the token for {}, the owner of {repository}.", repository.owner);
                repository.owner.clone()
            }
            (None, None) => {
                return Err(Error::Invalid(
                    "no GitHub remote here to take the owner from; name it with `--owner OWNER`".into(),
                )
                .into());
            }
        };
        let token = crate::read_token_from_stdin()?;
        let stored = client.github_login(&token, &owner).await?;
        let now = OffsetDateTime::now_utc();
        eprint!("{}", summary(&stored, &warnings(&stored, None, false, now), now));
        println!("{}", stored_line(&stored));
        return Ok(());
    }
    if !prompt::interactive() {
        return Err(Error::Invalid(
            "no terminal to paste the token on; pipe it to `agentctl github login --owner OWNER --from-stdin`".into(),
        )
        .into());
    }
    Err(Error::Invalid("pipe the token to `agentctl github login --owner OWNER --from-stdin`".into()).into())
}

fn stored_line(stored: &GithubLogin) -> String {
    format!(
        "Agents for {} now use GitHub as {}.",
        stored.token.owner, stored.token.account.login
    )
}

/// The GitHub repository of this directory's Git remote: `origin` when it is on GitHub, else the
/// first remote that is. `None` outside a repository, or without a Git that runs unattended.
fn this_repository() -> Option<Repository> {
    let output = std::process::Command::new(git()?)
        .args(["remote", "-v"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| repository_from_remotes(&String::from_utf8_lossy(&output.stdout)))?
}

/// Git on `PATH`. On macOS only with the Command Line Tools: without them Apple's `git` is a stub
/// that opens an installer dialog instead of running, and Git elsewhere is rare enough to skip.
fn git() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH");
    let git = crate::launch::find_program(&["git"], path.as_deref())?;
    let tools = || {
        crate::launch::find_program(&["xcode-select"], path.as_deref()).is_some_and(|program| {
            std::process::Command::new(program)
                .arg("-p")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    };
    (!cfg!(target_os = "macos") || tools()).then_some(git)
}

/// The GitHub repository `git remote -v` lists: `origin`'s when it is on GitHub, else the first.
fn repository_from_remotes(remotes: &str) -> Option<Repository> {
    let found: Vec<(bool, Repository)> = remotes
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            Some((name == "origin", Repository::from_remote(fields.next()?)?))
        })
        .collect();
    found
        .iter()
        .find(|(origin, _)| *origin)
        .or_else(|| found.first())
        .map(|(_, repository)| repository.clone())
}

/// Lists the stored tokens: owner, account, kind and expiry.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached.
pub(crate) async fn list(client: &Client) -> CommandResult<()> {
    print!("{}", table(&client.github_accounts().await?, OffsetDateTime::now_utc()));
    Ok(())
}

/// Forgets the token kept for `owner`, and says where to revoke it on GitHub.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached or cannot update its database.
pub(crate) async fn logout(client: &Client, owner: &str) -> CommandResult<()> {
    let owner = &github_owner(owner)?;
    match client.github_logout(owner).await? {
        Some(token) => print!("{}", forgotten(&token)),
        None => println!("No GitHub token is stored for {owner}; nothing changed."),
    }
    Ok(())
}

/// `value` as a GitHub owner, lowercased. Clap would repeat a rejected value, which may be a
/// token pasted in the wrong place, so owners are checked here instead.
fn github_owner(value: &str) -> Result<String, Error> {
    if agent::github::valid_owner(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(Error::Invalid(agent::github::OWNER_RULE.into()))
    }
}

/// What to warn of about a token: that it cannot see this repository (`visible`), expires
/// within [`agent::github::EXPIRY_WARNING`], reaches every repository of its account unless it
/// came from signing in as that account (`browser`), or lacks recommended scopes.
fn warnings(
    login: &GithubLogin,
    visible: Option<(&Repository, bool)>,
    browser: bool,
    now: OffsetDateTime,
) -> Vec<String> {
    let token = &login.token;
    let account = &token.account.login;
    let mut warnings = Vec::new();
    if let Some((repository, false)) = visible {
        warnings.push(if !token.kind.single_owner() {
            format!("{account} cannot see {repository}; give the account access to it")
        } else if token.owner.eq_ignore_ascii_case(account) {
            format!("the token cannot see {repository}: it was probably not selected under Repository access")
        } else {
            format!(
                "the token cannot see {repository}: it was not selected, or {} has yet to approve the token",
                token.owner
            )
        });
    }
    if let Some(expires_at) = token
        .expires_at
        .filter(|expires_at| *expires_at <= now + agent::github::EXPIRY_WARNING)
    {
        warnings.push(format!("the token expires on {}, within a week", expires_at.date()));
    }
    if !token.kind.single_owner() && !browser {
        warnings.push(format!(
            "this {} token reaches every repository {account} can, not only {}'s",
            token.kind.label(),
            token.owner
        ));
    }
    if !login.missing_scopes.is_empty() {
        warnings.push(format!(
            "the token lacks {}, so some `gh` and Git commands will fail",
            login.missing_scopes.join(", ")
        ));
    }
    warnings
}

/// What GitHub said about a token: whom it acts as, its kind, owner and expiry, then `warnings`.
fn summary(login: &GithubLogin, warnings: &[String], now: OffsetDateTime) -> String {
    let token = &login.token;
    let account = &token.account;
    let mut text = format!(
        "\n  Acts as:  {} ({} <{}>)\n  Kind:     {}\n  For:      {}\n  Expires:  {}\n",
        account.login,
        account.name,
        account.email,
        token.kind.label(),
        token.owner,
        expiry(token, now)
    );
    for warning in warnings {
        let _ = writeln!(text, "  Warning:  {warning}");
    }
    text
}

fn table(tokens: &[GithubToken], now: OffsetDateTime) -> String {
    if tokens.is_empty() {
        return "No GitHub tokens are stored. Store one with `agentctl github login --owner OWNER`.\n".into();
    }
    let rows: Vec<[String; 4]> = tokens
        .iter()
        .map(|token| {
            [
                token.owner.clone(),
                token.account.login.clone(),
                token.kind.label().to_owned(),
                expiry(token, now),
            ]
        })
        .collect();
    let width = |column: usize, title: &str| {
        rows.iter()
            .map(|row| row[column].len())
            .chain([title.len()])
            .max()
            .unwrap_or_default()
    };
    let (owner, account, kind) = (width(0, "OWNER"), width(1, "ACCOUNT"), width(2, "KIND"));
    let mut text = format!(
        "{:<owner$}  {:<account$}  {:<kind$}  EXPIRES\n",
        "OWNER", "ACCOUNT", "KIND"
    );
    for [o, a, k, e] in &rows {
        let _ = writeln!(text, "{o:<owner$}  {a:<account$}  {k:<kind$}  {e}");
    }
    text
}

/// The token's expiry date, marked when it has passed; GitHub states none for tokens without one.
fn expiry(token: &GithubToken, now: OffsetDateTime) -> String {
    match token.expires_at {
        None => "none stated".into(),
        Some(expires_at) if token.expired(now) => format!("{} (expired)", expires_at.date()),
        Some(expires_at) => expires_at.date().to_string(),
    }
}

fn forgotten(token: &GithubToken) -> String {
    format!(
        "Forgot the GitHub token for {owner}; agents for {owner} lose GitHub access at once, and nothing else.\n\
         It works on GitHub until it expires or is revoked: sign in as {login} at {page}\n",
        owner = token.owner,
        login = token.account.login,
        page = token.kind.revoke_page()
    )
}

#[cfg(test)]
mod tests {
    use agent::github::{GithubAccount, TokenKind};

    use super::*;

    /// 2027-01-13 10:00 UTC.
    fn january() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_799_834_400).expect("timestamp")
    }

    fn token(owner: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>) -> GithubToken {
        GithubToken {
            owner: owner.into(),
            account: GithubAccount {
                login: format!("{owner}-bot"),
                name: format!("{owner} bot"),
                email: format!("1+{owner}-bot@users.noreply.github.com"),
            },
            kind,
            expires_at,
        }
    }

    fn checked(owner: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>, missing: &[&str]) -> GithubLogin {
        GithubLogin {
            token: token(owner, kind, expires_at),
            missing_scopes: missing.iter().map(|scope| (*scope).to_owned()).collect(),
        }
    }

    fn repository(owner: &str, name: &str) -> Repository {
        Repository::from_remote(&format!("https://github.com/{owner}/{name}")).expect("repository")
    }

    #[test]
    fn the_summary_names_the_account_and_lists_the_warnings() {
        let narrow = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        let before = january() - time::Duration::days(30);
        assert_eq!(
            summary(&narrow, &warnings(&narrow, None, false, before), before),
            "\n  Acts as:  digdir-bot (digdir bot <1+digdir-bot@users.noreply.github.com>)\n  Kind:     fine-grained\n  For:      digdir\n  Expires:  2027-01-13\n"
        );
        let broad = checked("digdir", TokenKind::Classic, None, &["workflow"]);
        let text = summary(&broad, &warnings(&broad, None, false, january()), january());
        assert!(text.contains("  Expires:  none stated\n"), "{text}");
        assert!(
            text.ends_with(
                "  Warning:  this classic token reaches every repository digdir-bot can, not only digdir's\n\
                 \x20 Warning:  the token lacks workflow, so some `gh` and Git commands will fail\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn each_warning_is_raised_by_its_own_condition() {
        let case_web = repository("digdir", "case-web");
        let fine = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        let long_before = january() - time::Duration::days(8);
        assert_eq!(
            warnings(&fine, Some((&case_web, true)), false, long_before),
            Vec::<String>::new()
        );
        assert_eq!(warnings(&fine, None, false, long_before), Vec::<String>::new());

        assert_eq!(
            warnings(&fine, Some((&case_web, false)), false, long_before),
            ["the token cannot see digdir/case-web: it was not selected, or digdir has yet to approve the token"]
        );
        let personal = GithubLogin {
            token: GithubToken {
                owner: "digdir-bot".into(),
                ..fine.token.clone()
            },
            missing_scopes: Vec::new(),
        };
        let own = repository("digdir-bot", "notes");
        assert_eq!(
            warnings(&personal, Some((&own, false)), false, long_before),
            ["the token cannot see digdir-bot/notes: it was probably not selected under Repository access"]
        );

        assert_eq!(
            warnings(&fine, None, false, january() - time::Duration::days(6)),
            ["the token expires on 2027-01-13, within a week"]
        );

        let classic = checked("digdir", TokenKind::Classic, None, &["read:org"]);
        assert_eq!(
            warnings(&classic, Some((&case_web, false)), false, january()),
            [
                "digdir-bot cannot see digdir/case-web; give the account access to it",
                "this classic token reaches every repository digdir-bot can, not only digdir's",
                "the token lacks read:org, so some `gh` and Git commands will fail",
            ]
        );
    }

    #[test]
    fn the_repository_is_origins_on_github_else_the_first_on_github() {
        let remotes = "\
fork\tgit@github.com:someone/case-web.git (fetch)
fork\tgit@github.com:someone/case-web.git (push)
origin\thttps://github.com/Digdir/case-web.git (fetch)
origin\thttps://github.com/Digdir/case-web.git (push)
";
        assert_eq!(repository_from_remotes(remotes), Some(repository("digdir", "case-web")));
        let without_origin = "\
gitlab\thttps://gitlab.com/digdir/case-web.git (fetch)
origin\tgit@gitlab.com:digdir/case-web.git (fetch)
upstream\tssh://git@github.com/altinn/app-lib.git (fetch)
z\thttps://github.com/zzz/other (fetch)
";
        assert_eq!(
            repository_from_remotes(without_origin),
            Some(repository("altinn", "app-lib"))
        );
        assert_eq!(
            repository_from_remotes("origin\thttps://gitlab.com/a/b.git (fetch)\n"),
            None
        );
        assert_eq!(repository_from_remotes(""), None);
    }

    #[test]
    fn the_list_has_owner_account_kind_and_expiry() {
        let tokens = [
            token("digdir", TokenKind::FineGrained, Some(january())),
            token("altinn", TokenKind::Other, None),
        ];
        let text = table(&tokens, january());
        assert_eq!(
            text,
            "\
OWNER   ACCOUNT     KIND          EXPIRES
digdir  digdir-bot  fine-grained  2027-01-13 (expired)
altinn  altinn-bot  other         none stated
"
        );
        assert!(table(&[], january()).starts_with("No GitHub tokens are stored."));
    }

    #[test]
    fn logging_out_says_it_is_immediate_and_where_to_revoke_the_token() {
        let text = forgotten(&token("digdir", TokenKind::FineGrained, None));
        assert_eq!(
            text,
            "Forgot the GitHub token for digdir; agents for digdir lose GitHub access at once, and nothing else.\n\
             It works on GitHub until it expires or is revoked: sign in as digdir-bot at https://github.com/settings/personal-access-tokens\n"
        );
        assert!(forgotten(&token("digdir", TokenKind::Classic, None)).contains("https://github.com/settings/tokens\n"));
    }

    #[tokio::test(flavor = "local")]
    async fn a_token_given_as_the_owner_is_refused_without_being_repeated() {
        let client = Client::for_path("/nonexistent/agentctl.sock".into());
        for result in [
            login(&client, Some("ghp_FAKE_canary"), true).await,
            logout(&client, "ghp_FAKE_canary").await,
        ] {
            let Err(error) = result else {
                panic!("not an owner");
            };
            let message = crate::error_text(&error);
            assert!(
                message.contains("must be a GitHub user or organization name"),
                "{message}"
            );
            assert!(!message.contains("FAKE_canary"), "{message}");
        }
        assert_eq!(github_owner("DigDir").expect("owner"), "digdir");
    }

    #[tokio::test(flavor = "local")]
    async fn without_a_terminal_a_login_needs_the_token_on_standard_input() {
        assert!(!prompt::interactive(), "tests run without a terminal");
        let client = Client::for_path("/nonexistent/agentctl.sock".into());
        let Err(crate::CommandError::Agent(Error::Invalid(message))) = login(&client, Some("digdir"), false).await
        else {
            panic!("refused before reaching agentd");
        };
        assert!(message.contains("--from-stdin"), "{message}");
    }
}
