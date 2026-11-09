//! `agentctl new`: making a new agent with the agent builder.
//!
//! The builder is a definition agentd carries, so it needs nothing downloaded or checked out.
//! `new` runs it on a checkout, as `agentctl run` would, with the person's folder of personal
//! agents beside it. It keeps running until the person deletes it, so running `new` on the same
//! checkout again goes back to the same conversation.

use std::path::{Path, PathBuf};

use agent::{
    Agent, Error, ModelSelection, RunState,
    control_api::Client,
    control_plane::{BindSource, RunDefinition, RunRequest, WaitPolicy},
    local::home::ControlPlaneHome,
    sessions::{SessionName, SessionRequest},
};
use clap::Args;
use sandbox::SandboxPath;

use crate::{CommandError, CommandResult, progress};

/// The builder's definition, by its name among those agentd carries.
const DEFINITION: &str = "agent-builder";

/// The folder in the person's home where the builder writes personal agents: `agents/<name>/` of
/// the home directory, where agentctl finds them from there.
const PERSONAL_AGENTS: &str = "agents";

/// The builder's session, since it makes one agent at a time.
const SESSION: &str = "new-agent";

#[derive(Args)]
pub(crate) struct NewArguments {
    /// The checkout of the repository the new agent is for. Defaults to the current folder.
    directory: Option<PathBuf>,
}

/// Makes a new agent with the builder, starting it on the checkout or going back to it there.
pub(crate) async fn new(arguments: NewArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    if !crate::prompt::interactive() {
        return Err(CommandError::Message(
            "making a new agent is a conversation with the agent builder, which needs a terminal".into(),
        ));
    }
    let checkout = checkout(arguments.directory)?;
    let targets = Targets::of(&agent::bundled::Definitions::bundled()?.get(DEFINITION)?.agent())?;
    let agents = client.list_agents(false).await?;
    let name = match agents.iter().find(|agent| builds_on(agent, &targets, &checkout)) {
        Some(builder) => {
            let name = builder.metadata.name.clone();
            if builder.spec.run_state() == RunState::Stopped {
                client.set_run_state(&name, RunState::Running).await?;
            }
            eprintln!(
                "Back to the agent builder on {}, where you left it.",
                checkout.display()
            );
            talk(home, client, &name).await?;
            name
        }
        None => start(home, client, &targets, &checkout).await?,
    };
    println!(
        "The agent builder keeps running, and `agentctl new` in {} goes back to it.\nApply the agent it made with \
         `agentctl apply -f` and the manifest it named. Run `agentctl delete {name}` once you are done with it.",
        checkout.display()
    );
    Ok(())
}

/// The checkout `directory` names, or the current folder, as the folder it really is, which is how
/// agentd records the folders an agent is given.
fn checkout(directory: Option<PathBuf>) -> CommandResult<PathBuf> {
    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&directory)
        .ok()
        .filter(|checkout| checkout.is_dir())
        .ok_or_else(|| {
            CommandError::Message(format!(
                "{} is no folder. Name the checkout of the repository the new agent is for.",
                directory.display()
            ))
        })
}

/// Where the builder's definition mounts the folders `new` gives it: the checkout in its working
/// directory, and the person's folder of personal agents.
#[derive(Debug)]
struct Targets {
    checkout: SandboxPath,
    personal_agents: SandboxPath,
}

impl Targets {
    /// The targets of `builder`'s bind mounts without a source: the one in its working directory
    /// for the checkout, and the other for the personal agents.
    fn of(builder: &Agent) -> Result<Self, Error> {
        let working = agent::sandbox::platform::working_directory(&builder.spec.sandbox.platform.os);
        let (checkout, others): (Vec<_>, Vec<_>) = builder
            .spec
            .sandbox
            .mounts
            .iter()
            .filter_map(|mount| match mount {
                agent::MountSpec::Bind {
                    source: None, target, ..
                } => Some(target.clone()),
                _ => None,
            })
            .partition(|target| Some(target.as_str()) == working);
        match (checkout.as_slice(), others.as_slice()) {
            ([checkout], [personal_agents]) => Ok(Self {
                checkout: checkout.clone(),
                personal_agents: personal_agents.clone(),
            }),
            _ => Err(Error::Invalid(
                "the agent builder's definition must leave two folders to `agentctl new`: the checkout, in its \
                 working directory, and the personal agents"
                    .into(),
            )),
        }
    }
}

