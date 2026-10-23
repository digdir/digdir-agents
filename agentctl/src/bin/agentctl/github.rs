//! `agentctl github`: the GitHub tokens Agents use, one per owner.

use std::fmt::Write as _;

use agent::{
    Error,
    control_api::Client,
    github::{GithubLogin, GithubToken},
};
use time::OffsetDateTime;

use crate::{CommandResult, prompt};

/// Has agentd keep a GitHub token for Agents of `owner`, read from standard input or pasted at
/// the terminal. At the terminal, what GitHub says about the token is shown and must be confirmed
/// before anything is stored; standard input leaves nothing to answer on, so it is shown after.
///
/// # Errors
///
/// Returns an error when there is no terminal to paste on without `from_stdin`, no token is
/// given, GitHub rejects it, or agentd cannot keep it.
pub(crate) async fn login(client: &Client, owner: &str, from_stdin: bool) -> CommandResult<()> {
    let owner = &github_owner(owner)?;
    let token = if from_stdin {
        crate::read_token_from_stdin()?
    } else if prompt::interactive() {
        prompt::secret(&format!("Paste the GitHub token for {owner} (it is not shown)"))?
    } else {
        return Err(Error::Invalid(
            "no terminal to paste the token on; pipe it to `agentctl github login --owner OWNER --from-stdin`".into(),
        )
        .into());
    };
    let now = OffsetDateTime::now_utc();
    if !from_stdin {
        // Checked here so nothing is stored before the person agrees; agentd checks it again.
        let checked = agent::github::check(&token, owner).await?;
        eprint!("{}", summary(&checked, now));
        let question = format!(
            "Agents for {} will use GitHub as {}. Store this token?",
            checked.token.owner, checked.token.account.login
        );
        if !prompt::confirm_lines(&[&question])? {
            eprintln!("Nothing was stored.");
            return Ok(());
        }
    }
    let stored = client.github_login(&token, owner).await?;
    if from_stdin {
        eprint!("{}", summary(&stored, now));
    }
    println!(
        "Agents for {} now use GitHub as {}.",
        stored.token.owner, stored.token.account.login
    );
    Ok(())
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

/// What GitHub said about a token: whom it acts as, its kind, owner and expiry, then warnings.
fn summary(login: &GithubLogin, now: OffsetDateTime) -> String {
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
    if !token.kind.single_owner() {
        let _ = writeln!(
            text,
            "  Warning:  this {} token reaches every repository {} can, not only {}'s",
            token.kind.label(),
            account.login,
            token.owner
        );
    }
    if !login.missing_scopes.is_empty() {
        let _ = writeln!(
            text,
            "  Warning:  the token lacks {}, so some `gh` and Git commands will fail",
            login.missing_scopes.join(", ")
        );
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
        "Forgot the GitHub token for {owner}; Agents for {owner} lose GitHub access at once.\n\
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

    #[test]
    fn the_summary_names_the_account_and_warns_of_broad_tokens_and_missing_scopes() {
        let narrow = GithubLogin {
            token: token("digdir", TokenKind::FineGrained, Some(january())),
            missing_scopes: Vec::new(),
        };
        assert_eq!(
            summary(&narrow, january() - time::Duration::days(1)),
            "\n  Acts as:  digdir-bot (digdir bot <1+digdir-bot@users.noreply.github.com>)\n  Kind:     fine-grained\n  For:      digdir\n  Expires:  2027-01-13\n"
        );
        let broad = GithubLogin {
            token: token("digdir", TokenKind::Classic, None),
            missing_scopes: vec!["workflow".into(), "read:project".into()],
        };
        let text = summary(&broad, january());
        assert!(text.contains("  Expires:  none stated\n"), "{text}");
        assert!(
            text.contains(
                "  Warning:  this classic token reaches every repository digdir-bot can, not only digdir's\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  Warning:  the token lacks workflow, read:project, so some `gh` and Git commands will fail\n"
            ),
            "{text}"
        );
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
            "Forgot the GitHub token for digdir; Agents for digdir lose GitHub access at once.\n\
             It works on GitHub until it expires or is revoked: sign in as digdir-bot at https://github.com/settings/personal-access-tokens\n"
        );
        assert!(forgotten(&token("digdir", TokenKind::Classic, None)).contains("https://github.com/settings/tokens\n"));
    }

    #[tokio::test(flavor = "local")]
    async fn a_token_given_as_the_owner_is_refused_without_being_repeated() {
        let client = Client::for_path("/nonexistent/agentctl.sock".into());
        for result in [
            login(&client, "ghp_FAKE_canary", true).await,
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
        let Err(crate::CommandError::Agent(Error::Invalid(message))) = login(&client, "digdir", false).await else {
            panic!("refused before reaching agentd");
        };
        assert!(message.contains("--from-stdin"), "{message}");
    }
}
