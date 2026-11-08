//! `agentctl github`: the GitHub accounts agents use, each a token kept under a name manifests ask
//! for.

use std::fmt::Write as _;

use agent::{
    Agent, Error,
    accounts::{self, AccountType},
    control_api::Client,
    github::{Account, Login, Repository},
};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{CommandResult, prompt};

/// Has agentd keep a GitHub token as `name`, by default this repository's name. At a terminal it
/// walks the person through GitHub's pre-filled form for a fine-grained token and reads the pasted
/// token without showing it. With `from_stdin` it reads the token from standard input instead.
/// What GitHub says about the token, with warnings, is shown once it is kept.
///
/// # Errors
///
/// Returns an error when there is no terminal to paste on without `from_stdin`, no name is given
/// or found, no token is given, GitHub rejects it, or agentd cannot keep it.
pub(crate) async fn login(client: &Client, name: Option<&str>, from_stdin: bool) -> CommandResult<()> {
    let name = name.map(token_name).transpose()?;
    if !from_stdin && !prompt::interactive() {
        return Err(Error::Invalid(
            "no terminal to paste the token on; pipe it to `agentctl github login NAME --from-stdin`".into(),
        )
        .into());
    }
    let repository = this_repository();
    let name = match (name, repository.as_ref().and_then(Repository::token_name)) {
        (Some(name), _) => name,
        (None, Some(name)) => {
            eprintln!("Keeping the token as {name}, after this repository.");
            name
        }
        (None, None) => {
            return Err(Error::Invalid(
                "no GitHub repository here to name the token after; name it: `agentctl github login NAME`".into(),
            )
            .into());
        }
    };
    let token = if from_stdin {
        crate::read_token_from_stdin()?
    } else {
        let owner = ask_owner(repository.as_ref())?;
        let theirs = repository.as_ref().filter(|repository| repository.owner == owner);
        paste_from_form(&owner, &name, theirs).await?
    };
    let accounts::Login::Github(login) = client.account_login(AccountType::Github, &name, &token).await?;
    // Asked here, where the token already is, since only the person knows which repository matters.
    let visible = match &repository {
        Some(repository) => agent::github::can_see(&token, repository)
            .await
            .ok()
            .map(|seen| (repository, seen)),
        None => None,
    };
    let now = OffsetDateTime::now_utc();
    eprint!("{}", summary(&login, &warnings(&login, visible, now), now));
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

/// The owner the person names, offering the owner of this repository's GitHub remote.
fn ask_owner(repository: Option<&Repository>) -> Result<String, Error> {
    let context = repository.map(|repository| format!("This repository is {repository} on GitHub."));
    let mut lines = vec!["Which GitHub user or organization's repositories will the token reach?"];
    lines.extend(context.as_deref());
    lines.push("Owner");
    let default = repository.map(|repository| repository.owner.as_str());
    let mut answer = prompt::ask(&lines, default)?;
    loop {
        // Never repeated: the answer may be a token pasted in the wrong place.
        if agent::github::valid_owner(&answer) {
            return Ok(answer.to_ascii_lowercase());
        }
        if !answer.is_empty() {
            eprintln!("That is not a GitHub user or organization name: letters, digits and inner hyphens.");
        }
        answer = prompt::ask(&["Owner"], default)?;
    }
}

/// Shows the steps to create a fine-grained token for `owner`, opens GitHub's pre-filled form,
/// and reads the token the person pastes. `repository` is this repository when `owner` owns it.
async fn paste_from_form(owner: &str, name: &str, repository: Option<&Repository>) -> Result<Zeroizing<String>, Error> {
    let host = whoami::hostname().unwrap_or_else(|_| "this host".into());
    // The token's name on GitHub is short, so it takes the host's first label; the description has all of it.
    let short_host = host.split('.').next().unwrap_or(&host);
    let url = agent::github::token_form_url(
        owner,
        &format!("agentctl {name} on {short_host}"),
        &format!("Agents asking for {name} on {host}, kept by agentctl"),
    );
    let pick = repository.map_or_else(
        || "the repositories the agents need".to_owned(),
        |repository| format!("{repository} and any others the agents need"),
    );
    eprint!(
        "\nCreate a fine-grained GitHub token for {owner}:\n\n  \
         1. Sign in to github.com as the account the agents should act as, such as a bot account.\n  \
         2. In the form below, the name, owner, a {days}-day lifetime and the permissions are filled in.\n  \
         3. Under Repository access, choose \"Only select repositories\" and pick {pick}.\n     \
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

/// What to warn of about a token: that it cannot see this repository (`visible`), expires
/// within [`agent::github::EXPIRY_WARNING`], reaches every repository of its account, or lacks
/// recommended scopes.
fn warnings(login: &Login, visible: Option<(&Repository, bool)>, now: OffsetDateTime) -> Vec<String> {
    let account = &login.account;
    let acts_as = &account.identity.login;
    let mut warnings = Vec::new();
    if let Some((repository, false)) = visible {
        warnings.push(if !account.kind.single_owner() {
            format!("{acts_as} cannot see {repository}; give the account access to it")
        } else if repository.owner.eq_ignore_ascii_case(acts_as) {
            format!("the token cannot see {repository}: it was probably not selected under Repository access")
        } else {
            format!(
                "the token cannot see {repository}: it was not selected, or {} has yet to approve the token",
                repository.owner
            )
        });
    }
    if let Some(expires_at) = account
        .expires_at
        .filter(|expires_at| *expires_at <= now + agent::github::EXPIRY_WARNING)
    {
        warnings.push(format!("the token expires on {}, within a week", expires_at.date()));
    }
    warnings.extend(account.kind.reach_warning(acts_as));
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

    fn repository(owner: &str, name: &str) -> Repository {
        Repository::from_remote(&format!("https://github.com/{owner}/{name}")).expect("repository")
    }

    #[test]
    fn the_summary_names_the_account_and_lists_the_warnings() {
        let narrow = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        let before = january() - time::Duration::days(30);
        assert_eq!(
            summary(&narrow, &warnings(&narrow, None, before), before),
            "\n  Name:     digdir\n  Acts as:  digdir-bot (digdir bot <1+digdir-bot@users.noreply.github.com>)\n  Kind:     fine-grained\n  Expires:  2027-01-13\n"
        );
        let broad = checked("digdir", TokenKind::Classic, None, &["workflow"]);
        let text = summary(&broad, &warnings(&broad, None, january()), january());
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
        let case_web = repository("digdir", "case-web");
        let fine = checked("digdir", TokenKind::FineGrained, Some(january()), &[]);
        let long_before = january() - time::Duration::days(8);
        assert_eq!(
            warnings(&fine, Some((&case_web, true)), long_before),
            Vec::<String>::new()
        );
        assert_eq!(warnings(&fine, None, long_before), Vec::<String>::new());

        assert_eq!(
            warnings(&fine, Some((&case_web, false)), long_before),
            ["the token cannot see digdir/case-web: it was not selected, or digdir has yet to approve the token"]
        );
        let own = repository("digdir-bot", "notes");
        assert_eq!(
            warnings(&fine, Some((&own, false)), long_before),
            ["the token cannot see digdir-bot/notes: it was probably not selected under Repository access"]
        );

        assert_eq!(
            warnings(&fine, None, january() - time::Duration::days(6)),
            ["the token expires on 2027-01-13, within a week"]
        );

        let classic = checked("digdir", TokenKind::Classic, None, &["read:org"]);
        assert_eq!(
            warnings(&classic, Some((&case_web, false)), january()),
            [
                "digdir-bot cannot see digdir/case-web; give the account access to it",
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
            login(&client, Some("ghp_FAKE_canary"), true).await,
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
        let Err(crate::CommandError::Agent(Error::Invalid(message))) = login(&client, Some("digdir"), false).await
        else {
            panic!("refused before reaching agentd");
        };
        assert!(message.contains("--from-stdin"), "{message}");
    }
}
