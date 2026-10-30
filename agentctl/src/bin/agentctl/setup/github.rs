//! GitHub access for a new agent: a token checked against GitHub on the host, and the identity
//! its commits carry. The agent sees only a placeholder that the network mediator swaps for the
//! token in requests to GitHub's hosts.

use super::answers::{Answers, Choice};
use super::{BrowserSignIn, Draft, GithubChoice, SetupArguments, existing};
use crate::{CommandError, CommandResult};

use std::path::Path;

use agent::{EnvironmentSpec, Error, SecretSpec, Spec};
use serde::Deserialize;
use zeroize::Zeroizing;

/// The variable the token reaches the agent's `gh` and git through.
pub(super) const TOKEN_VARIABLE: &str = "GITHUB_TOKEN";
/// `gh` attaches only a token it recognises by prefix, so the inert placeholder carries the
/// fine-grained token prefix.
const TOKEN_PLACEHOLDER: &str = "github_pat_AGENT_MEDIATED_GITHUB_TOKEN";
const TOKEN_HOSTS: [&str; 4] = ["github.com", "api.github.com", "gist.github.com", "uploads.github.com"];
pub(super) const GIT_USER_NAME: &str = "GIT_USER_NAME";
pub(super) const GIT_USER_EMAIL: &str = "GIT_USER_EMAIL";

/// Who a token acts as.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct GithubUser {
    pub(super) login: String,
    pub(super) id: u64,
    #[serde(default)]
    pub(super) name: Option<String>,
}

impl GithubUser {
    /// The name commits carry: the profile name, else the login.
    pub(super) fn commit_name(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.login.clone())
    }

    /// GitHub's private commit address for the account, which links commits to it without
    /// publishing an address.
    pub(super) fn noreply_email(&self) -> String {
        format!("{}+{}@users.noreply.github.com", self.id, self.login)
    }
}

/// A GitHub account another agent in ~/agents uses, with the token in its `.env`.
pub(super) struct KnownAccount {
    pub(super) login: String,
    /// The agent it was found with.
    pub(super) agent: String,
    pub(super) token: Zeroizing<String>,
}

/// The GitHub accounts `agents`, by name and folder, use, one each, with tokens GitHub still
/// accepts. Each agent keeps its token in its own `.env`, so another agent can only reuse a login
/// by copying it.
pub(super) async fn known_accounts(agents: &[(&str, &Path)]) -> Vec<KnownAccount> {
    let mut tokens: Vec<(&str, Zeroizing<String>)> = Vec::new();
    for (agent, folder) in agents {
        let Ok(values) = agent::environment::read_or_empty(&folder.join(agent::control_plane::ENV_FILE)).await else {
            continue;
        };
        if let Some(token) = values.get(TOKEN_VARIABLE).filter(|token| !token.is_empty())
            && !tokens.iter().any(|(_, known)| known == token)
        {
            tokens.push((agent, token.clone()));
        }
    }
    let users = futures_util::future::join_all(tokens.iter().map(|(_, token)| verify(token))).await;
    let mut accounts: Vec<KnownAccount> = Vec::new();
    for ((agent, token), user) in tokens.into_iter().zip(users) {
        if let Ok(user) = user
            && !accounts
                .iter()
                .any(|account| account.login.eq_ignore_ascii_case(&user.login))
        {
            accounts.push(KnownAccount {
                login: user.login,
                agent: agent.to_owned(),
                token,
            });
        }
    }
    accounts
}

/// Asks GitHub who `token` acts as, which also proves it works.
pub(super) async fn verify(token: &str) -> Result<GithubUser, Error> {
    let response = reqwest::Client::new()
        .get("https://api.github.com/user")
        .header(
            reqwest::header::USER_AGENT,
            concat!("agentctl/", env!("CARGO_PKG_VERSION")),
        )
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| Error::Invalid(format!("could not reach GitHub to check the token: {error}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::Invalid(
            "GitHub rejected the token: it is wrong, expired or revoked".into(),
        ));
    }
    if !status.is_success() {
        return Err(Error::Invalid(format!(
            "GitHub answered {status} when checking the token"
        )));
    }
    response
        .json()
        .await
        .map_err(|error| Error::Invalid(format!("GitHub's answer could not be read: {error}")))
}

