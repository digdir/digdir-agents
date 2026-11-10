//! The agent builder, which `agentctl new` and `agentctl update` talk with on a checkout.
//!
//! The builder is a definition agentd carries, so it needs nothing downloaded or checked out. It
//! runs on a checkout, as `agentctl run` would, with the person's folder of personal agents beside
//! it. A checkout has one builder, which keeps running until the person deletes it, and each
//! command talks with it in a session of its own, so running the command again goes back to the
//! same conversation.

use std::path::{Path, PathBuf};

use agent::{
    Agent, Error, ModelSelection, RunState,
    control_api::Client,
    control_plane::{BindSource, RunDefinition, RunRequest, WaitPolicy},
    local::home::ControlPlaneHome,
    sessions::{AttachTarget, SessionName, SessionRequest},
};
use sandbox::SandboxPath;

use crate::{CommandError, CommandResult, consent, progress};

/// The builder's definition, by its name among those agentd carries.
const DEFINITION: &str = "agent-builder";

/// The folder in the person's home where the builder writes personal agents: `agents/<name>/` of
/// the home directory, where agentctl finds them from there.
const PERSONAL_AGENTS: &str = "agents";

/// What a command asks the builder to do: the session it talks in, and how it opens.
pub(crate) struct Task {
    pub(crate) session: &'static str,
    pub(crate) prompt: String,
}

/// The builder as this build of agentctl runs it.
pub(crate) struct Builder {
    definition: Agent,
    targets: Targets,
}

impl Builder {
    /// The builder agentd carries.
    pub(crate) fn bundled() -> Result<Self, Error> {
        let definition = agent::bundled::Definitions::bundled()?.get(DEFINITION)?.agent();
        let targets = Targets::of(&definition)?;
        Ok(Self { definition, targets })
    }

    /// The builder on `checkout` that is not being deleted, which a command can go back to.
    pub(crate) async fn find(&self, client: &Client, checkout: &Path) -> Result<Option<Agent>, Error> {
        Ok(client
            .list_agents(false)
            .await?
            .into_iter()
            .find(|agent| builds_on(agent, &self.targets, checkout)))
    }

    /// Whether `builder` runs the image this version runs it on. An agent keeps the image it was
    /// made with, which holds the agentctl it tries agents with, while its instructions and skills
    /// follow agentd.
    pub(crate) fn current(&self, builder: &Agent) -> bool {
        builder.spec.sandbox.image == self.definition.spec.sandbox.image
    }

    /// Starts `builder` again if it is stopped, and talks with it about `task`.
    pub(crate) async fn resume(
        &self,
        home: &ControlPlaneHome,
        client: &Client,
        builder: &Agent,
        task: &Task,
    ) -> CommandResult<()> {
        let name = &builder.metadata.name;
        if builder.spec.run_state() == RunState::Stopped {
            client.set_run_state(name, RunState::Running).await?;
        }
        let target = open_session(client, name, task).await?;
        agent::sessions::attach(home.path(), &target).await?;
        Ok(())
    }

    /// Starts a builder on `checkout` and talks with it about `task`, returning its name. agentd
    /// names it after the checkout's folder. A builder whose session never opens is deleted, so
    /// that the command starts afresh rather than going back to it. Once the person has talked
    /// with it, it is kept however the attachment ends, since going back to it is what the
    /// commands offer.
    pub(crate) async fn start(
        &self,
        home: &ControlPlaneHome,
        client: &Client,
        checkout: &Path,
        task: &Task,
    ) -> CommandResult<String> {
        let personal_agents = agent::local::home::user_home_directory()
            .ok_or_else(|| Error::Invalid("cannot find your home folder, where personal agents go".into()))?
            .join(PERSONAL_AGENTS);
        std::fs::create_dir_all(&personal_agents).map_err(Error::from)?;
        eprintln!("Starting the agent builder on {}.", checkout.display());
        let request = RunRequest {
            definition: RunDefinition::Reference {
                reference: DEFINITION.into(),
            },
            binds: vec![
                BindSource {
                    source: checkout.to_path_buf(),
                    target: Some(self.targets.checkout.clone()),
                },
                BindSource {
                    source: personal_agents,
                    target: Some(self.targets.personal_agents.clone()),
                },
            ],
            lifetime: None,
            name: None,
            allow_exposed_files: std::collections::BTreeSet::new(),
        };
        // The builder is talked with at a terminal, so the person is there to answer.
        let ask = consent::Ask {
            reader: "The agent builder",
            action: "Mount",
            allowed: false,
        };
        let (ran, _lease) = consent::send(
            request,
            &ask,
            |request| &mut request.allow_exposed_files,
            |request| client.run(request),
        )
        .await?;
        let name = ran.agent.metadata.name;
        let target = match open_session(client, &name, task).await {
            Ok(target) => target,
            Err(error) => {
                client.delete(&name).await.ok();
                return Err(error);
            }
        };
        agent::sessions::attach(home.path(), &target).await?;
        Ok(name)
    }
}

