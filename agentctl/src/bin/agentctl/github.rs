//! `agentctl github`: stores and lists the GitHub tokens Agents use, one per owner.

use agent::{
    Error,
    control_api::Client,
    github::{GithubToken, Inspection, Logout, OwnerKind, Repository, TokenKind},
};
use zeroize::Zeroizing;

use crate::{CommandResult, prompt};

/// Where the token comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Source {
    /// A fine-grained token created in the pre-filled GitHub form and pasted.
    Form,
    /// A token read from standard input.
    Stdin,
    /// The host's own `gh` login.
    HostGh,
    /// The GitHub CLI's browser sign-in, kept apart from the host's own login.
    Browser,
}

/// Gets a token from `source`, shows what GitHub says about it, and stores it once confirmed.
///
/// A fine-grained token is kept for `owner`, by default the owner of the current directory's
/// repository. Any other token reaches every owner and is kept for every organization without one of its own, unless `owner`
/// is named. Returns `None` when nothing was stored.
///
/// # Errors
///
/// Returns an error when no token is supplied, GitHub rejects it, or it cannot be stored.
pub(crate) async fn login(
    client: &Client,
    source: Source,
    owner: Option<String>,
) -> CommandResult<Option<GithubToken>> {
    // These errors are about the token or the sign-in, not about an Agent, so they print as is.
    sign_in(client, source, owner).await.map_err(|error| match error {
        crate::CommandError::Agent(Error::Invalid(message)) => crate::CommandError::Message(message),
        error => error,
    })
}

/// One sign-in attempt's outcome.
enum Attempt {
    Stored(GithubToken),
    /// The person turned the token down, GitHub rejected it, or the browser signed in as another
    /// account.
    Declined,
}

async fn sign_in(client: &Client, source: Source, owner: Option<String>) -> CommandResult<Option<GithubToken>> {
    if source != Source::Stdin && !prompt::interactive() {
        return Err(
            Error::Invalid("no terminal to sign in to GitHub on; pass a token with `--from-stdin`".into()).into(),
        );
    }
    // Another attempt can only differ where the person creates or approves a new token.
    let retryable = matches!(source, Source::Form | Source::Browser);
    loop {
        if let Attempt::Stored(token) = attempt(client, source, owner.clone()).await? {
            return Ok(Some(token));
        }
        if !retryable || !prompt::confirm("Sign in again?")? {
            eprintln!("Nothing was stored.");
            return Ok(None);
        }
    }
}

/// Where to get the GitHub CLI, whose browser sign-in agentctl uses.
const GH_CLI: &str = "https://cli.github.com";

