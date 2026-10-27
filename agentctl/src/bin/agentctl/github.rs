//! `agentctl github`: the GitHub tokens Agents use, each stored under a name manifests ask for.

use std::fmt::Write as _;

use agent::{
    Error,
    control_api::Client,
    github::{GithubLogin, GithubToken, Repository},
};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{CommandError, CommandResult, prompt};

/// Has agentd keep a GitHub token as `name`. At the terminal the person names it, offered the
/// name the agent here asks for and this repository's, then signs in through the browser as an
/// account, or walks through GitHub's pre-filled token form for one owner, by default the owner of
/// this repository's GitHub remote. What GitHub says about the token, with warnings, is shown for
/// the person to confirm before anything is stored; a rejected or declined token can be replaced.
/// From standard input it stores at once, by default as this repository's name, and shows the same
/// after.
///
/// # Errors
///
/// Returns an error when there is no terminal to paste on without `from_stdin`, no name is given
/// or found, no token is given, GitHub rejects it, or agentd cannot keep it.
pub(crate) async fn login(client: &Client, name: Option<&str>, from_stdin: bool) -> CommandResult<()> {
    let name = name.map(token_name).transpose()?;
    let repository = this_repository();
    if from_stdin {
        let name = match (name, repository.as_ref().and_then(Repository::token_name)) {
            (Some(name), _) => name,
            (None, Some(name)) => {
                eprintln!("Storing the token as {name}, after this repository.");
                name
            }
            (None, None) => {
                return Err(Error::Invalid(
                    "no GitHub repository here to name the token after; name it: `agentctl github login NAME --from-stdin`"
                        .into(),
                )
                .into());
            }
        };
        let token = crate::read_token_from_stdin()?;
        let stored = client.github_login(&token, &name).await?;
        let now = OffsetDateTime::now_utc();
        eprint!("{}", summary(&stored, &warnings(&stored, None, false, now), now));
        println!("{}", stored_line(&stored));
        return Ok(());
    }
    if !prompt::interactive() {
        return Err(Error::Invalid(
            "no terminal to paste the token on; pipe it to `agentctl github login NAME --from-stdin`".into(),
        )
        .into());
    }
    let name = match name {
        Some(name) => name,
        None => ask_name(&suggestions(client, repository.as_ref()).await)?,
    };
    let browser = prompt::choose("How should agents sign in to GitHub?", &SIGN_IN_OPTIONS)? == 0;
    let gh = browser
        .then(|| {
            gh_or_explain(crate::launch::find_program(
                &["gh"],
                std::env::var_os("PATH").as_deref(),
            ))
        })
        .flatten();
    let way = match gh {
        Some(gh) => SignIn::Browser { gh },
        None => SignIn::Form {
            owner: ask_owner(repository.as_ref())?,
        },
    };
    // A token from the form is for one owner, so only that owner's repository can tell.
    let repository = match &way {
        SignIn::Form { owner } => repository.filter(|repository| repository.owner == *owner),
        SignIn::Browser { .. } => repository,
    };
    loop {
        let token = match &way {
            SignIn::Browser { gh } => agent::github::sign_in(gh),
            SignIn::Form { owner } => Ok(paste_from_form(owner, &name, repository.as_ref()).await?),
        };
        let browser = matches!(way, SignIn::Browser { .. });
        match review(token, &name, repository.as_ref(), browser).await {
            Ok(Some((token, checked))) => {
                let stored = client.github_login(&token, &checked.token.name).await?;
                println!("{}", stored_line(&stored));
                println!("{}", manifest_line(&stored.token.name));
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => eprintln!("{}", crate::error_text(&error)),
        }
        if !prompt::confirm_lines(&["Try again?"])? {
            return Err(CommandError::Message("no GitHub token was stored".into()));
        }
    }
}

/// How the person gets agents a token.
enum SignIn {
    /// Signing in through the browser with the GitHub CLI `gh`.
    Browser { gh: std::path::PathBuf },
    /// Creating a fine-grained token for `owner` with GitHub's form.
    Form { owner: String },
}

/// How agents can sign in: the first through the browser with the GitHub CLI, the second with a
/// fine-grained token from GitHub's form.
const SIGN_IN_OPTIONS: [&str; 2] = [
    "Sign in through the browser as a GitHub account made for agents, not your own",
    "Create a token for one organization's repositories",
];

/// `gh`, or when it is missing, `None` after saying that the browser sign-in needs it.
fn gh_or_explain(gh: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if gh.is_none() {
        eprintln!(
            "Signing in through the browser needs the GitHub CLI, `gh`, from https://cli.github.com. \
             Install it and run this again, or create a token now."
        );
    }
    gh
}

fn stored_line(stored: &GithubLogin) -> String {
    format!(
        "Agents asking for {} now use GitHub as {}.",
        stored.token.name, stored.token.account.login
    )
}

/// What a manifest names to use the token stored as `name`.
fn manifest_line(name: &str) -> String {
    format!("Agents use it with `accounts: [{{type: github, name: {name}}}]` in their manifest.")
}

/// The names to offer for a new token, first the one the agent here asks for when no unexpired
/// token has it, then `repository`'s, then the name of the folder here: the repository's root, or
/// the current directory outside one, though never the home directory.
async fn suggestions(client: &Client, repository: Option<&Repository>) -> Vec<String> {
    let current = std::env::current_dir().ok();
    let here = match &current {
        Some(directory) => client.resolve_agent_variant(directory.clone(), None).await.ok(),
        None => None,
    };
    let wanted = here.and_then(|agent| agent.spec.github_account().map(str::to_ascii_lowercase));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let folder = current
        .as_deref()
        .map(|directory| crate::tui::repository_root(directory).unwrap_or(directory))
        .filter(|folder| Some(*folder) != home.as_deref())
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    let stored = client.github_accounts().await.unwrap_or_default();
    suggested(
        wanted,
        &stored,
        repository,
        folder.as_deref(),
        OffsetDateTime::now_utc(),
    )
}

/// `wanted`, the name the agent here asks for, unless `stored` has an unexpired token of that name
/// at `now`, then `repository`'s name, then one after `folder`.
fn suggested(
    wanted: Option<String>,
    stored: &[GithubToken],
    repository: Option<&Repository>,
    folder: Option<&str>,
    now: OffsetDateTime,
) -> Vec<String> {
    let wanted = wanted.filter(|wanted| !stored.iter().any(|token| token.name == *wanted && !token.expired(now)));
    let mut names: Vec<String> = wanted.into_iter().collect();
    for name in [
        repository.and_then(Repository::token_name),
        folder.and_then(agent::github::name_after),
    ]
    .into_iter()
    .flatten()
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The name the person gives the token, offering `suggestions`, the first as the default.
fn ask_name(suggestions: &[String]) -> Result<String, Error> {
    let offered = (suggestions.len() > 1).then(|| format!("Suggested: {}.", suggestions.join(", ")));
    let mut lines = vec!["What name should agents' manifests use for this token?"];
    lines.extend(offered.as_deref());
    lines.push("Name");
    let default = suggestions.first().map(String::as_str);
    let mut answer = prompt::ask(&lines, default)?;
    loop {
        // Never repeated: the answer may be a token pasted in the wrong place.
        match token_name(&answer) {
            Ok(name) => return Ok(name),
            Err(_) if answer.is_empty() => {}
            Err(_) => eprintln!("A name is letters, digits and inner hyphens."),
        }
        answer = prompt::ask(&["Name"], default)?;
    }
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
/// and reads the token the person pastes.
async fn paste_from_form(owner: &str, name: &str, repository: Option<&Repository>) -> Result<Zeroizing<String>, Error> {
    let host = whoami::hostname().unwrap_or_else(|_| "this host".into());
    // The token's name on GitHub is short, so it takes the host's first label; the description has all of it.
    let short_host = host.split('.').next().unwrap_or(&host);
    let url = agent::github::token_form_url(
        owner,
        &format!("agentctl {name} on {short_host}"),
        &format!("Agents asking for {name} on {host}, stored by agentctl"),
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

/// Asks GitHub what `token` is and whether it sees `repository`, shows it with any warnings, and
/// asks whether to store it as `name`. `browser` says the person signed in as the token's
/// account, so it reaching all the account can is expected.
async fn review(
    token: Result<Zeroizing<String>, Error>,
    name: &str,
    repository: Option<&Repository>,
    browser: bool,
) -> CommandResult<Option<(Zeroizing<String>, GithubLogin)>> {
    let token = token?;
    // Checked here so nothing is stored before the person agrees; agentd checks it again.
    let checked = agent::github::check(&token, name).await?;
    let visible = match repository {
        Some(repository) => Some((repository, agent::github::can_see(&token, repository).await?)),
        None => None,
    };
    let now = OffsetDateTime::now_utc();
    eprint!("{}", summary(&checked, &warnings(&checked, visible, browser, now), now));
    let reach = if browser && !checked.token.kind.single_owner() {
        ", in every repository it can reach"
    } else {
        ""
    };
    let question = format!(
        "Agents asking for {} will use GitHub as {}{reach}. Store this token?",
        checked.token.name, checked.token.account.login
    );
    Ok(prompt::confirm_lines(&[&question])?.then_some((token, checked)))
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

/// Lists the stored tokens: name, account, kind and expiry.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached.
pub(crate) async fn list(client: &Client) -> CommandResult<()> {
    print!("{}", table(&client.github_accounts().await?, OffsetDateTime::now_utc()));
    Ok(())
}

/// Forgets the token named `name`, and says where to revoke it on GitHub.
///
/// # Errors
///
/// Returns an error when agentd cannot be reached or cannot update its database.
pub(crate) async fn logout(client: &Client, name: &str) -> CommandResult<()> {
    let name = &token_name(name)?;
    match client.github_logout(name).await? {
        Some(token) => print!("{}", forgotten(&token)),
        None => println!("No GitHub token named {name} is stored; nothing changed."),
    }
    Ok(())
}

/// `value` as a token's name, lowercased. Clap would repeat a rejected value, which may be a
/// token pasted in the wrong place, so names are checked here instead.
fn token_name(value: &str) -> Result<String, Error> {
    if agent::github::valid_name(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(Error::Invalid(agent::github::NAME_RULE.into()))
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
        } else if repository.owner.eq_ignore_ascii_case(account) {
            format!("the token cannot see {repository}: it was probably not selected under Repository access")
        } else {
            format!(
                "the token cannot see {repository}: it was not selected, or {} has yet to approve the token",
                repository.owner
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
            "this {} token reaches every repository {account} can",
            token.kind.label()
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

/// What GitHub said about a token: its name, whom it acts as, its kind and expiry, then `warnings`.
fn summary(login: &GithubLogin, warnings: &[String], now: OffsetDateTime) -> String {
    let token = &login.token;
    let account = &token.account;
    let mut text = format!(
        "\n  Name:     {}\n  Acts as:  {} ({} <{}>)\n  Kind:     {}\n  Expires:  {}\n",
        token.name,
        account.login,
        account.name,
        account.email,
        token.kind.label(),
        expiry(token, now)
    );
    for warning in warnings {
        let _ = writeln!(text, "  Warning:  {warning}");
    }
    text
}

fn table(tokens: &[GithubToken], now: OffsetDateTime) -> String {
    if tokens.is_empty() {
        return "No GitHub tokens are stored. Store one with `agentctl github login NAME`.\n".into();
    }
    let rows: Vec<[String; 4]> = tokens
        .iter()
        .map(|token| {
            [
                token.name.clone(),
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
fn expiry(token: &GithubToken, now: OffsetDateTime) -> String {
    match token.expires_at {
        None => "none stated".into(),
        Some(expires_at) if token.expired(now) => format!("{} (expired)", expires_at.date()),
        Some(expires_at) => expires_at.date().to_string(),
    }
}

fn forgotten(token: &GithubToken) -> String {
    format!(
        "Forgot the GitHub token {name}; agents asking for {name} lose GitHub access at once, and nothing else.\n\
         It works on GitHub until it expires or is revoked: sign in as {login} at {page}\n",
        name = token.name,
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

    fn token(name: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>) -> GithubToken {
        GithubToken {
            name: name.into(),
            account: GithubAccount {
                login: format!("{name}-bot"),
                name: format!("{name} bot"),
                email: format!("1+{name}-bot@users.noreply.github.com"),
            },
            kind,
            expires_at,
        }
    }

    fn checked(name: &str, kind: TokenKind, expires_at: Option<OffsetDateTime>, missing: &[&str]) -> GithubLogin {
        GithubLogin {
            token: token(name, kind, expires_at),
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
            "\n  Name:     digdir\n  Acts as:  digdir-bot (digdir bot <1+digdir-bot@users.noreply.github.com>)\n  Kind:     fine-grained\n  Expires:  2027-01-13\n"
        );
        let broad = checked("digdir", TokenKind::Classic, None, &["workflow"]);
        let text = summary(&broad, &warnings(&broad, None, false, january()), january());
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
            warnings(&fine, Some((&case_web, true)), false, long_before),
            Vec::<String>::new()
        );
        assert_eq!(warnings(&fine, None, false, long_before), Vec::<String>::new());

        assert_eq!(
            warnings(&fine, Some((&case_web, false)), false, long_before),
            ["the token cannot see digdir/case-web: it was not selected, or digdir has yet to approve the token"]
        );
        let own = repository("digdir-bot", "notes");
        assert_eq!(
            warnings(&fine, Some((&own, false)), false, long_before),
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
                "this classic token reaches every repository digdir-bot can",
                "the token lacks read:org, so some `gh` and Git commands will fail",
            ]
        );
    }

    #[test]
    fn signing_in_as_an_account_reaches_all_it_can_without_a_warning() {
        let case_web = repository("digdir", "case-web");
        // A classic token stands in for the browser sign-in's: neither is limited to one owner.
        let broad = checked("digdir-bot", TokenKind::Classic, None, &[]);
        assert_eq!(
            warnings(&broad, Some((&case_web, false)), true, january()),
            ["digdir-bot-bot cannot see digdir/case-web; give the account access to it"]
        );
        assert_eq!(
            warnings(&broad, None, false, january()),
            ["this classic token reaches every repository digdir-bot-bot can"]
        );
        assert!(SIGN_IN_OPTIONS[0].starts_with("Sign in through the browser"));
        assert_eq!(
            manifest_line("digdir-agent"),
            "Agents use it with `accounts: [{type: github, name: digdir-agent}]` in their manifest."
        );
    }

    #[test]
    fn the_name_the_agent_here_asks_for_is_offered_until_it_has_a_token_then_the_repositorys_and_folders() {
        let studio = repository("digdir", "altinn-studio");
        let wanted = || Some("app-lib".to_owned());
        assert_eq!(
            suggested(wanted(), &[], Some(&studio), Some("altinn-studio"), january()),
            ["app-lib", "altinn-studio"]
        );
        let expired = token("app-lib", TokenKind::FineGrained, Some(january()));
        assert_eq!(
            suggested(wanted(), std::slice::from_ref(&expired), Some(&studio), None, january()),
            ["app-lib", "altinn-studio"]
        );
        let current = token("app-lib", TokenKind::FineGrained, None);
        assert_eq!(
            suggested(wanted(), &[current], Some(&studio), None, january()),
            ["altinn-studio"]
        );
        assert_eq!(
            suggested(Some("altinn-studio".into()), &[expired], Some(&studio), None, january()),
            ["altinn-studio"]
        );
        // Without a GitHub remote, the folder's name.
        assert_eq!(
            suggested(None, &[], None, Some("Test_Test.test"), january()),
            ["test-test-test"]
        );
        assert_eq!(suggested(None, &[], None, None, january()), Vec::<String>::new());
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
            token("altinn", TokenKind::Other, None),
        ];
        let text = table(&tokens, january());
        assert_eq!(
            text,
            "\
NAME    ACCOUNT     KIND          EXPIRES
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
            let message = crate::error_text(&error);
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
