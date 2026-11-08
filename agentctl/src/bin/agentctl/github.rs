//! `agentctl github`: the GitHub accounts agents use, each a token kept under a name manifests ask
//! for.

use std::fmt::Write as _;

use agent::{
    Agent, Error,
    accounts::{self, AccountType},
    control_api::Client,
    github::{Account, Login},
};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{CommandResult, prompt};

/// Has agentd keep a GitHub token as `name`. At a terminal it walks the person through GitHub's
/// pre-filled form for a fine-grained token and reads the pasted token without showing it. With
/// `from_stdin` it reads the token from standard input instead. What GitHub says about the token,
/// with warnings, is shown once it is kept.
///
/// # Errors
///
/// Returns an error when the name is invalid, there is no terminal to paste on without
/// `from_stdin`, no token is given, GitHub rejects it, or agentd cannot keep it.
pub(crate) async fn login(client: &Client, name: &str, from_stdin: bool) -> CommandResult<()> {
    let name = token_name(name)?;
    let token = if from_stdin {
        crate::read_token_from_stdin()?
    } else if prompt::interactive() {
        let owner = ask_owner()?;
        paste_from_form(&owner, &name).await?
    } else {
        return Err(Error::Invalid(
            "no terminal to paste the token on; pipe it to `agentctl github login NAME --from-stdin`".into(),
        )
        .into());
    };
    let accounts::Login::Github(login) = client.account_login(AccountType::Github, &name, &token).await?;
    let now = OffsetDateTime::now_utc();
    eprint!("{}", summary(&login, &warnings(&login, now), now));
    println!("{}", stored_line(&login));
    println!("{}", manifest_line(&login.account.name));
    Ok(())
}

fn stored_line(login: &Login) -> String {
    format!(
        "Agents asking for {} now use GitHub as {}.",
        login.account.name, login.account.identity.login
    )
}

/// What a manifest names to use the token kept as `name`.
fn manifest_line(name: &str) -> String {
    format!("Agents use it with `accounts: [{{type: github, name: {name}}}]` in their manifest.")
}

/// The user or organization the person names as the owner of the repositories the token reaches.
fn ask_owner() -> Result<String, Error> {
    let mut answer = prompt::ask(
        &[
            "Which GitHub user or organization's repositories will the token reach?",
            "Owner",
        ],
        None,
    )?;
    loop {
        // Never repeated: the answer may be a token pasted in the wrong place.
        if agent::github::valid_owner(&answer) {
            return Ok(answer.to_ascii_lowercase());
        }
        if !answer.is_empty() {
            eprintln!("That is not a GitHub user or organization name: letters, digits and inner hyphens.");
        }
        answer = prompt::ask(&["Owner"], None)?;
    }
}

/// Shows the steps to create a fine-grained token for `owner`, opens GitHub's pre-filled form,
/// and reads the token the person pastes.
async fn paste_from_form(owner: &str, name: &str) -> Result<Zeroizing<String>, Error> {
    let host = whoami::hostname().unwrap_or_else(|_| "this host".into());
    // The token's name on GitHub is short, so it takes the host's first label; the description has all of it.
    let short_host = host.split('.').next().unwrap_or(&host);
    let url = agent::github::token_form_url(
        owner,
        &format!("agentctl {name} on {short_host}"),
        &format!("Agents asking for {name} on {host}, kept by agentctl"),
    );
    eprint!(
        "\nCreate a fine-grained GitHub token for {owner}:\n\n  \
         1. Sign in to github.com as the account the agents should act as, such as a bot account.\n  \
         2. In the form below, the name, owner, a {days}-day lifetime and the permissions are filled in.\n  \
         3. Under Repository access, choose \"Only select repositories\" and pick the repositories the agents need.\n     \
         The form cannot be given the repositories.\n  \
         4. Generate the token, copy it and paste it here.\n\n  {url}\n",
        days = agent::github::TOKEN_LIFETIME_DAYS,
    );
    if crate::launch::launch_blocked(|name| std::env::var_os(name)).is_none()
        && let Err(error) = crate::launch::open_url(&url).await
    {
        eprintln!("Could not open the browser ({error}); open the address above instead.");
    }
    prompt::secret(&format!("Paste the GitHub token for {name} (it is not shown)"))
}

/// Lists the kept tokens: name, account, kind and expiry, then the names agents ask for that
/// have no token.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached.
pub(crate) async fn list(client: &Client) -> CommandResult<()> {
    let kept = github_accounts(client.accounts().await?);
    let agents = client.list_agents().await?;
    print!("{}{}", table(&kept, OffsetDateTime::now_utc()), unkept(&kept, &agents));
    Ok(())
}

/// The GitHub accounts among `accounts`.
fn github_accounts(accounts: Vec<accounts::Account>) -> Vec<Account> {
    accounts
        .into_iter()
        .map(|account| match account {
            accounts::Account::Github(account) => account,
        })
        .collect()
}