async fn attempt(client: &Client, source: Source, owner: Option<String>) -> CommandResult<Attempt> {
    let mut repository = current_repository();
    let interactive = source != Source::Stdin;
    let mut named_owner = owner.map(|owner| owner.to_ascii_lowercase());
    // Naming an owner asks for that owner's fine-grained token; otherwise the person chooses.
    let source = if source == Source::Form && named_owner.is_none() {
        if agent::github::gh_installed() {
            choose_method()?
        } else {
            eprintln!(
                "\nWith the GitHub CLI installed ({GH_CLI}), agentctl could also sign in with the browser instead."
            );
            source
        }
    } else {
        source
    };
    let (token, expected_login) = match source {
        Source::Stdin => (crate::read_token_from_stdin()?, None),
        Source::HostGh => (agent::github::token_from_gh()?, None),
        Source::Browser => {
            let expected = browser_friction()?;
            eprintln!();
            let token = agent::github::token_from_gh_sign_in()?
                .ok_or_else(|| Error::Invalid("`--browser` needs the GitHub CLI, `gh`, on this host".into()))?;
            (token, Some(expected))
        }
        Source::Form => {
            let owner = match named_owner.clone() {
                Some(owner) => owner,
                None => choose_owner(&mut repository)?,
            };
            named_owner = Some(owner.clone());
            (create_in_form(&owner, repository.as_ref()).await?, None)
        }
    };
    let kind = TokenKind::of(&token);
    let owner = if kind.single_owner() {
        Some(
            match named_owner.or_else(|| repository.as_ref().map(|repository| repository.owner.clone())) {
                Some(owner) => owner,
                None if interactive => choose_owner(&mut repository)?,
                None => {
                    return Err(Error::Invalid(
                        "a fine-grained token belongs to one owner; name it with `--owner`".into(),
                    )
                    .into());
                }
            },
        )
    } else {
        named_owner
    };
    let checked = repository
        .as_ref()
        .filter(|repository| owner.as_deref().is_none_or(|owner| owner == repository.owner));
    let inspection = match agent::github::inspect(&token, checked).await {
        Ok(inspection) => inspection,
        // A mistyped or incomplete paste is the likeliest cause, which another attempt can fix.
        Err(Error::Invalid(reason)) if interactive => {
            eprintln!("\n{reason}.");
            return Ok(Attempt::Declined);
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(expected) = expected_login
        && let Err(refused) = signed_in_as(&expected, &inspection.account.login)
    {
        eprintln!("\n{refused}");
        return Ok(Attempt::Declined);
    }
    // A repository the token sees shows the owner is right; otherwise a mistyped owner, or
    // another person's account, leaves the token unusable for its repositories.
    let owner_kind = match owner.as_deref() {
        Some(owner) if !matches!(inspection.repository, Some((_, true))) => {
            agent::github::owner_kind(&token, owner).await.ok()
        }
        _ => None,
    };
    let mut warnings = warnings(&inspection, kind, agent::github::host_login().as_deref());
    if let (Some(owner), Some(owner_kind)) = (owner.as_deref(), owner_kind)
        && let Some(warning) = owner_warning(owner, owner_kind, &inspection.account.login, kind)
    {
        warnings.insert(0, warning);
    }
    eprint!("{}", summary(&inspection, owner.as_deref(), &warnings));
    if interactive {
        let question = format!(
            "Agents will use GitHub as {}. Use this token?",
            inspection.account.login
        );
        if !prompt::confirm(&question)? {
            return Ok(Attempt::Declined);
        }
    }
    let stored = client.github_login(token.to_string(), owner).await?;
    match &stored.owner {
        Some(owner) => println!("Agents in {owner} now use GitHub as {}.", stored.account.login),
        None => println!("Agents now use GitHub as {}.", stored.account.login),
    }
    Ok(Attempt::Stored(stored))
}

/// Forgets a stored token: the one `target` names, or one chosen from a menu.
///
/// # Errors
///
/// Returns an error when there is no terminal to choose or confirm on, or the daemon fails.
pub(crate) async fn logout(client: &Client, target: Option<Target>, yes: bool) -> CommandResult<()> {
    let tokens = client.auth_status().await?.github;
    if tokens.is_empty() {
        println!("No GitHub tokens are stored.");
        return Ok(());
    }
    if let Some(target) = target {
        let target = match target {
            Target::Every => Logout::Everywhere,
            Target::Owner(owner) => Logout::Owner(owner),
            Target::Named(name) => named(&tokens, &name)?,
        };
        return forget(client, &tokens, target, yes, "Log out").await;
    }
    if !prompt::interactive() {
        return Err(Error::Invalid("no terminal to choose on; pass `--owner` or `--everywhere`".into()).into());
    }
    let choices = logout_choices(&tokens);
    let labels: Vec<String> = choices.iter().map(|(label, _)| label.clone()).collect();
    let Some(index) = prompt::choose("Log out of GitHub for agents in:", &labels, None)? else {
        eprintln!("Nothing was changed.");
        return Ok(());
    };
    forget(client, &tokens, choices[index].1.clone(), yes, "Log out").await
}

/// Which token `agentctl github logout` was told to forget.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    /// Every stored token.
    Every,
    /// The token for this owner.
    Owner(String),
    /// The token whose owner, or else whose account, this is, as `agentctl github list` shows.
    Named(String),
}

/// The stored token `name` picks: the one for that owner, else the only one acting as that
/// account.
fn named(tokens: &[GithubToken], name: &str) -> Result<Logout, Error> {
    let lowered = name.to_ascii_lowercase();
    if tokens
        .iter()
        .any(|token| token.owner.as_deref() == Some(lowered.as_str()))
    {
        return Ok(Logout::Owner(lowered));
    }
    let acting: Vec<&GithubToken> = tokens
        .iter()
        .filter(|token| token.account.login.eq_ignore_ascii_case(name))
        .collect();
    match acting.as_slice() {
        [token] => Ok(token.owner.clone().map_or(Logout::AnyOther, Logout::Owner)),
        [] => Err(Error::Invalid(format!(
            "no stored GitHub token is for or acts as {name}; `agentctl github list` shows them"
        ))),
        several => {
            let owners: Vec<&str> = several.iter().map(|token| token.scope()).collect();
            Err(Error::Invalid(format!(
                "{name} acts for several owners ({}); pass `--owner` to pick one",
                owners.join(", ")
            )))
        }
    }
}

/// Forgets every stored token, after a warning.
///
/// # Errors
///
/// Returns an error when there is no terminal to confirm on without `--yes`, or the daemon fails.
pub(crate) async fn reset(client: &Client, yes: bool) -> CommandResult<()> {
    let tokens = client.auth_status().await?.github;
    if tokens.is_empty() {
        println!("No GitHub tokens are stored; there is nothing to reset.");
        return Ok(());
    }
    eprintln!("This forgets every GitHub token agentctl has stored. Agents using them lose GitHub access");
    eprintln!("until you run `agentctl github login` again.");
    forget(client, &tokens, Logout::Everywhere, yes, "Reset").await
}

/// Confirms, then forgets the tokens `target` names, and says what happened.
async fn forget(client: &Client, tokens: &[GithubToken], target: Logout, yes: bool, verb: &str) -> CommandResult<()> {
    let affected: Vec<&GithubToken> = tokens
        .iter()
        .filter(|token| match &target {
            Logout::Owner(owner) => token.owner.as_deref() == Some(owner.as_str()),
            Logout::AnyOther => token.owner.is_none(),
            Logout::Everywhere => true,
        })
        .collect();
    if affected.is_empty() {
        println!("No stored GitHub token matches; nothing was changed.");
        return Ok(());
    }
    eprintln!();
    for token in &affected {
        eprintln!("  {}", describe(token));
    }
    eprintln!();
    if !yes {
        if !prompt::interactive() {
            return Err(Error::Invalid("no terminal to confirm on; pass `--yes`".into()).into());
        }
        let question = format!(
            "{verb}: forget {}?",
            if affected.len() == 1 {
                "this token"
            } else {
                "these tokens"
            }
        );
        if !prompt::confirm(&question)? {
            eprintln!("Nothing was changed.");
            return Ok(());
        }
    }
    let forgotten = client.github_logout(target).await?;
    println!(
        "Forgot {} GitHub token{}. Agents using {} lose GitHub access on their next pass.",
        forgotten.len(),
        if forgotten.len() == 1 { "" } else { "s" },
        if forgotten.len() == 1 { "it" } else { "them" }
    );
    for line in revoke_lines(&forgotten) {
        println!("{line}");
    }
    Ok(())
}

/// How to revoke each forgotten token on GitHub, which forgetting it here does not: on the page
/// for its kind, signed in as its account.
fn revoke_lines(forgotten: &[GithubToken]) -> Vec<String> {
    let mut lines = Vec::new();
    for token in forgotten {
        let Some(line) = token.kind.revoke_hint(&token.account.login) else {
            continue;
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// The menu `logout` offers: one entry per stored token, then everywhere.
fn logout_choices(tokens: &[GithubToken]) -> Vec<(String, Logout)> {
    tokens
        .iter()
        .map(|token| {
            let target = token.owner.clone().map_or(Logout::AnyOther, Logout::Owner);
            (describe(token), target)
        })
        .chain(std::iter::once((
            "Everywhere: every token above".to_owned(),
            Logout::Everywhere,
        )))
        .collect()
}

/// A token in words: where it is used, whom it acts as, and when it expires.
fn describe(token: &GithubToken) -> String {
    let place = token.owner.as_deref().map_or_else(
        || capitalized(agent::github::EVERY_OTHER_ORGANIZATION),
        ToOwned::to_owned,
    );
    format!(
        "{place}: acts as {} ({} token, expires {})",
        token.account.login,
        token.kind.label(),
        expiry(token, time::OffsetDateTime::now_utc())
    )
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// Lists the stored tokens: owner, account, kind and expiry.
///
/// # Errors
///
/// Returns an error when the daemon cannot be reached.
pub(crate) async fn list(client: &Client) -> CommandResult<()> {
    let tokens = client.auth_status().await?.github;
    if tokens.is_empty() {
        println!("No GitHub tokens. Run `agentctl github login` in a repository.");
        return Ok(());
    }
    let now = time::OffsetDateTime::now_utc();
    println!("{:<20} {:<24} {:<13} EXPIRES", "OWNER", "ACCOUNT", "KIND");
    for token in tokens {
        println!(
            "{:<20} {:<24} {:<13} {}",
            token.owner.as_deref().unwrap_or("(all others)"),
            token.account.login,
            token.kind.label(),
            expiry(&token, now)
        );
    }
    Ok(())
}

fn expiry(token: &GithubToken, now: time::OffsetDateTime) -> String {
    match token.expires_at {
        None => token.kind.unstated_expiry().into(),
        Some(expires_at) if token.expired(now) => format!("{} (expired)", expires_at.date()),
        Some(expires_at) if token.expires_soon(now) => format!("{} (soon)", expires_at.date()),
        Some(expires_at) => expires_at.date().to_string(),
    }
}

/// The GitHub repository of the current directory's `origin` remote, if it is on GitHub.
pub(crate) fn current_repository() -> Option<Repository> {
    if !crate::host::git_available() {
        return None;
    }
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| repository_from_remotes(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

/// The GitHub repository `git remote -v` lists: `origin` when it is on GitHub, else the first
/// remote that is.
pub(crate) fn repository_from_remotes(remotes: &str) -> Option<Repository> {
    let mut found = remotes.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let name = fields.next()?;
        Repository::from_remote(fields.next()?).map(|repository| (name == "origin", repository))
    });
    let first = found.next()?;
    if first.0 {
        return Some(first.1);
    }
    found
        .find(|(origin, _)| *origin)
        .map_or(Some(first.1), |(_, repository)| Some(repository))
}

/// Asks whether to create a fine-grained token or sign in through the GitHub CLI's browser flow.
fn choose_method() -> Result<Source, Error> {
    let options = [
        "Create a fine-grained token: only the repositories you pick (recommended)".to_owned(),
        "Sign in with the browser through the GitHub CLI: every repository the account can reach".to_owned(),
    ];
    Ok(
        match prompt::choose("How should agents sign in to GitHub?", &options, Some(0))? {
            Some(1) => Source::Browser,
            _ => Source::Form,
        },
    )
}

/// The organization or user to set up access for: this repository's owner when the person agrees,
/// otherwise one they name or paste a GitHub address of. A pasted repository address becomes the
/// repository the instructions and the access check use.
fn choose_owner(repository: &mut Option<Repository>) -> Result<String, Error> {
    if let Some(found) = repository.as_ref()
        && prompt::confirm(&format!(
            "This repository is {found} on GitHub. Set up access for agents working in {}?",
            found.owner
        ))?
    {
        return Ok(found.owner.clone());
    }
    let mut question = "Which GitHub organization or user should agents work in? Type a name or paste a repository URL";
    loop {
        let answer = prompt::ask(question)?;
        question = "Organization, user or repository URL";
        match owner_from_answer(&answer) {
            Some((owner, named)) => {
                if named.is_some() || repository.as_ref().is_some_and(|found| found.owner != owner) {
                    *repository = named;
                }
                return Ok(owner);
            }
            None => eprintln!(
                "Type a name such as digdir, or paste an address such as https://github.com/digdir/digdir-agents."
            ),
        }
    }
}

/// The owner, and the repository when there is one, that an answer names: a plain name, or a
/// GitHub address of the owner or of one of its repositories.
fn owner_from_answer(answer: &str) -> Option<(String, Option<Repository>)> {
    let answer = answer.trim().trim_end_matches('/');
    if let Some(repository) =
        Repository::from_remote(answer).or_else(|| Repository::from_remote(&format!("https://{answer}")))
    {
        return Some((repository.owner.clone(), Some(repository)));
    }
    let name = answer
        .strip_prefix("https://")
        .or_else(|| answer.strip_prefix("http://"))
        .unwrap_or(answer);
    let name = name.strip_prefix("github.com/").unwrap_or(name);
    agent::github::valid_owner(name).then(|| (name.to_ascii_lowercase(), None))
}

/// Walks through creating a fine-grained token in GitHub's pre-filled form, and reads it.
async fn create_in_form(owner: &str, repository: Option<&Repository>) -> Result<Zeroizing<String>, Error> {
    let repository_name = repository
        .filter(|repository| repository.owner == owner)
        .map(|repository| repository.name.as_str());
    let name = format!("agentctl {}", repository_name.unwrap_or(owner));
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(hostname_command)
        .unwrap_or_else(|| "this host".into());
    let description = format!(
        "Agents on {host}, created by agentctl on {}",
        time::OffsetDateTime::now_utc().date()
    );
    let url = agent::github::token_form_url(owner, &name, &description);
    eprintln!();
    eprintln!("Agents need a fine-grained GitHub token for {owner}.");
    eprintln!();
    eprintln!("  1. Sign in to github.com as the account agents should act as, such as a bot account.");
    eprintln!("     A private browser window keeps it apart from your own account.");
    eprintln!("  2. Open the form below. Name, owner, lifetime and permissions are filled in.");
    eprintln!(
        "  3. Under Repository access, choose \"Only select repositories\" and pick {}.",
        repository_name.map_or_else(
            || "the repositories agents work on".to_owned(),
            |name| format!("{owner}/{name}")
        )
    );
    eprintln!("  4. Generate the token and copy it.");
    eprintln!();
    eprintln!("  {url}");
    if crate::launch::launch_blocked(|name| std::env::var_os(name)).is_none() {
        prompt::wait_for_enter("Press Enter to open the form in your browser…")?;
        if let Err(error) = crate::launch::open_url(&url).await {
            eprintln!("Could not open the browser ({error}); open the address above instead.");
        }
    }
    prompt::secret("Paste the token")
}

fn hostname_command() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// The friction before a browser sign-in: it yields a token that reaches every repository the
/// account can, so the person names the account first and a different one is refused.
fn browser_friction() -> Result<String, Error> {
    let expected = prompt::ask_lines(&[
        "Which GitHub account should agents act as?",
        "Sign in to github.com as that account in your browser first. If the browser approves",
        "a different account, agentctl refuses it.",
        "Account name",
    ])?;
    if expected.is_empty() {
        return Err(Error::Invalid("no account was named; nothing was signed in".into()));
    }
    Ok(expected)
}

/// Refuses a browser sign-in as any account but the one the person named beforehand.
fn signed_in_as(expected: &str, actual: &str) -> Result<(), Error> {
    if expected.trim_start_matches('@').eq_ignore_ascii_case(actual) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "The browser signed in as {actual}, not {expected}, so this token is not used."
        )))
    }
}

/// A warning when the token cannot act on `owner`'s repositories: there is no such owner, or it
/// is another person's account, which a fine-grained token of `login` cannot reach.
fn owner_warning(owner: &str, owner_kind: Option<OwnerKind>, login: &str, kind: TokenKind) -> Option<String> {
    match owner_kind {
        None => Some(format!(
            "GitHub has no user or organization named {owner}, so no agent will use this token"
        )),
        Some(OwnerKind::User) if kind == TokenKind::FineGrained && !owner.eq_ignore_ascii_case(login) => Some(format!(
            "{owner} is a personal account, not an organization, and a fine-grained token of {login} cannot act on its repositories"
        )),
        Some(_) => None,
    }
}

/// Soft warnings about a token, each one line.
fn warnings(inspection: &Inspection, kind: TokenKind, host_login: Option<&str>) -> Vec<String> {
    let login = &inspection.account.login;
    let mut warnings = Vec::new();
    if host_login.is_some_and(|host| host.eq_ignore_ascii_case(login)) {
        warnings.push(format!("{login} is also your own `gh` login on this host"));
    }
    if !agent::github::looks_like_automation(login) {
        warnings.push(format!(
            "{login} is not named like an automation account (…-bot, …-agent)"
        ));
    }
    if !kind.single_owner() {
        warnings.push(format!(
            "this {} token reaches every repository {login} can, not selected ones",
            kind.label()
        ));
    }
    if let Some((repository, false)) = &inspection.repository {
        warnings.push(format!(
            "the token cannot see {repository}: it was not selected, or the organization has not approved the token yet"
        ));
    }
    if !inspection.missing_scopes.is_empty() {
        warnings.push(format!(
            "the token lacks {}, so some `gh` and Git commands will fail",
            inspection.missing_scopes.join(", ")
        ));
    }
    let now = time::OffsetDateTime::now_utc();
    if let Some(expires_at) = inspection
        .expires_at
        .filter(|expires_at| *expires_at <= now + agent::github::EXPIRY_WARNING)
    {
        warnings.push(format!("the token expires on {}", expires_at.date()));
    }
    warnings
}

/// What the token is, as a short block, followed by its warnings.
fn summary(inspection: &Inspection, owner: Option<&str>, warnings: &[String]) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "  Acts as:   {} ({} <{}>)",
        inspection.account.login, inspection.account.name, inspection.account.email
    );
    let _ = writeln!(text, "  Kind:      {}", inspection.kind.label());
    let _ = writeln!(
        text,
        "  For:       {}",
        owner.unwrap_or(agent::github::EVERY_OTHER_ORGANIZATION)
    );
    if let Some((repository, visible)) = &inspection.repository {
        let _ = writeln!(
            text,
            "  Can see:   {repository} {}",
            if *visible { "yes" } else { "no" }
        );
    }
    let _ = writeln!(
        text,
        "  Expires:   {}",
        inspection.expires_at.map_or_else(
            || inspection.kind.unstated_expiry().to_owned(),
            |expires_at| expires_at.date().to_string()
        )
    );
    for warning in warnings {
        let _ = writeln!(text, "  ⚠ {warning}");
    }
    text
}

