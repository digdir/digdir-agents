//! The agent builder, which `agentctl new` and `agentctl update` talk with on a checkout.
//!
//! The builder is a definition agentd carries, so it needs nothing downloaded or checked out. It
//! runs on a checkout, as `agentctl run` would, with the person's folder of personal agents beside
//! it. A checkout has one builder, which keeps running until the person deletes it, and each
//! command talks with it in a session of its own, so running the command again goes back to the
//! same conversation. Asked to with `--rm`, a command starts one that agentd deletes when the
//! command ends instead.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use agent::{
    Agent, Error, Lifetime, ModelSelection, RunState,
    control_api::Client,
    control_plane::{BindSource, RunDefinition, RunRequest, WaitPolicy},
    local::home::ControlPlaneHome,
    sessions::{AttachTarget, SessionName, SessionRequest},
};
use sandbox::SandboxPath;

use crate::{CommandError, CommandResult, consent, environment_file, progress};

/// The builder's definition, by its name among those agentd carries.
const DEFINITION: &str = "agent-builder";

/// The folder in the person's home where the builder writes personal agents: `agents/<name>/` of
/// the home directory, where agentctl finds them from there.
const PERSONAL_AGENTS: &str = "agents";

/// What a command asks the builder to do: the session it talks in, and how it opens.
pub(crate) struct Task {
    pub(crate) session: &'static str,
    pub(crate) prompt: String,
    /// Whether the task has the builder move the manifests of the checkout off their environment
    /// files, which then go first, as the builder may not be given a folder that holds one.
    pub(crate) moves_off: bool,
}