/// Whether `spec` cannot do without the token: agentd refuses to create an agent whose required
/// secret has no value, so going without GitHub is no answer for it.
pub(super) fn token_required(spec: &Spec) -> bool {
    spec.secrets
        .iter()
        .any(|secret| secret.source() == TOKEN_VARIABLE && !secret.optional)
}

/// Declares the token secret and the git identity in `spec`, unless the manifest already does.
pub(super) fn declare(spec: &mut Spec) {
    if !spec.secrets.iter().any(|secret| secret.environment == TOKEN_VARIABLE) {
        spec.secrets.push(SecretSpec {
            environment: TOKEN_VARIABLE.into(),
            optional: false,
            placeholder: Some(TOKEN_PLACEHOLDER.into()),
            allowed_hosts: TOKEN_HOSTS.into_iter().map(str::to_owned).collect(),
            source: None,
        });
    }
    declare_identity(spec);
}

/// Declares `GIT_USER_NAME` and `GIT_USER_EMAIL`, which agentd requires together.
pub(super) fn declare_identity(spec: &mut Spec) {
    for name in [GIT_USER_NAME, GIT_USER_EMAIL] {
        if !spec.environment.iter().any(|variable| variable.name == name) {
            spec.environment.push(EnvironmentSpec {
                name: name.into(),
                source: None,
            });
        }
    }
}

/// Where a person creates a token the agent can use.
pub(super) const TOKEN_ADVICE: &str = "Create a fine-grained token at https://github.com/settings/personal-access-tokens/new \
     with Contents, Pull requests and Workflows set to read and write, for the repositories the agent works on. \
     A token of an account made for agents keeps your own access out of the agent.";

/// A token read from standard input, without its line ending.
pub(super) fn token_from_stdin() -> Result<Zeroizing<String>, Error> {
    let mut line = Zeroizing::new(String::new());
    std::io::stdin().read_line(&mut line)?;
    let token = Zeroizing::new(line.trim().to_owned());
    if token.is_empty() {
        return Err(Error::Invalid("no GitHub token was given on standard input".into()));
    }
    Ok(token)
}

/// How the agent reaches GitHub, as chosen.
pub(super) enum GithubAnswer {
    Choice(GithubChoice),
    /// One of the accounts other agents use, by index.
    Known(usize),
}

/// Gives the Git identity from its flags to an agent that declares it, with or without a GitHub
/// sign-in, unless that sign-in gave it already.
pub(super) fn give_identity(arguments: &SetupArguments, draft: &mut Draft) {
    for (name, value) in [
        (GIT_USER_NAME, &arguments.git_user_name),
        (GIT_USER_EMAIL, &arguments.git_user_email),
    ] {
        let declared = draft
            .agent
            .spec
            .environment
            .iter()
            .any(|variable| variable.source() == name);
        let given = draft.access.values.iter().any(|(given, _)| given == name);
        if let (Some(value), true, false) = (value, declared, given) {
            draft
                .access
                .values
                .push((name.to_owned(), Zeroizing::new(value.clone())));
        }
    }
}

