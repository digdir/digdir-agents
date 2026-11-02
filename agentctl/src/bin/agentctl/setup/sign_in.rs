//! Which logins an agent needs, and signing in to them in a short-lived agent, through
//! [`crate::sign_in`], so this machine needs neither the harnesses nor `gh`.

use super::answers::{Answers, Choice};
use super::github::record_github;
use super::{BrowserSignIn, Draft, SetupArguments, SignInChoice};
use crate::sign_in::{SignInAgent, Staging};
use crate::{CommandError, CommandResult};

use std::collections::BTreeMap;

use agent::{
    Agent, Error, Harness, HarnessAuthMode,
    control_api::Client,
    harness::{LoginName, LoginSet},
    local::home::ControlPlaneHome,
};

/// The logins the harnesses of `agent` cannot start without: its default harness, and the others
/// it does not mark optional.
pub(super) struct Logins {
    /// Those agentd does not hold.
    pub(super) missing: Vec<Harness>,
    /// Those agentd holds already.
    pub(super) stored: Vec<Harness>,
}

/// The names of the logins agentd holds, by harness.
async fn stored_logins(client: &Client) -> Result<BTreeMap<Harness, Vec<LoginName>>, Error> {
    let mut stored: BTreeMap<Harness, Vec<LoginName>> = BTreeMap::new();
    for login in client.auth_logins().await? {
        stored.entry(login.harness).or_default().push(login.login);
    }
    Ok(stored)
}

/// Which logins `agent` needs, and which of them agentd holds: a harness has one when its login
/// set can choose among the logins stored for it.
pub(super) async fn logins(client: &Client, agent: &Agent) -> Result<Logins, Error> {
    let stored = stored_logins(client).await?;
    let (held, missing): (Vec<_>, Vec<_>) = agent
        .spec
        .harnesses
        .iter()
        .filter(|harness| harness.auth == HarnessAuthMode::Mediated && (harness.default || !harness.optional))
        .partition(|harness| {
            let stored = stored.get(&harness.kind).map_or(&[][..], Vec::as_slice);
            harness.login.choose(stored, None, &BTreeMap::new()).is_some()
        });
    let kinds = |harnesses: Vec<&agent::HarnessSpec>| harnesses.into_iter().map(|harness| harness.kind).collect();
    Ok(Logins {
        missing: kinds(missing),
        stored: kinds(held),
    })
}

/// The login `harness` of `agent` signs in under: the first its set names, else the default one.
fn login_of(agent: &Agent, harness: Harness) -> LoginName {
    agent
        .spec
        .harnesses
        .iter()
        .find(|spec| spec.kind == harness)
        .and_then(|spec| spec.login.first().cloned())
        .unwrap_or_default()
}

/// The command that stores the login `harness` of `agent` signs in under, quoted for a message.
pub(super) fn login_command(agent: &Agent, harness: Harness) -> String {
    agent::harness::login_command(harness, &login_of(agent, harness))
}

/// Decides which harnesses to sign in to: those with no login yet, and those whose login agentd
/// holds already when the person would rather sign in again, as when it stopped working.
pub(super) async fn choose_logins(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    draft: &mut Draft,
) -> CommandResult<()> {
    choose_stored_logins(arguments, answers, client, draft).await?;
    let Logins { missing, stored } = logins(client, &draft.agent).await?;
    if arguments.sign_in == Some(SignInChoice::Skip) {
        if !missing.is_empty() {
            let logins: Vec<_> = missing
                .iter()
                .map(|harness| login_command(&draft.agent, *harness))
                .collect();
            eprintln!("warning: the agent waits until you run {}", logins.join(" and "));
        }
        draft.access.reused_logins = stored;
        return Ok(());
    }
    draft.access.sign_in_to = missing;
    for harness in stored {
        let again = match arguments.sign_in {
            Some(SignInChoice::Again) => true,
            Some(_) => false,
            None if !answers.interactive() => false,
            None => answers.choose(
                "--sign-in",
                &format!(
                    "{} is signed in already on this machine, as {}. Use that login?",
                    harness.display_name(),
                    login_of(&draft.agent, harness)
                ),
                vec![
                    Choice::new(false, "Yes, use it").suggested(),
                    Choice::new(true, "No, sign in again"),
                ],
                0,
            )?,
        };
        if again {
            draft.access.sign_in_to.push(harness);
        } else {
            draft.access.reused_logins.push(harness);
        }
    }
    Ok(())
}

