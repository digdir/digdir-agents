//! The agents setup made before, in `~/agents/<name>/`: finding them with their state, carrying
//! on with one, and replacing one with a new definition.
//!
//! Replacing is done so that a failure or a change of mind leaves the old definition as it was:
//! the new folder is written in full first, and only then is the old agent deleted and its folder
//! swapped for the new one. The folder goes whole, so no file of the old definition is left in
//! the new one.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use agent::{Agent, Error, control_api::Client, manifest};
use clap::ValueEnum;
use zeroize::Zeroizing;

use super::{
    SetupArguments,
    answers::{Answers, Choice},
};
use crate::{CommandError, CommandResult};

/// The first line `agentctl setup` writes in a manifest, before what the agent came from.
const ORIGIN_PREFIX: &str = "# Written by `agentctl setup` from ";

/// One agent definition in `~/agents`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Existing {
    pub(super) folder: PathBuf,
    /// The manifest's agent name, or the folder's name when the manifest does not resolve.
    pub(super) name: String,
    /// What setup made it from, when setup made it.
    pub(super) origin: Option<String>,
    pub(super) state: State,
}

/// Where an agent definition stands in agentd.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum State {
    /// Created from this folder and ready.
    Running,
    /// Created from this folder and stopped.
    Stopped,
    /// Created from this folder, but not ready.
    NotReady,
    /// Created from this folder and being deleted.
    Deleting,
    /// Not created, or created and deleted again.
    NotCreated,
    /// agentd has an agent of this name from another folder.
    NameUsedElsewhere,
    /// The manifest does not resolve, for this reason.
    Invalid(String),
}

impl Existing {
    /// The agent agentd runs from this folder, if any.
    pub(super) fn agent_here(&self) -> Option<&str> {
        matches!(
            self.state,
            State::Running | State::Stopped | State::NotReady | State::Deleting
        )
        .then_some(self.name.as_str())
    }

    fn label(&self) -> String {
        let state = match &self.state {
            State::Running => "running".to_owned(),
            State::Stopped => "stopped".to_owned(),
            State::NotReady => "not ready".to_owned(),
            State::Deleting => "being deleted".to_owned(),
            State::NotCreated => "not created".to_owned(),
            State::NameUsedElsewhere => format!("another agent is named {}", self.name),
            State::Invalid(_) => "its agent.yaml has an error".to_owned(),
        };
        self.origin.as_ref().map_or_else(
            || format!("{}: {state}", self.name),
            |origin| format!("{}: {state} (from {origin})", self.name),
        )
    }
}

/// The agent definitions in `root`, one per folder holding an `agent.yaml`, with their state in
/// `agents`. Without a `client`, as in a dry run, none counts as created.
pub(super) async fn scan(client: Option<&Client>, root: &Path, agents: &[Agent]) -> Vec<Existing> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|folder| folder.join(manifest::MANIFEST_FILE).is_file())
        .collect();
    folders.sort();
    let mut found = Vec::new();
    for folder in folders {
        let path = folder.join(manifest::MANIFEST_FILE);
        let origin = std::fs::read_to_string(&path).ok().and_then(|text| {
            let first = text.lines().next()?;
            Some(first.strip_prefix(ORIGIN_PREFIX)?.trim_end_matches('.').to_owned())
        });
        let folder_name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (name, state) = match manifest::resolve(&path) {
            Err(error) => (folder_name, State::Invalid(error.to_string())),
            Ok(resolved) => {
                let name = resolved.agent.metadata.name;
                let state = state_of(client, &folder, &name, agents).await;
                (name, state)
            }
        };
        found.push(Existing {
            folder,
            name,
            origin,
            state,
        });
    }
    found
}