#[cfg(test)]
mod tests {
    use agent::github::GithubAccount;

    use super::*;

    fn inspection(login: &str, kind: TokenKind, repository: Option<bool>) -> Inspection {
        Inspection {
            account: GithubAccount {
                login: login.into(),
                name: "Name".into(),
                email: "name@example.com".into(),
            },
            kind,
            expires_at: None,
            missing_scopes: Vec::new(),
            repository: repository.map(|visible| (Repository::new("digdir", "agents"), visible)),
        }
    }

    #[test]
    fn a_fine_grained_bot_token_that_sees_its_repository_raises_no_warning() {
        let checked = inspection("digdir-bot", TokenKind::FineGrained, Some(true));
        assert!(warnings(&checked, TokenKind::FineGrained, Some("danielskovli")).is_empty());
    }

    #[test]
    fn a_personal_broad_token_that_misses_its_repository_warns_of_each() {
        let checked = inspection("danielskovli", TokenKind::Classic, Some(false));
        let warnings = warnings(&checked, TokenKind::Classic, Some("DanielSkovli"));
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings[0].contains("your own `gh` login"), "{warnings:?}");
        assert!(warnings[1].contains("automation account"), "{warnings:?}");
        assert!(warnings[2].contains("every repository"), "{warnings:?}");
        assert!(warnings[3].contains("cannot see digdir/agents"), "{warnings:?}");
    }

    fn token(owner: Option<&str>, login: &str) -> GithubToken {
        GithubToken {
            owner: owner.map(str::to_owned),
            account: GithubAccount {
                login: login.into(),
                name: login.into(),
                email: format!("{login}@example.com"),
            },
            kind: if owner.is_some() {
                TokenKind::FineGrained
            } else {
                TokenKind::Classic
            },
            expires_at: None,
        }
    }

    #[test]
    fn logout_offers_each_token_in_words_and_then_everywhere() {
        let choices = logout_choices(&[token(Some("digdir"), "digdir-bot"), token(None, "digdir-bot")]);
        let labels: Vec<&str> = choices.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "digdir: acts as digdir-bot (fine-grained token, expires unknown)",
                "Every organization without a token of its own: acts as digdir-bot (classic token, expires never)",
                "Everywhere: every token above",
            ]
        );
        assert_eq!(choices[0].1, Logout::Owner("digdir".into()));
        assert_eq!(choices[1].1, Logout::AnyOther);
        assert_eq!(choices[2].1, Logout::Everywhere);
    }

    #[test]
    fn the_repository_is_origin_on_github_else_the_first_remote_on_github() {
        let remotes = "upstream\thttps://github.com/digdir/upstream.git (fetch)\n\
                       upstream\thttps://github.com/digdir/upstream.git (push)\n\
                       origin\tgit@github.com:danielskovli/fork.git (fetch)\n";
        assert_eq!(
            repository_from_remotes(remotes),
            Some(Repository::new("danielskovli", "fork"))
        );
        let without_origin =
            "gitlab\thttps://gitlab.com/a/b.git (fetch)\nmine\thttps://github.com/Digdir/agents (fetch)\n";
        assert_eq!(
            repository_from_remotes(without_origin),
            Some(Repository::new("digdir", "agents"))
        );
        assert_eq!(
            repository_from_remotes("origin\thttps://gitlab.com/a/b.git (fetch)\n"),
            None
        );
        assert_eq!(repository_from_remotes(""), None);
    }

    #[test]
    fn an_owner_is_a_name_or_a_github_address_of_it_or_its_repositories() {
        let repository = Some(Repository::new("digdir", "digdir-agents"));
        for answer in [
            "https://github.com/digdir/digdir-agents",
            "https://github.com/Digdir/digdir-agents.git/",
            "github.com/digdir/digdir-agents",
            "git@github.com:digdir/digdir-agents.git",
        ] {
            assert_eq!(
                owner_from_answer(answer),
                Some(("digdir".to_owned(), repository.clone())),
                "{answer}"
            );
        }
        for answer in ["digdir", " Digdir ", "https://github.com/digdir", "github.com/digdir/"] {
            assert_eq!(owner_from_answer(answer), Some(("digdir".to_owned(), None)), "{answer}");
        }
        for answer in ["", "https://gitlab.com/digdir/agents", "not an owner", "-digdir"] {
            assert_eq!(owner_from_answer(answer), None, "{answer:?}");
        }
    }

    #[test]
    fn a_browser_sign_in_as_another_account_is_refused() {
        signed_in_as("digdir-bot", "Digdir-Bot").expect("the named account, in any case");
        signed_in_as("@digdir-bot", "digdir-bot").expect("with a leading @");
        let refused = signed_in_as("digdir-bot", "danielskovli").expect_err("another account");
        assert!(
            refused
                .to_string()
                .contains("signed in as danielskovli, not digdir-bot"),
            "{refused}"
        );
    }

    #[test]
    fn the_summary_names_the_account_owner_access_and_expiry() {
        let mut checked = inspection("digdir-bot", TokenKind::FineGrained, Some(true));
        checked.expires_at = time::OffsetDateTime::from_unix_timestamp(1_799_834_400).ok();
        let text = summary(&checked, Some("digdir"), &["a warning".into()]);
        for expected in [
            "Acts as:   digdir-bot (Name <name@example.com>)",
            "Kind:      fine-grained",
            "For:       digdir",
            "Can see:   digdir/agents yes",
            "Expires:   2027-01-13",
            "⚠ a warning",
        ] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
    }

    #[test]
    fn an_owner_the_token_cannot_reach_is_called_out() {
        let fine = TokenKind::FineGrained;
        assert!(owner_warning("digdir", Some(OwnerKind::Organization), "digdir-bot", fine).is_none());
        assert!(
            owner_warning("DigdirBot", Some(OwnerKind::User), "digdirbot", fine).is_none(),
            "its own account"
        );
        let other = owner_warning("doesntexist", Some(OwnerKind::User), "danielskovli", fine).expect("warned");
        assert!(other.contains("personal account"), "{other}");
        assert!(
            owner_warning("doesntexist", Some(OwnerKind::User), "danielskovli", TokenKind::Classic).is_none(),
            "a classic token reaches repositories it collaborates on"
        );
        let missing = owner_warning("nobody", None, "danielskovli", fine).expect("warned");
        assert!(missing.contains("no user or organization named nobody"), "{missing}");
    }

    #[test]
    fn logout_names_a_token_by_its_owner_or_its_account() {
        let tokens = [
            token(Some("digdir"), "digdir-agent"),
            token(Some("altinn"), "shared-bot"),
            token(Some("ssb"), "shared-bot"),
            token(None, "danielskovli-agent"),
        ];
        assert!(matches!(named(&tokens, "Digdir"), Ok(Logout::Owner(owner)) if owner == "digdir"));
        assert!(matches!(named(&tokens, "digdir-agent"), Ok(Logout::Owner(owner)) if owner == "digdir"));
        assert!(matches!(named(&tokens, "danielskovli-agent"), Ok(Logout::AnyOther)));
        let several = named(&tokens, "shared-bot").expect_err("acts for two owners");
        assert!(several.to_string().contains("altinn, ssb"), "{several}");
        named(&tokens, "nobody").expect_err("no such token");
    }

    #[test]
    fn each_forgotten_access_token_is_revoked_on_the_page_for_its_kind() {
        let signed_in = GithubToken {
            kind: TokenKind::of("gho_signed_in"),
            ..token(None, "danielskovli-agent")
        };
        let lines = revoke_lines(&[
            signed_in,
            token(Some("digdir"), "digdir-bot"),
            token(Some("ssb"), "digdir-bot"),
        ]);
        assert_eq!(
            lines.len(),
            1,
            "nothing for the sign-in's token, one line per kind and account: {lines:?}"
        );
        assert!(
            lines[0].contains("settings/personal-access-tokens") && lines[0].contains("digdir-bot"),
            "{lines:?}"
        );
        let classic = revoke_lines(&[token(None, "someone")]);
        assert!(classic[0].contains("settings/tokens"), "{classic:?}");
    }
}