/// The folder `directory` names, or the current folder, as the folder it really is, which is how
/// agentd records the folders an agent is given.
pub(crate) fn folder(directory: Option<PathBuf>) -> CommandResult<PathBuf> {
    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&directory)
        .ok()
        .filter(|folder| folder.is_dir())
        .ok_or_else(|| {
            CommandError::Message(format!(
                "{} is no folder. Name the checkout of a repository.",
                directory.display()
            ))
        })
}

/// Where the builder's definition mounts the folders it is given: the checkout in its working
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
        let working = agent::sandbox::platform::working_directory(builder);
        let (checkout, others): (Vec<_>, Vec<_>) = agent::definitions::unsourced_binds(builder)
            .into_iter()
            .partition(|target| Some(target.as_str()) == working);
        match (checkout.as_slice(), others.as_slice()) {
            ([checkout], [personal_agents]) => Ok(Self {
                checkout: checkout.clone(),
                personal_agents: personal_agents.clone(),
            }),
            _ => Err(Error::Invalid(
                "the agent builder's definition must leave two folders to agentctl: the checkout, in its working \
                 directory, and the personal agents"
                    .into(),
            )),
        }
    }
}

/// Whether `agent` is a builder started on `checkout` that is not being deleted, which a command
/// could not go back to.
fn builds_on(agent: &Agent, targets: &Targets, checkout: &Path) -> bool {
    let builder = matches!(
        &agent.status.origin,
        Some(agent::Origin::Reference { reference })
            if agent::DefinitionReference::parse(reference).is_ok_and(|parsed| parsed.name() == DEFINITION)
    );
    builder
        && agent.metadata.deletion_timestamp.is_none()
        && agent.status.bindings.get(&targets.checkout).map(PathBuf::as_path) == Some(checkout)
}

/// The builder's session for `task`, which opens with the task's prompt once the builder has
/// started, and goes back to the conversation when it is open already.
async fn open_session(client: &Client, name: &str, task: &Task) -> CommandResult<AttachTarget> {
    Ok(progress::Wait::start()
        .until(
            client,
            name,
            client.ensure_session(
                name,
                SessionName::new(task.session)?,
                SessionRequest {
                    harness: None,
                    model_selection: ModelSelection::default(),
                    initial_prompt: Some(task.prompt.clone()),
                },
                WaitPolicy::UntilConverged,
            ),
        )
        .await?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Builder, DEFINITION, builds_on};

    #[test]
    fn the_builder_leaves_the_checkout_and_the_personal_agents() {
        let builder = Builder::bundled().expect("the agent builder");
        assert_eq!(
            Some(builder.targets.checkout.as_str()),
            agent::sandbox::platform::working_directory(&builder.definition)
        );
        assert_ne!(builder.targets.personal_agents, builder.targets.checkout);
    }

    #[test]
    fn only_the_builder_on_the_same_checkout_is_gone_back_to() {
        let Builder {
            definition: mut builder,
            targets,
        } = Builder::bundled().expect("the agent builder");
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

        builder.metadata.deletion_timestamp = Some(time::OffsetDateTime::UNIX_EPOCH);
        assert!(!builds_on(&builder, &targets, checkout), "a builder being deleted");
        builder.metadata.deletion_timestamp = None;

        builder.status.origin = Some(agent::Origin::Reference {
            reference: "bundled/reviewer".into(),
        });
        assert!(!builds_on(&builder, &targets, checkout), "another definition");
    }
}