pub(super) async fn choose_github(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    found: &[existing::Existing],
) -> CommandResult<()> {
    let required = token_required(&draft.agent.spec);
    if required && arguments.github == Some(GithubChoice::Skip) {
        return Err(CommandError::Message(format!(
            "this agent's definition requires {TOKEN_VARIABLE}, so it cannot go without GitHub; pass --github \
             browser, token or existing"
        )));
    }
    let reused = draft.access.known_values.get(TOKEN_VARIABLE).cloned();
    if let (Some(token), None, false) = (reused, arguments.github, arguments.github_token_stdin) {
        match record_github(arguments, answers, draft, token, None).await {
            Ok(()) => return Ok(()),
            Err(error) => eprintln!("The GitHub token of the agent being replaced does not work: {error}"),
        }
    }
    // Without a terminal, another agent's account is used only when `--github existing` asks.
    let offered = match arguments.github {
        Some(GithubChoice::Existing) => true,
        None => answers.interactive() && !arguments.github_token_stdin,
        Some(_) => false,
    };
    let mut known = if offered {
        let agents: Vec<_> = found
            .iter()
            .map(|existing| (existing.name.as_str(), existing.folder.as_path()))
            .collect();
        known_accounts(&agents).await
    } else {
        Vec::new()
    };
    let answer = match (arguments.github, arguments.github_token_stdin) {
        (Some(GithubChoice::Existing), _) => {
            let index = known
                .iter()
                .position(|account| {
                    arguments
                        .github_user
                        .as_ref()
                        .is_none_or(|user| account.login.eq_ignore_ascii_case(user))
                })
                .ok_or_else(|| {
                    CommandError::Message(arguments.github_user.as_ref().map_or_else(
                        || "no agent in ~/agents has a working GitHub token".to_owned(),
                        |user| format!("no agent in ~/agents acts on GitHub as {user} with a working token"),
                    ))
                })?;
            GithubAnswer::Known(index)
        }
        (Some(choice), _) => GithubAnswer::Choice(choice),
        (None, true) => GithubAnswer::Choice(GithubChoice::Token),
        (None, false) => ask_github(answers, &known, required)?,
    };
    let choice = match answer {
        GithubAnswer::Known(index) => {
            let account = known.swap_remove(index);
            return record_github(arguments, answers, draft, account.token, None).await;
        }
        GithubAnswer::Choice(choice) => choice,
    };
    match choice {
        // An existing account is answered above.
        GithubChoice::Skip | GithubChoice::Existing => Ok(()),
        GithubChoice::Browser => {
            let account = match &arguments.github_user {
                Some(account) => account.clone(),
                None => answers.text(
                    "--github-user",
                    "GitHub account to sign in as (Enter for whichever your browser uses)",
                    Some(""),
                    |_| Ok(()),
                )?,
            };
            declare(&mut draft.agent.spec);
            draft.access.github_browser = Some(BrowserSignIn {
                account: Some(account.trim().to_owned()).filter(|account| !account.is_empty()),
            });
            Ok(())
        }
        GithubChoice::Token => {
            let token = if arguments.github_token_stdin {
                token_from_stdin()?
            } else {
                eprintln!("\n{TOKEN_ADVICE}");
                answers.secret("--github-token-stdin", &["GitHub token"])?
            };
            record_github(arguments, answers, draft, token, None).await
        }
    }
}

/// Asks how the agent reaches GitHub, saying why going without it is not offered when the token
/// is `required`.
fn ask_github(answers: &Answers, known: &[KnownAccount], required: bool) -> CommandResult<GithubAnswer> {
    let choices = github_choices(answers, known, required);
    if choices.is_empty() {
        return Err(CommandError::Message(format!(
            "this agent's definition requires {TOKEN_VARIABLE}; without a terminal, pass --github token with \
             --github-token-stdin, or --github existing"
        )));
    }
    let question = if required {
        "How should the agent reach GitHub? Its definition requires a token."
    } else {
        "How should the agent reach GitHub?"
    };
    Ok(answers.choose("--github", question, choices, 0)?)
}