async fn state_of(client: Option<&Client>, folder: &Path, name: &str, agents: &[Agent]) -> State {
    let (Some(client), Some(agent)) = (client, agents.iter().find(|agent| agent.metadata.name == name)) else {
        return State::NotCreated;
    };
    let from_here = client
        .resolve_agent(folder.to_path_buf())
        .await
        .is_ok_and(|resolved| resolved.metadata.name == name);
    if !from_here {
        State::NameUsedElsewhere
    } else if agent.metadata.deletion_timestamp.is_some() {
        State::Deleting
    } else if agent.status.is_ready() {
        State::Running
    } else if agent.spec.is_stopped() || agent.status.is_stopped() {
        State::Stopped
    } else {
        State::NotReady
    }
}

/// What setup can do with an agent it made before. What `agentctl` does already, such as starting
/// an agent or showing why it is not ready, setup names the command for instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ExistingAction {
    /// Connect it to an editor on this machine.
    Connect,
    /// Sign in to its harnesses.
    SignIn,
    /// Replace it with a new agent definition.
    Replace,
}

/// What happens after an action on an existing agent.
pub(super) enum Proceed {
    /// Setup is done.
    Done,
    /// Setup goes on to make a new definition that replaces this one.
    Replace(Existing),
}

/// What an existing-agent action needs from setup.
pub(super) struct Context<'a> {
    pub(super) arguments: &'a SetupArguments,
    pub(super) answers: &'a Answers,
    pub(super) client: &'a Client,
    pub(super) home: &'a agent::local::home::ControlPlaneHome,
    pub(super) staging: &'a super::staging::Staging,
}

/// The existing agent `--existing` names, or the one the person picks.
pub(super) fn pick(arguments: &SetupArguments, answers: &Answers, found: &[Existing]) -> CommandResult<Existing> {
    if let Some(wanted) = &arguments.existing {
        return found
            .iter()
            .find(|existing| existing.name == *wanted || existing.folder.ends_with(wanted))
            .cloned()
            .ok_or_else(|| CommandError::Message(format!("~/agents holds no agent named {wanted}")));
    }
    let choices = found
        .iter()
        .map(|existing| Choice::new(existing.clone(), existing.label()).flag("--existing", &[&existing.name]))
        .collect();
    Ok(answers.choose("--existing", "Which agent?", choices, 0)?)
}

/// Says what the state of `existing` needs, asks what to do with it, offering what fits that
/// state, and does it.
pub(super) async fn proceed(context: &Context<'_>, existing: Existing) -> CommandResult<Proceed> {
    let cli = super::command_line(context.home);
    if let Some(remedy) = remedy(&existing, &cli) {
        println!("{remedy}");
    }
    let sign_in = match existing.state {
        State::Running | State::Stopped | State::NotReady => {
            let agent = manifest::resolve(&existing.folder.join(manifest::MANIFEST_FILE))?.agent;
            let logins = super::sign_in::logins(context.client, &agent).await?;
            !logins.missing.is_empty() || !logins.stored.is_empty()
        }
        _ => false,
    };
    let offered = actions(&existing.state, sign_in);
    let action = match context.arguments.action {
        Some(action) if offered.iter().any(|(offered, _)| *offered == Some(action)) => action,
        Some(action) => {
            return Err(CommandError::Message(format!(
                "{} cannot be given --action {}; it is {}",
                existing.name,
                super::value_name(&action),
                existing.label()
            )));
        }
        None => {
            let choices = offered
                .into_iter()
                .map(|(action, label)| {
                    action.map_or_else(
                        || Choice::new(None, label),
                        |action| Choice::new(Some(action), label).flag("--action", &[&super::value_name(&action)]),
                    )
                })
                .collect();
            let question = format!("What should setup do with {}?", existing.label());
            let Some(action) = context.answers.choose("--action", &question, choices, 0)? else {
                return Ok(Proceed::Done);
            };
            action
        }
    };
    if action != ExistingAction::Replace {
        context.answers.no_way_back();
    }
    let name = existing.name.as_str();
    match action {
        ExistingAction::Connect => {
            if !context.arguments.no_ssh_config {
                crate::install_ssh_config(context.home)?;
            }
            super::ide::offer(context.answers, context.arguments.ide, context.client, name).await?;
        }
        ExistingAction::SignIn => {
            let mut draft = super::team_draft(&existing.folder.join(manifest::MANIFEST_FILE), name)?;
            let logins = super::sign_in::logins(context.client, &draft.agent).await?;
            draft.access.sign_in_to = logins.missing;
            draft.access.sign_in_to.extend(logins.stored);
            super::sign_in_where_needed(
                context.arguments,
                context.answers,
                context.client,
                context.home,
                context.staging,
                &mut draft,
            )
            .await?;
            println!("Signed in; agent/{name} picks the logins up as it converges.");
        }
        ExistingAction::Replace => return Ok(Proceed::Replace(existing)),
    }
    Ok(Proceed::Done)
}