/// How the person is asked about the files in the builder's folders that may hold real keys. The
/// builder is talked with at a terminal, so the person is there to answer.
const ASK: consent::Ask<'static> = consent::Ask {
    reader: "The agent builder",
    action: "Mount",
    allowed: false,
};

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

    /// The builder kept on `checkout` that is not being deleted, which a command can go back to. One
    /// another command started with `--rm` is refused, since it goes when that command does, and a
    /// second builder would write to the same files.
    pub(crate) async fn find(&self, client: &Client, checkout: &Path) -> CommandResult<Option<Agent>> {
        let found = client
            .list_agents(true)
            .await?
            .into_iter()
            .find(|agent| builds_on(agent, &self.targets, checkout));
        match found {
            Some(builder) if builder.status.is_ephemeral() => Err(CommandError::Message(format!(
                "agent/{}, the agent builder on {}, was started with --rm by another `agentctl new` or `agentctl \
                 update`, and is deleted when that command ends. Go back to that command",
                builder.metadata.name,
                checkout.display()
            ))),
            found => Ok(found),
        }
    }

    /// Whether `builder` runs the image this version runs it on. An agent keeps the image it was
    /// made with, which holds the agentctl it tries agents with, while its instructions and skills
    /// follow agentd.
    pub(crate) fn current(&self, builder: &Agent) -> bool {
        builder.spec.sandbox.image == self.definition.spec.sandbox.image
    }

    /// Starts `builder` on `checkout` again if it is stopped, and talks with it about `task`.
    pub(crate) async fn resume(
        &self,
        home: &ControlPlaneHome,
        client: &Client,
        builder: &Agent,
        checkout: &Path,
        task: &Task,
    ) -> CommandResult<()> {
        let wait = progress::Wait::summarized(
            format!("Going back to the agent builder on {}", checkout.display()),
            format!("Back to the agent builder on {}, where you left it", checkout.display()),
        );
        let name = &builder.metadata.name;
        if builder.spec.run_state() == RunState::Stopped {
            client.set_run_state(name, RunState::Running).await?;
        }
        let target = open_session(&wait, client, name, task).await?;
        agent::sessions::attach(home.path(), &target).await?;
        Ok(())
    }

    /// The folders the builder is given on `checkout`: the checkout, and the person's folder of
    /// personal agents, made if it is missing.
    fn binds(&self, checkout: &Path) -> Result<Vec<BindSource>, Error> {
        let personal_agents = agent::local::home::user_home_directory()
            .ok_or_else(|| Error::Invalid("cannot find your home folder, where personal agents go".into()))?
            .join(PERSONAL_AGENTS);
        std::fs::create_dir_all(&personal_agents).map_err(Error::from)?;
        Ok(vec![
            BindSource {
                source: checkout.to_path_buf(),
                target: Some(self.targets.checkout.clone()),
            },
            BindSource {
                source: personal_agents,
                target: Some(self.targets.personal_agents.clone()),
            },
        ])
    }

    /// The files that may hold real keys in the folders the builder would be given on `checkout`,
    /// once the person agrees to let it read them, or why it may not. Asking before a command
    /// changes anything lets it stop with nothing deleted or started. When the `task` has the
    /// builder change the manifests that read an environment file, the person may have agentctl
    /// keep the file's values and delete it, and the task then says where the values are.
    pub(crate) async fn agree(
        &self,
        client: &Client,
        checkout: &Path,
        task: &mut Task,
    ) -> CommandResult<BTreeSet<PathBuf>> {
        let mut files = client.exposed_files(self.binds(checkout)?).await?;
        let mut agreed = BTreeSet::new();
        loop {
            files.retain(|file| !agreed.contains(&file.path));
            if files.is_empty() {
                return Ok(agreed);
            }
            // Every question comes before anything changes, so that a no leaves everything as it was.
            let moving = if task.moves_off {
                environment_file::ask(client, &files, checkout).await?
            } else {
                None
            };
            let rest = files
                .iter()
                .filter(|file| moving.as_ref().is_none_or(|moving| !moving.moves(file)))
                .cloned()
                .collect::<Vec<_>>();
            let answered = consent::agree(&rest, &ASK)?;
            if let Some(moving) = moving {
                task.prompt
                    .push_str(&moving.apply(client, self.binds(checkout)?).await?);
            }
            agreed.extend(answered.read);
            let now = client.exposed_files(self.binds(checkout)?).await?;
            // One that turned up while the person answered is asked about before anything is deleted.
            if !answered.delete.is_empty() && consent::asked_about_all(&rest, &now) {
                consent::delete(&answered.delete, &now)?;
                files = client.exposed_files(self.binds(checkout)?).await?;
            } else {
                files = now;
            }
        }
    }

    /// Starts a builder on `checkout` and talks with it about `task`, returning its name, with the
    /// files that may hold real keys the person `agreed` to, as [`Self::agree`] gives them. Files
    /// that turned up since are asked about too. agentd names it after the checkout's folder. A
    /// builder whose session never opens is deleted, so that the command starts afresh rather than
    /// going back to it. Once the person has talked with it, it is kept however the attachment
    /// ends, since going back to it is what the commands offer, unless they asked to `remove` it.
    /// Then agentd deletes it once this returns, or the command ends any other way.
    pub(crate) async fn start(
        &self,
        home: &ControlPlaneHome,
        client: &Client,
        checkout: &Path,
        task: &Task,
        agreed: BTreeSet<PathBuf>,
        remove: bool,
    ) -> CommandResult<String> {
        let request = RunRequest {
            definition: RunDefinition::Reference {
                reference: DEFINITION.into(),
            },
            binds: self.binds(checkout)?,
            lifetime: remove.then_some(Lifetime::Ephemeral),
            name: None,
            allow_exposed_files: agreed,
        };
        // Held while the person talks with it: agentd deletes an ephemeral builder when it goes.
        let (ran, _lease) = consent::send(
            request,
            &ASK,
            |request| &mut request.allow_exposed_files,
            |request| client.run(request),
        )
        .await?;
        let name = ran.agent.metadata.name;
        // After the questions about the folders, which the updating line would cut into.
        let wait = progress::Wait::summarized(
            format!("Starting the agent builder on {}", checkout.display()),
            format!("Agent builder started on {}", checkout.display()),
        );
        let target = match open_session(&wait, client, &name, task).await {
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

/// Why `command --rm` does not start a builder on `checkout`, where `existing` is kept: going back to
/// it is what the command does without `--rm`, and a second builder would write to the same files.
pub(crate) fn kept_already(command: &str, checkout: &Path, existing: &Agent) -> CommandError {
    let name = &existing.metadata.name;
    CommandError::Message(format!(
        "agent/{name}, the agent builder on {}, is kept already. Run `agentctl {command}` without --rm to go back \
         to it, or `agentctl delete agent/{name}` first",
        checkout.display()
    ))
}

/// The folder `directory` names, or the current folder, as the folder it really is, which is how
/// agentd records the folders an agent is given.
pub(crate) fn folder(directory: Option<PathBuf>) -> CommandResult<PathBuf> {
    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    common::fs::canonicalize(&directory)
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
/// started, and goes back to the conversation when it is open already. `wait` shows the progress
/// and, once the session is open, that the builder is ready: the builder is how the command makes
/// or updates definitions, not what the person asked for.
async fn open_session(wait: &progress::Wait, client: &Client, name: &str, task: &Task) -> CommandResult<AttachTarget> {
    let target = wait
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
        .await?;
    wait.ready();
    Ok(target)
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