/// The names `agents` ask for that no token in `kept` has, each with the agents asking, which run
/// without GitHub access until it is kept.
fn unkept(kept: &[Account], agents: &[Agent]) -> String {
    let mut wanted = std::collections::BTreeMap::<String, Vec<&str>>::new();
    for agent in agents {
        if let Some(name) = agent.spec.account(AccountType::Github).map(str::to_ascii_lowercase)
            && !kept.iter().any(|account| account.name == name)
        {
            wanted.entry(name).or_default().push(&agent.metadata.name);
        }
    }
    let mut text = String::new();
    for (name, agents) in wanted {
        let _ = writeln!(
            text,
            "\nNo token is kept as {name}, which {} ask for, so they run without GitHub access. \
             Keep one with `agentctl github login {name}`.",
            agents.join(", ")
        );
    }
    text
}

/// Forgets the token named `name`, and says where to revoke it on GitHub.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached or cannot update its database.
pub(crate) async fn logout(client: &Client, name: &str) -> CommandResult<()> {
    let name = &token_name(name)?;
    match client.account_logout(AccountType::Github, name).await? {
        Some(accounts::Account::Github(account)) => print!("{}", forgotten(&account)),
        None => println!("No GitHub token named {name} is kept; nothing changed."),
    }
    Ok(())
}

/// `value` as a token's name, lowercased. Clap would repeat a rejected value, which may be a
/// token pasted in the wrong place, so names are checked here instead.
fn token_name(value: &str) -> Result<String, Error> {
    if accounts::valid_name(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(Error::Invalid(accounts::NAME_RULE.into()))
    }
}

/// What to warn of about a token: that it expires within [`agent::github::EXPIRY_WARNING`],
/// reaches every repository of its account, or lacks recommended scopes.
fn warnings(login: &Login, now: OffsetDateTime) -> Vec<String> {
    let account = &login.account;
    let mut warnings = Vec::new();
    if let Some(expires_at) = account
        .expires_at
        .filter(|expires_at| *expires_at <= now + agent::github::EXPIRY_WARNING)
    {
        warnings.push(format!("the token expires on {}, within a week", expires_at.date()));
    }
    warnings.extend(account.kind.reach_warning(&account.identity.login));
    if !login.missing_scopes.is_empty() {
        warnings.push(format!(
            "the token lacks {}, so some `gh` and Git commands will fail",
            login.missing_scopes.join(", ")
        ));
    }
    warnings
}

/// What GitHub said about a token: its name, whom it acts as, its kind and expiry, then `warnings`.
fn summary(login: &Login, warnings: &[String], now: OffsetDateTime) -> String {
    let account = &login.account;
    let identity = &account.identity;
    let mut text = format!(
        "\n  Name:     {}\n  Acts as:  {} ({} <{}>)\n  Kind:     {}\n  Expires:  {}\n",
        account.name,
        identity.login,
        identity.name,
        identity.email,
        account.kind.label(),
        expiry(account, now)
    );
    for warning in warnings {
        let _ = writeln!(text, "  Warning:  {warning}");
    }
    text
}