/// The command that brings `existing` on from its state, when its state needs one.
fn remedy(existing: &Existing, cli: &str) -> Option<String> {
    let name = &existing.name;
    let manifest = existing.folder.join(manifest::MANIFEST_FILE);
    Some(match &existing.state {
        State::Running | State::Deleting => return None,
        State::Stopped => format!("agent/{name} is stopped; `{cli} start agent/{name}` starts it."),
        State::NotReady => format!("agent/{name} is not ready; `{cli} describe agent/{name}` shows why."),
        State::NotCreated => format!(
            "agent/{name} is not created; `{cli} apply -f {}` creates it.",
            manifest.display()
        ),
        State::NameUsedElsewhere => format!(
            "Another agent is named {name}; give this one another name in {}.",
            manifest.display()
        ),
        State::Invalid(error) => format!("{}: {error}", manifest.display()),
    })
}

/// What setup offers for an agent in `state`: connecting a running one, signing in when
/// `sign_in` allows it, replacing it, and leaving it as it is.
fn actions(state: &State, sign_in: bool) -> Vec<(Option<ExistingAction>, &'static str)> {
    let mut actions = Vec::new();
    if *state == State::Running {
        actions.push((Some(ExistingAction::Connect), "Connect it to an editor on this machine"));
    }
    if sign_in {
        actions.push((Some(ExistingAction::SignIn), "Sign in to its harnesses"));
    }
    actions.push((Some(ExistingAction::Replace), "Replace it with a new agent definition"));
    actions.push((None, "Nothing, for now"));
    actions
}

/// Offers to reuse the values in the `.env` of the definition being replaced, for what the new
/// manifest declares, so tokens and keys need not be entered again.
pub(super) async fn reusable_values(
    answers: &Answers,
    replaced: &Existing,
) -> Result<BTreeMap<String, Zeroizing<String>>, Error> {
    let values = agent::environment::read_or_empty(&replaced.folder.join(agent::control_plane::ENV_FILE)).await?;
    if values.is_empty() {
        return Ok(values);
    }
    let names: Vec<_> = values.keys().map(String::as_str).collect();
    let question = format!("Reuse the values of the agent being replaced ({})?", names.join(", "));
    if answers.confirm("--env-file", &[&question], true)? {
        Ok(values)
    } else {
        Ok(BTreeMap::new())
    }
}

/// Swaps the old definition `replaced` for the folder written at `staged`: deletes the agent
/// created from it, waits until it is gone, then moves the old folder aside to `backup` and the
/// new one into its place. Should the move fail, the old folder is put back.
pub(super) async fn swap(
    client: &Client,
    replaced: &Existing,
    staged: &Path,
    backup: &Path,
    timeout: Duration,
) -> CommandResult<()> {
    if let Some(name) = replaced.agent_here() {
        eprintln!("Deleting agent/{name}, which the new definition replaces.");
        if replaced.state != State::Deleting {
            client.delete(name).await?;
        }
        wait_until_deleted(client, name, timeout).await?;
    }
    std::fs::rename(&replaced.folder, backup).map_err(|error| {
        CommandError::Message(format!("could not move {} aside: {error}", replaced.folder.display()))
    })?;
    if let Err(error) = move_folder(staged, &replaced.folder) {
        std::fs::remove_dir_all(&replaced.folder).ok();
        std::fs::rename(backup, &replaced.folder).ok();
        return Err(CommandError::Message(format!(
            "could not put the new definition in {}: {error}; the old one is back",
            replaced.folder.display()
        )));
    }
    Ok(())
}