/// Whether `agent` is a builder `new` started on `checkout`.
fn builds_on(agent: &Agent, targets: &Targets, checkout: &Path) -> bool {
    let builder = matches!(
        &agent.status.origin,
        Some(agent::Origin::Reference { reference })
            if agent::DefinitionReference::parse(reference).is_ok_and(|parsed| parsed.name() == DEFINITION)
    );
    builder && agent.status.bindings.get(&targets.checkout).map(PathBuf::as_path) == Some(checkout)
}

/// Starts a builder on `checkout` and talks with it, returning its name. agentd names it after the
/// checkout's folder. A builder that does not start is deleted, so that `new` starts afresh rather
/// than going back to it.
async fn start(home: &ControlPlaneHome, client: &Client, targets: &Targets, checkout: &Path) -> CommandResult<String> {
    let personal_agents = agent::local::home::user_home_directory()
        .ok_or_else(|| Error::Invalid("cannot find your home folder, where personal agents go".into()))?
        .join(PERSONAL_AGENTS);
    std::fs::create_dir_all(&personal_agents).map_err(Error::from)?;
    eprintln!("Starting the agent builder on {}.", checkout.display());
    let (ran, _lease) = client
        .run(RunRequest {
            definition: RunDefinition::Reference {
                reference: DEFINITION.into(),
            },
            binds: vec![
                BindSource {
                    source: checkout.to_path_buf(),
                    target: Some(targets.checkout.clone()),
                },
                BindSource {
                    source: personal_agents,
                    target: Some(targets.personal_agents.clone()),
                },
            ],
            lifetime: None,
            name: None,
        })
        .await?;
    let name = ran.agent.metadata.name;
    if let Err(error) = talk(home, client, &name).await {
        client.delete(&name).await.ok();
        return Err(error);
    }
    Ok(name)
}

/// Attaches to the builder's session, which opens with its first question, waiting for the builder
/// to start first.
async fn talk(home: &ControlPlaneHome, client: &Client, name: &str) -> CommandResult<()> {
    let target = progress::Wait::start()
        .until(
            client,
            name,
            client.ensure_session(
                name,
                SessionName::new(SESSION)?,
                SessionRequest {
                    harness: None,
                    model_selection: ModelSelection::default(),
                    initial_prompt: Some("I want to make a new agent for the repository you have checked out.".into()),
                },
                WaitPolicy::UntilConverged,
            ),
        )
        .await?;
    agent::sessions::attach(home.path(), &target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DEFINITION, Targets, builds_on};

    fn builder() -> agent::Agent {
        agent::bundled::Definitions::bundled()
            .expect("the bundled definitions")
            .get(DEFINITION)
            .expect("the agent builder")
            .agent()
    }

    #[test]
    fn the_builder_leaves_new_the_checkout_and_the_personal_agents() {
        let builder = builder();
        let targets = Targets::of(&builder).expect("targets");
        let working = agent::sandbox::platform::working_directory(&builder.spec.sandbox.platform.os);
        assert_eq!(Some(targets.checkout.as_str()), working);
        assert_ne!(targets.personal_agents, targets.checkout);
    }

    #[test]
    fn new_goes_back_only_to_the_builder_on_the_same_checkout() {
        let mut builder = builder();
        let targets = Targets::of(&builder).expect("targets");
        let checkout = Path::new("/src/service");
        builder
            .status
            .bindings
            .insert(targets.checkout.clone(), checkout.to_path_buf());
        assert!(
            !builds_on(&builder, &targets, checkout),
            "an agent applied from a manifest"
        );

        builder.status.origin = Some(agent::Origin::Reference {
            reference: format!("bundled/{DEFINITION}"),
        });
        assert!(builds_on(&builder, &targets, checkout));
        assert!(!builds_on(&builder, &targets, Path::new("/src/other")));

        builder.status.origin = Some(agent::Origin::Reference {
            reference: "bundled/reviewer".into(),
        });
        assert!(!builds_on(&builder, &targets, checkout), "another definition");
    }
}
