//! GitHub access for an agent whose definition declares `GITHUB_TOKEN`: a token checked against
//! GitHub on the host, and the identity its commits carry. The agent sees only a placeholder that
//! the network mediator swaps for the token in requests to GitHub's hosts.

use super::answers::{Answers, Choice};
use super::{BrowserSignIn, Draft, GithubChoice, SetupArguments};
use crate::{CommandError, CommandResult};

use agent::{Error, Spec};
use serde::Deserialize;
use zeroize::Zeroizing;

/// The variable the token reaches the agent's `gh` and git through.
pub(super) const TOKEN_VARIABLE: &str = "GITHUB_TOKEN";
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

/// Whether `spec` declares the token, which is what makes an agent reach GitHub.
pub(super) fn token_declared(spec: &Spec) -> bool {
    spec.secrets.iter().any(|secret| secret.source() == TOKEN_VARIABLE)
}

/// How an agent whose definition declares the token gets it: one already known, such as from
/// `--env-file`, the flags, or the person's choice. An agent that declares none goes without.
pub(super) async fn choose_github(
    arguments: &SetupArguments,
    answers: &Answers,
    draft: &mut Draft,
) -> CommandResult<()> {
    if !token_declared(&draft.agent.spec) {
        if arguments.github.is_some_and(|choice| choice != GithubChoice::Skip) {
            return Err(CommandError::Message(format!(
                "this agent's definition does not declare {TOKEN_VARIABLE}, so it does not reach GitHub"
            )));
        }
        return Ok(());
    }
    let required = token_required(&draft.agent.spec);
    if required && arguments.github == Some(GithubChoice::Skip) {
        return Err(CommandError::Message(format!(
            "this agent's definition requires {TOKEN_VARIABLE}, so it cannot go without GitHub; pass --github \
             browser or token"
        )));
    }
    let known = draft.access.known_values.get(TOKEN_VARIABLE).cloned();
    if let (Some(token), None, false) = (known, arguments.github, arguments.github_token_stdin) {
        match record_github(arguments, answers, draft, token, arguments.github_user.as_deref()).await {
            Ok(()) => return Ok(()),
            Err(error) => eprintln!("The GitHub token known for this agent does not work: {error}"),
        }
    }
    let choice = match (arguments.github, arguments.github_token_stdin) {
        (Some(choice), _) => choice,
        (None, true) => GithubChoice::Token,
        (None, false) => ask_github(answers, required)?,
    };
    match choice {
        GithubChoice::Skip => Ok(()),
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
            record_github(arguments, answers, draft, token, arguments.github_user.as_deref()).await
        }
    }
}

/// Asks how the agent reaches GitHub, saying why going without it is not offered when the token
/// is `required`.
fn ask_github(answers: &Answers, required: bool) -> CommandResult<GithubChoice> {
    let choices = github_choices(answers, required);
    if choices.is_empty() {
        return Err(CommandError::Message(format!(
            "this agent's definition requires {TOKEN_VARIABLE}; without a terminal, pass --github token with \
             --github-token-stdin, or its value in --env-file"
        )));
    }
    let question = if required {
        "How should the agent reach GitHub? Its definition requires a token."
    } else {
        "How should the agent reach GitHub?"
    };
    Ok(answers.choose("--github", question, choices, 0)?)
}

/// The ways to reach GitHub on offer: signing in or a token, which need a terminal, then none,
/// unless the token is `required`.
pub(super) fn github_choices(answers: &Answers, required: bool) -> Vec<Choice<GithubChoice>> {
    let skip = (!required).then(|| Choice::new(GithubChoice::Skip, "Not at all, for now").flag("--github", &["skip"]));
    if !answers.interactive() {
        return skip.into_iter().collect();
    }
    let mut choices = vec![
        Choice::new(
            GithubChoice::Browser,
            "Sign in through the browser, as an account made for agents",
        )
        .suggested()
        .flag("--github", &["browser"]),
        Choice::new(GithubChoice::Token, "With a token you create for it").flag("--github", &["token"]),
    ];
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
    if !answers.filling_in() {
        println!("The agent acts on GitHub as {}.", user.login);
    }
    draft.access.github_account = Some(user.login.clone());
    // Done again without questions, the token comes from the values file setup writes, which
    // the repeated command reads with `--env-file`.
    answers.forget("--github");
    answers.forget("--github-token-stdin");
    answers.record("--github-user", &[&user.login]);
    let declared = |name: &str| {
        draft
            .agent
            .spec
            .environment
            .iter()
            .any(|variable| variable.source() == name)
    };
    let identity = [
        (
            GIT_USER_NAME,
            arguments.git_user_name.clone().unwrap_or_else(|| user.commit_name()),
        ),
        (
            GIT_USER_EMAIL,
            arguments.git_user_email.clone().unwrap_or_else(|| user.noreply_email()),
        ),
    ]
    .into_iter()
    .filter(|(name, _)| declared(name))
    .collect::<Vec<_>>();
    draft
        .access
        .values
        .retain(|(name, _)| ![TOKEN_VARIABLE, GIT_USER_NAME, GIT_USER_EMAIL].contains(&name.as_str()));
    draft.access.values.push((TOKEN_VARIABLE.into(), token));
    for (name, value) in identity {
        draft.access.values.push((name.into(), Zeroizing::new(value)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GithubUser, github_choices};
    use crate::setup::{GithubChoice, answers::Answers};

    fn offers_skip(choices: &[crate::setup::answers::Choice<GithubChoice>]) -> bool {
        choices.iter().any(|choice| choice.value == GithubChoice::Skip)
    }

    #[test]
    fn going_without_github_is_offered_only_when_the_token_is_optional() {
        for interactive in [true, false] {
            let answers = Answers::new(interactive, false);
            assert!(offers_skip(&github_choices(&answers, false)));
            assert!(!offers_skip(&github_choices(&answers, true)));
        }
        let script = Answers::new(false, true);
        assert!(github_choices(&script, true).is_empty());
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