async fn wait_until_deleted(client: &Client, name: &str, timeout: Duration) -> CommandResult<()> {
    let deleted = async {
        loop {
            match client.get(name).await {
                Err(Error::Rpc(error)) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error),
                Ok(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    };
    match tokio::time::timeout(timeout, deleted).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(CommandError::Message(format!(
            "agent/{name} was not deleted in time; nothing was replaced"
        ))),
    }
}

/// Moves a folder, copying it when it is on another file system than its destination.
pub(super) fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_all(from, to)?;
    std::fs::remove_dir_all(from)
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{ExistingAction, State, actions, move_folder, scan};

    #[test]
    fn a_running_agent_can_be_connected_and_any_replaced() {
        let offered = |state: &State, sign_in| -> Vec<_> {
            actions(state, sign_in).into_iter().map(|(action, _)| action).collect()
        };
        assert_eq!(
            offered(&State::Running, true),
            [
                Some(ExistingAction::Connect),
                Some(ExistingAction::SignIn),
                Some(ExistingAction::Replace),
                None
            ]
        );
        assert_eq!(offered(&State::Stopped, false), [Some(ExistingAction::Replace), None]);
        assert_eq!(
            offered(&State::Invalid("bad".into()), false),
            [Some(ExistingAction::Replace), None]
        );
    }

    #[tokio::test(flavor = "local")]
    async fn definitions_are_found_with_where_they_came_from() {
        let root = tempfile::tempdir().expect("a root");
        let valid = root.path().join("api");
        std::fs::create_dir_all(valid.join("home")).expect("a home");
        std::fs::write(
            valid.join("agent.yaml"),
            "# Written by `agentctl setup` from the standard agent dotnet-backend-developer.\n\
             apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: api}\nspec:\n  sandbox:\n    \
             image: {type: reference, reference: example.com/agent:latest}\n    platform: {os: linux}\n    \
             resources: {cpu: '2', memory: 4Gi, rootFilesystem: {capacity: 20Gi, mode: layered}}\n  \
             home: {source: home}\n  harnesses: [{type: pi}]\n  network: {mode: mediated, allow: all}\n",
        )
        .expect("a manifest");
        let broken = root.path().join("broken");
        std::fs::create_dir_all(&broken).expect("a folder");
        std::fs::write(broken.join("agent.yaml"), "kind: Agent\n").expect("a broken manifest");
        std::fs::create_dir_all(root.path().join("notes")).expect("a folder without a manifest");

        let found = scan(None, root.path(), &[]).await;

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "api");
        assert_eq!(found[0].state, State::NotCreated);
        assert_eq!(
            found[0].origin.as_deref(),
            Some("the standard agent dotnet-backend-developer")
        );
        assert_eq!(found[1].name, "broken");
        assert!(matches!(found[1].state, State::Invalid(_)));
    }

    #[test]
    fn a_folder_moves_whole() {
        let root = tempfile::tempdir().expect("a root");
        let from = root.path().join("from");
        std::fs::create_dir_all(from.join("home/.claude")).expect("folders");
        std::fs::write(from.join("home/.claude/settings.json"), "{}").expect("a file");
        let to = root.path().join("to");
        move_folder(&from, &to).expect("moved");
        assert!(!from.exists());
        assert!(to.join("home/.claude/settings.json").is_file());
    }
}