fn table(accounts: &[Account], now: OffsetDateTime) -> String {
    if accounts.is_empty() {
        return "No GitHub tokens are kept. Keep one with `agentctl github login NAME`.\n".into();
    }
    let rows: Vec<[String; 4]> = accounts
        .iter()
        .map(|account| {
            [
                account.name.clone(),
                account.identity.login.clone(),
                account.kind.label().to_owned(),
                expiry(account, now),
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
    let (name, account, kind) = (width(0, "NAME"), width(1, "ACCOUNT"), width(2, "KIND"));
    let mut text = format!(
        "{:<name$}  {:<account$}  {:<kind$}  EXPIRES\n",
        "NAME", "ACCOUNT", "KIND"
    );
    for [n, a, k, e] in &rows {
        let _ = writeln!(text, "{n:<name$}  {a:<account$}  {k:<kind$}  {e}");
    }
    text
}

/// The token's expiry date, marked when it has passed; GitHub states none for tokens without one.
fn expiry(account: &Account, now: OffsetDateTime) -> String {
    match account.expires_at {
        None => "none stated".into(),
        Some(expires_at) if account.expired(now) => format!("{} (expired)", expires_at.date()),
        Some(expires_at) => expires_at.date().to_string(),
    }
}

fn forgotten(account: &Account) -> String {
    format!(
        "Forgot the GitHub token {name}; agents asking for {name} lose GitHub access at once, and nothing else.\n\
         It works on GitHub until it expires or is revoked: sign in as {login} at {page}\n",
        name = account.name,
        login = account.identity.login,
        page = account.kind.revoke_page()
    )
}

#[cfg(test)]
mod tests {
    use agent::github::{Identity, TokenKind};

    use super::*;

    /// 2027-01-13 10:00 UTC.
    fn january() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_799_834_400).expect("timestamp")
    }

    fn token(name: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>) -> Account {
        Account {
            name: name.into(),
            identity: Identity {
                login: format!("{name}-bot"),
                name: format!("{name} bot"),
                email: format!("1+{name}-bot@users.noreply.github.com"),
            },
            kind,
            expires_at,
        }
    }

    fn checked(name: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>, missing: &[&str]) -> Login {
        Login {
            account: token(name, kind, expires_at),
            missing_scopes: missing.iter().map(|scope| (*scope).to_owned()).collect(),
        }
    }

    #[test]
    fn the_summary_names_the_account_and_lists_the_warnings() {
        let narrow = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        let before = january() - time::Duration::days(30);
        assert_eq!(
            summary(&narrow, &warnings(&narrow, before), before),
            "\n  Name:     digdir\n  Acts as:  digdir-bot (digdir bot <1+digdir-bot@users.noreply.github.com>)\n  Kind:     fine-grained\n  Expires:  2027-01-13\n"
        );
        let broad = checked("digdir", TokenKind::Classic, None, &["workflow"]);
        let text = summary(&broad, &warnings(&broad, january()), january());
        assert!(text.contains("  Expires:  none stated\n"), "{text}");
        assert!(
            text.ends_with(
                "  Warning:  this classic token reaches every repository digdir-bot can\n\
                 \x20 Warning:  the token lacks workflow, so some `gh` and Git commands will fail\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn each_warning_is_raised_by_its_own_condition() {
        let fine = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        assert_eq!(
            warnings(&fine, january() - time::Duration::days(8)),
            Vec::<String>::new()
        );
        assert_eq!(
            warnings(&fine, january() - time::Duration::days(6)),
            ["the token expires on 2027-01-13, within a week"]
        );

        let classic = checked("digdir", TokenKind::Classic, None, &["read:org"]);
        assert_eq!(
            warnings(&classic, january()),
            [
                "this classic token reaches every repository digdir-bot can",
                "the token lacks read:org, so some `gh` and Git commands will fail",
            ]
        );
    }

    #[test]
    fn the_manifest_line_names_the_account() {
        assert_eq!(
            manifest_line("digdir-agent"),
            "Agents use it with `accounts: [{type: github, name: digdir-agent}]` in their manifest."
        );
    }

    fn agent_asking_for(name: &str, account: Option<&str>) -> Agent {
        let accounts = account.map_or_else(String::new, |account| {
            format!("  accounts: [{{type: github, name: {account}}}]\n")
        });
        let manifest = format!(
            "apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: {name}
spec:
  sandbox:
    image:
      type: reference
      reference: example.com/agent:latest
    platform:
      os: linux
    resources:
      cpu: \"1\"
      memory: \"1Gi\"
      rootFilesystem:
        capacity: \"8Gi\"
        mode: layered
  harnesses: [{{type: pi}}]
{accounts}"
        );
        agent::manifest::decode(manifest.as_bytes()).expect("manifest")
    }

    #[test]
    fn the_list_names_each_token_agents_ask_for_that_is_not_kept() {
        let agents = [
            agent_asking_for("web", Some("Case-Web")),
            agent_asking_for("api", Some("case-web")),
            agent_asking_for("docs", Some("digdir")),
            agent_asking_for("plain", None),
        ];
        let kept = [token("digdir", TokenKind::FineGrained, None)];
        assert_eq!(
            unkept(&kept, &agents),
            "\nNo token is kept as case-web, which web, api ask for, so they run without GitHub access. \
             Keep one with `agentctl github login case-web`.\n"
        );
        assert_eq!(unkept(&kept, &agents[2..]), "");
    }

    #[test]
    fn the_list_has_name_account_kind_and_expiry() {
        let tokens = [
            token("digdir", TokenKind::FineGrained, Some(january())),
            token("altinn", TokenKind::Classic, None),
        ];
        let text = table(&tokens, january());
        assert_eq!(
            text,
            "\
NAME    ACCOUNT     KIND          EXPIRES
digdir  digdir-bot  fine-grained  2027-01-13 (expired)
altinn  altinn-bot  classic       none stated
"
        );
        assert!(table(&[], january()).starts_with("No GitHub tokens are kept."));
    }

    #[test]
    fn logging_out_says_it_is_immediate_and_where_to_revoke_the_token() {
        let text = forgotten(&token("digdir", TokenKind::FineGrained, None));
        assert_eq!(
            text,
            "Forgot the GitHub token digdir; agents asking for digdir lose GitHub access at once, and nothing else.\n\
             It works on GitHub until it expires or is revoked: sign in as digdir-bot at https://github.com/settings/personal-access-tokens\n"
        );
        assert!(forgotten(&token("digdir", TokenKind::Classic, None)).contains("https://github.com/settings/tokens\n"));
    }

    #[tokio::test(flavor = "local")]
    async fn a_token_given_as_the_name_is_refused_without_being_repeated() {
        let client = Client::for_path("/nonexistent/agentctl.sock".into());
        for result in [
            login(&client, "ghp_FAKE_canary", true).await,
            logout(&client, "ghp_FAKE_canary").await,
        ] {
            let Err(error) = result else {
                panic!("not a name");
            };
            let message = error.to_string();
            assert!(
                message.contains("the name must be letters, digits and inner hyphens"),
                "{message}"
            );
            assert!(!message.contains("FAKE_canary"), "{message}");
        }
        assert_eq!(token_name("Altinn-Studio").expect("name"), "altinn-studio");
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