/// The ways to reach GitHub on offer: the accounts other agents use first, as signing in again
/// is what people would rather skip, then signing in or a token, which need a terminal, then none,
/// unless the token is `required`. Without a terminal, none comes first, so a script reuses an
/// account only with `--github existing`.
pub(super) fn github_choices(answers: &Answers, known: &[KnownAccount], required: bool) -> Vec<Choice<GithubAnswer>> {
    let skip = (!required).then(|| {
        Choice::new(GithubAnswer::Choice(GithubChoice::Skip), "Not at all, for now").flag("--github", &["skip"])
    });
    let accounts = known.iter().enumerate().map(|(index, account)| {
        Choice::new(
            GithubAnswer::Known(index),
            format!("As {}, the account agent {} uses", account.login, account.agent),
        )
        .flag("--github", &["existing"])
        .flag("--github-user", &[&account.login])
    });
    if !answers.interactive() {
        return skip.into_iter().chain(accounts).collect();
    }
    let mut choices: Vec<_> = accounts
        .enumerate()
        .map(|(index, choice)| if index == 0 { choice.suggested() } else { choice })
        .collect();
    let browser = Choice::new(
        GithubAnswer::Choice(GithubChoice::Browser),
        "Sign in through the browser, as an account made for agents",
    );
    let browser = if choices.is_empty() {
        browser.suggested()
    } else {
        browser
    };
    choices.push(browser.flag("--github", &["browser"]));
    choices.push(
        Choice::new(
            GithubAnswer::Choice(GithubChoice::Token),
            "With a token you create for it",
        )
        .flag("--github", &["token"]),
    );
    choices.extend(skip);
    choices
}

/// Checks a GitHub token, as `expected` when one is named, and keeps it and the identity its
/// commits carry for the agent's `.env`.
pub(super) async fn record_github(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
    token: Zeroizing<String>,
    expected: Option<&str>,
) -> CommandResult<()> {
    let user = verify(&token).await?;
    if let Some(expected) = expected
        && !user.login.eq_ignore_ascii_case(expected)
    {
        return Err(CommandError::Message(format!(
            "the browser signed in to GitHub as {}, not {expected}; sign out of GitHub there or use a private window, \
             and run setup again",
            user.login
        )));
    }
    println!("The agent acts on GitHub as {}.", user.login);
    draft.access.github_account = Some(user.login.clone());
    // Done again without questions, the token comes from this agent's .env, which a browser
    // sign-in or a pasted token cannot give.
    answers.record("--github", &["existing"]);
    answers.record("--github-user", &[&user.login]);
    answers.forget("--github-token-stdin");
    declare(&mut draft.agent.spec);
    let name = arguments.git_user_name.clone().unwrap_or_else(|| user.commit_name());
    let email = arguments.git_user_email.clone().unwrap_or_else(|| user.noreply_email());
    draft
        .access
        .values
        .retain(|(name, _)| ![TOKEN_VARIABLE, GIT_USER_NAME, GIT_USER_EMAIL].contains(&name.as_str()));
    draft.access.values.push((TOKEN_VARIABLE.into(), token));
    draft.access.values.push((GIT_USER_NAME.into(), Zeroizing::new(name)));
    draft.access.values.push((GIT_USER_EMAIL.into(), Zeroizing::new(email)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GithubAnswer, GithubUser, github_choices};
    use crate::setup::{GithubChoice, answers::Answers};

    fn offers_skip(choices: &[crate::setup::answers::Choice<GithubAnswer>]) -> bool {
        choices
            .iter()
            .any(|choice| matches!(choice.value, GithubAnswer::Choice(GithubChoice::Skip)))
    }

    #[test]
    fn going_without_github_is_offered_only_when_the_token_is_optional() {
        for interactive in [true, false] {
            let answers = Answers::new(interactive, false);
            assert!(offers_skip(&github_choices(&answers, &[], false)));
            assert!(!offers_skip(&github_choices(&answers, &[], true)));
        }
        let script = Answers::new(false, true);
        assert!(github_choices(&script, &[], true).is_empty());
    }

    #[test]
    fn commits_carry_the_profile_name_and_the_private_address() {
        let user = GithubUser {
            login: "digdir-bot".into(),
            id: 42,
            name: Some("Digdir Bot".into()),
        };
        assert_eq!(user.commit_name(), "Digdir Bot");
        assert_eq!(user.noreply_email(), "42+digdir-bot@users.noreply.github.com");
        let unnamed = GithubUser {
            name: Some(" ".into()),
            ..user
        };
        assert_eq!(unnamed.commit_name(), "digdir-bot");
    }
}