/// Points each harness of `draft` at the login `--login` names for it, or, when its login set finds
/// none of the logins agentd holds, at one of those the person picks. Signing in again would store
/// a second login for a subscription agentd holds already.
async fn choose_stored_logins(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    draft: &mut Draft,
) -> CommandResult<()> {
    let stored = stored_logins(client).await?;
    for harness in &mut draft.agent.spec.harnesses {
        if let Some((_, login)) = arguments.login.iter().find(|(kind, _)| *kind == harness.kind) {
            harness.login = LoginSet::one(login.clone());
            continue;
        }
        let names = stored.get(&harness.kind).map_or(&[][..], Vec::as_slice);
        if harness.auth != HarnessAuthMode::Mediated
            || names.is_empty()
            || harness.login.choose(names, None, &BTreeMap::new()).is_some()
        {
            continue;
        }
        let kind = harness.kind;
        let mut choices: Vec<_> = names
            .iter()
            .map(|name| {
                Choice::new(Some(name.clone()), format!("Your login {name}"))
                    .flag("--login", &[&format!("{}={name}", kind.as_str())])
            })
            .collect();
        choices.push(Choice::new(None, "Sign in to a new one"));
        let question = format!("Which {} login should the agent use?", kind.display_name());
        if let Some(login) = answers.choose("--login", &question, choices, 0)? {
            harness.login = LoginSet::one(login);
        }
    }
    Ok(())
}

/// Signs in, in a short-lived sandbox, to the harnesses chosen for it, and to GitHub when the
/// browser was chosen.
pub(super) async fn sign_in_where_needed(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    home: &ControlPlaneHome,
    staging: &Staging,
    draft: &mut Draft,
) -> CommandResult<()> {
    let harnesses = std::mem::take(&mut draft.access.sign_in_to);
    let github = draft.access.github_browser.take();
    if harnesses.is_empty() && github.is_none() {
        return Ok(());
    }
    if !answers.interactive() {
        let logins: Vec<_> = harnesses
            .iter()
            .map(|harness| login_command(&draft.agent, *harness))
            .collect();
        return Err(CommandError::Message(format!(
            "signing in needs a terminal and a browser; run setup at a terminal, or {} first",
            if logins.is_empty() {
                "--github token".to_owned()
            } else {
                logins.join(" and ")
            }
        )));
    }
    let agent = SignInAgent::start(client, home, staging, arguments.timeout).await?;
    let signed_in = async {
        for harness in harnesses {
            let Some(recipe) = agent::harness::sandbox_sign_in(harness) else {
                continue;
            };
            println!(
                "\nSign in to {}. Its sign-in page opens in your browser.",
                harness.display_name()
            );
            let credential = agent.run(&recipe).await?;
            let login = login_of(&draft.agent, harness);
            let stored = match client
                .auth_login(harness, login.clone(), credential.to_string(), false)
                .await
            {
                Ok(stored) => stored,
                // A credential read off the sign-in's screen can come out garbled, while the person
                // still sees it whole: let them paste it. One read from a file has no such cause.
                Err(error) if recipe.artifact.is_none() => {
                    eprintln!(
                        "\nThe {} credential read from the screen was refused: {error}",
                        harness.display_name()
                    );
                    let pasted = answers.secret(
                        "--sign-in skip",
                        &["Copy the credential the sign-in printed above, all of it, and paste it here"],
                    )?;
                    let pasted: String = pasted.chars().filter(|character| !character.is_whitespace()).collect();
                    client.auth_login(harness, login, pasted, false).await?
                }
                Err(error) => return Err(error.into()),
            };
            println!("\r{} authentication stored", stored.provider);
        }
        if let Some(BrowserSignIn { account }) = github {
            println!("\nSign in to GitHub. Its device page opens in your browser; enter the code shown here.");
            let token = agent.run(&agent::harness::github::sign_in()).await?;
            record_github(arguments, answers, draft, token, account.as_deref()).await?;
        }
        Ok::<_, CommandError>(())
    }
    .await;
    agent.finish().await;
    signed_in
}
