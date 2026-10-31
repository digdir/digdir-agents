//! `agentctl new`: making a new agent with the agent builder. The builder is agentctl's tool, not
//! one of the person's agents, so it runs as setup's sign-in agent does: under a name of its own,
//! which `get` and the TUI leave out, sized from its manifest, with the logins agentd holds, and
//! from a folder in the control-plane home rather than in ~/agents. `new` asks only which
//! repository the new agent is for, and mounts its checkout, where the builder writes a team's
//! agent for the person to review. Given the repository's GitHub address, `new` clones it on this
//! machine, so the builder needs no GitHub access and the person pushes with their own. Then it
//! attaches to a session in the builder that asks what the agent is for.
//!
//! When the person leaves the session, `new` offers to set up what the builder made, with setup's
//! questions, and removes the builder once that agent exists. Until then the builder keeps running,
//! so running `new` again goes back to the same conversation.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use agent::{
    Agent, Error, ModelSelection,
    control_plane::ApplyRequest,
    control_plane::WaitPolicy,
    manifest,
    sessions::{SessionName, SessionRequest},
};
use sandbox::image::ImageSource;

use super::answers::Choice;
use super::create::write_folder;
use super::existing::Context;
use super::fetch::{GithubSource, git_available};
use super::mounts::{Protected, bind_mounts, check_mounts, found_manifests, mount_unless_mounted, protected_folders};
use super::sizing::{Host, presets};
use super::source::{builder_draft, manifest_label};
use super::{AGENTS_FOLDER, NewArguments, docker, image, sign_in};
use crate::{CommandError, CommandResult};

/// The names of the agent builders `new` runs, which `get` and the TUI leave out.
pub(crate) const BUILDER_AGENT_PREFIX: &str = "agentctl-builder-";

/// The folder in the control-plane home that holds each running builder's definition, which agentd
/// reads from for as long as the builder runs.
const FOLDER: &str = "agent-builders";

/// The builder's session, one per builder, since each builder makes one agent.
const SESSION: &str = "new-agent";

/// Why a repository's GitHub address does not do on this machine.
const NO_GIT: &str = "agentctl clones a repository with git, which this machine does not have; install git, or give \
                      the folder of a checkout";

/// Whether `name` is one of the agent builders `new` runs.
pub(crate) fn is_builder_agent(name: &str) -> bool {
    name.starts_with(BUILDER_AGENT_PREFIX)
}

/// An agent the builder made, for setup to set up, and the builder to remove once it exists.
pub(super) struct Made {
    pub(super) manifest: PathBuf,
    pub(super) builder: String,
}

/// A running builder.
struct Builder {
    name: String,
    /// The folders it mounts: where it writes personal agents, and the repository's checkout.
    mounted: Vec<PathBuf>,
    /// When it started, so that what it made since tells apart from what was there.
    started: SystemTime,
}

/// Makes a new agent with the builder: starts it, or goes back to the one running, and talks with
/// it. Returns the agent it made when the person wants to set that up now.
pub(super) async fn make(context: &Context<'_>, new: &NewArguments, agents: &[Agent]) -> CommandResult<Option<Made>> {
    if !context.answers.interactive() {
        return Err(CommandError::Message(
            "making a new agent is a conversation with the agent builder, which needs a terminal".into(),
        ));
    }
    sweep(context.home.path(), agents);
    let builder = match agents.iter().find(|agent| is_builder_agent(&agent.metadata.name)) {
        Some(agent) => {
            println!("Back to the agent builder, where you left it.");
            running(context.home.path(), &agent.metadata.name)?
        }
        None => Box::pin(start(context, new)).await?,
    };
    talk(context, &builder).await?;
    after(context, &builder).await
}

/// Removes the builder `name` and its folder.
pub(super) async fn remove(context: &Context<'_>, name: &str) -> CommandResult<()> {
    context.client.delete(name).await?;
    std::fs::remove_dir_all(context.home.path().join(FOLDER).join(name)).ok();
    println!("Removed the agent builder.");
    Ok(())
}

/// Removes the folders of builders that no longer run, as when one was deleted by hand.
fn sweep(home: &Path, agents: &[Agent]) {
    let Ok(entries) = std::fs::read_dir(home.join(FOLDER)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !agents.iter().any(|agent| agent.metadata.name == name) {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
}

/// The builder `name`, from its folder.
fn running(home: &Path, name: &str) -> CommandResult<Builder> {
    let path = home.join(FOLDER).join(name).join(manifest::MANIFEST_FILE);
    let agent = manifest::resolve(&path)?.agent;
    let mounted = agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            agent::MountSpec::Bind { source, .. } => Some(source.clone()),
            agent::MountSpec::Tmpfs { .. } => None,
        })
        .collect();
    let started = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .map_err(Error::from)?;
    Ok(Builder {
        name: name.to_owned(),
        mounted,
        started,
    })
}

/// Starts a builder for the repository the person names.
async fn start(context: &Context<'_>, new: &NewArguments) -> CommandResult<Builder> {
    let (arguments, answers, client, home) = (context.arguments, context.answers, context.client, context.home);
    let user_home = agent::local::home::user_home_directory()
        .ok_or_else(|| Error::Invalid("the home directory is not set".into()))?;
    let protected = protected_folders(&user_home.join(AGENTS_FOLDER), home);
    let git = git_available().await;
    let check = |answer: &str| match GithubSource::parse(answer) {
        Some(_) if !git => Err(NO_GIT.to_owned()),
        Some(_) => Ok(()),
        None => {
            let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
            protected.check(&path).map(|_| ())
        }
    };
    let answer = match &new.repository {
        Some(repository) => repository.clone(),
        None => answers.text(
            "REPOSITORY",
            "Which repository is the new agent for? The folder of its checkout on this machine, or its address on \
             GitHub",
            None,
            check,
        )?,
    };
    let checkout = match GithubSource::parse(&answer) {
        Some(_) if !git => return Err(Error::Invalid(NO_GIT.into()).into()),
        Some(source) => clone(context, new, &protected, source).await?,
        None => protected
            .check(&manifest::expand_host_path(Path::new(answer.trim()))?)
            .map_err(Error::Invalid)?,
    };

    let mut draft = builder_draft(context.staging.path())?;
    draft.agent.metadata.name = format!("{BUILDER_AGENT_PREFIX}{:04x}", sign_in::rand_suffix());
    let [_, medium, _] = presets(&Host::measure(home.path()), draft.minimum, draft.recommended);
    medium.apply(&mut draft.agent.spec.sandbox.resources)?;
    // The folder it writes personal agents to is agentctl's to make.
    for (source, _) in bind_mounts(&draft)? {
        if !source.exists() && source.starts_with(&user_home) {
            std::fs::create_dir_all(&source).map_err(Error::from)?;
        }
    }
    mount_unless_mounted(&mut draft, &checkout)?;
    check_mounts(&draft, &protected, &[])?;
    if let ImageSource::Reference { reference } = &draft.agent.spec.sandbox.image
        && matches!(image::check(reference).await, image::Pull::Missing)
    {
        eprintln!(
            "The agent builder's image is not published yet, so agentctl builds it here. That needs Docker and takes a \
             while."
        );
        draft.built_here = Some(reference.clone());
        docker::ensure().await?;
    }
    let logins = sign_in::logins(client, &draft.agent).await?;
    draft.access.sign_in_to = logins.missing;
    draft.access.reused_logins = logins.stored;
    answers.no_way_back();
    sign_in::sign_in_where_needed(arguments, answers, client, home, context.staging, &mut draft).await?;

    let name = draft.agent.metadata.name.clone();
    let folder = home.path().join(FOLDER).join(&name);
    let manifest_path = write_folder(&mut draft, &folder)?;
    eprintln!("Starting the agent builder. It is removed once your new agent is set up.");
    let started = async {
        client
            .apply(ApplyRequest {
                source_directory: folder.clone(),
                manifest_path: Some(manifest_path),
                env_file: None,
                create_only: true,
                agent: draft.agent.clone(),
            })
            .await?;
        crate::wait_until_converged(client, &name, arguments.timeout).await?;
        Ok::<_, CommandError>(())
    }
    .await;
    if let Err(error) = started {
        client.delete(&name).await.ok();
        std::fs::remove_dir_all(&folder).ok();
        return Err(error);
    }
    running(home.path(), &name)
}

/// The checkout of a repository the person gave by its GitHub address: theirs, when the folder
/// they name already holds one, else a clone made there. It stays afterwards, since the person
/// reviews and pushes from it what the builder writes.
async fn clone(
    context: &Context<'_>,
    new: &NewArguments,
    protected: &Protected,
    mut source: GithubSource,
) -> CommandResult<PathBuf> {
    let check = |answer: &str| {
        let path = manifest::expand_host_path(Path::new(answer.trim())).map_err(|error| error.to_string())?;
        cloned_already(protected, &source, &path).map(|_| ())
    };
    let answer = match &new.clone_into {
        Some(path) => path.display().to_string(),
        None => context.answers.text(
            "--clone-into",
            &format!(
                "Where should agentctl clone {}/{}? A folder on this machine, kept for you to review and push what \
                 the builder writes there",
                source.owner, source.repository
            ),
            Some(&format!("~/repos/{}", source.repository)),
            check,
        )?,
    };
    let folder = manifest::expand_host_path(Path::new(answer.trim()))?;
    if cloned_already(protected, &source, &folder).map_err(Error::Invalid)? {
        println!("Using your checkout in {}, as it is.", folder.display());
    } else {
        source.clone_to(&folder).await?;
    }
    Ok(protected.check(&folder).map_err(Error::Invalid)?)
}

/// Whether `folder` holds a checkout of `source` already, or why it may not be cloned into: it
/// must be new, or empty, and may not be inside a folder that stays out of agents.
fn cloned_already(protected: &Protected, source: &GithubSource, folder: &Path) -> Result<bool, String> {
    if !folder.exists() {
        return protected.check_new(folder).map(|()| false);
    }
    protected.check(folder)?;
    if source.is_cloned_in(folder) {
        return Ok(true);
    }
    let empty = std::fs::read_dir(folder)
        .map_err(|error| format!("{}: {error}", folder.display()))?
        .next()
        .is_none();
    if empty {
        Ok(false)
    } else {
        Err(format!(
            "{} holds something other than a checkout of {}/{}; choose a new or empty folder",
            folder.display(),
            source.owner,
            source.repository
        ))
    }
}

/// Attaches to the builder's session, which opens with its first question.
async fn talk(context: &Context<'_>, builder: &Builder) -> CommandResult<()> {
    let checkout = builder.mounted.last().map_or_else(String::new, |checkout| {
        checkout
            .file_name()
            .map(|name| format!(" for the repository in the folder {} here", name.to_string_lossy()))
            .unwrap_or_default()
    });
    let target = context
        .client
        .ensure_session(
            &builder.name,
            SessionName::new(SESSION)?,
            SessionRequest {
                harness: None,
                model_selection: ModelSelection::default(),
                initial_prompt: Some(format!("I want to make a new agent{checkout}.")),
            },
            WaitPolicy::UntilConverged,
        )
        .await?;
    agent::sessions::attach(context.home.path(), &target).await?;
    Ok(())
}

/// What the person does once they leave the builder's session.
enum After {
    SetUp(PathBuf),
    Later,
    Remove,
}

/// Offers to set up an agent the builder made, to come back to it later, or to remove it.
async fn after(context: &Context<'_>, builder: &Builder) -> CommandResult<Option<Made>> {
    let mut choices: Vec<_> = made_since(builder)
        .into_iter()
        .map(|(root, manifest)| {
            let label = format!("Set up {}", manifest_label(&root, &manifest));
            Choice::new(After::SetUp(manifest), label)
        })
        .collect();
    choices.push(Choice::new(
        After::Later,
        "Not now. Keep the agent builder, and making a new agent goes back to it",
    ));
    choices.push(Choice::new(After::Remove, "Remove the agent builder"));
    match context
        .answers
        .choose_shown("--yes", "What next, now you have left the agent builder?", choices, 0)?
    {
        After::SetUp(manifest) => Ok(Some(Made {
            manifest,
            builder: builder.name.clone(),
        })),
        After::Later => Ok(None),
        After::Remove => {
            remove(context, &builder.name).await?;
            Ok(None)
        }
    }
}

/// The agents the builder wrote since it started, in the folders it mounts, each with the folder
/// it was found in.
fn made_since(builder: &Builder) -> Vec<(PathBuf, PathBuf)> {
    let mut made = Vec::new();
    for root in &builder.mounted {
        for found in found_manifests(root) {
            let new = found.file_name().is_some_and(|name| name == manifest::MANIFEST_FILE)
                && std::fs::metadata(&found)
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| modified >= builder.started);
            if new {
                made.push((root.clone(), found));
            }
        }
    }
    made
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    use super::{GithubSource, Protected, cloned_already, is_builder_agent};

    #[test]
    fn builder_agents_are_known_by_their_prefix() {
        assert!(is_builder_agent("agentctl-builder-1a2b"));
        assert!(!is_builder_agent("agent-builder"));
    }

    fn checkout_of(folder: &Path, origin: &str) {
        std::fs::create_dir_all(folder).expect("a folder");
        for arguments in [&["init", "--quiet"][..], &["remote", "add", "origin", origin]] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(folder)
                .args(arguments)
                .status()
                .expect("git");
            assert!(status.success());
        }
    }

    #[test]
    fn new_clones_into_a_new_or_empty_folder_and_uses_a_checkout_of_the_repository_there() {
        let home = tempfile::tempdir().expect("a home");
        let protected = Protected {
            user_home: Some(home.path().to_path_buf()),
            inside: vec![home.path().join("agents")],
        };
        let source = GithubSource::parse("https://github.com/digdir/digdir-agents").expect("an address");
        let repos = home.path().join("repos");

        assert_eq!(cloned_already(&protected, &source, &repos.join("new")), Ok(false));
        std::fs::create_dir_all(repos.join("empty")).expect("an empty folder");
        assert_eq!(cloned_already(&protected, &source, &repos.join("empty")), Ok(false));
        checkout_of(&repos.join("ours"), "git@github.com:digdir/digdir-agents.git");
        assert_eq!(cloned_already(&protected, &source, &repos.join("ours")), Ok(true));

        checkout_of(&repos.join("other"), "https://github.com/digdir/other.git");
        assert!(cloned_already(&protected, &source, &repos.join("other")).is_err());
        std::fs::create_dir_all(repos.join("notes")).expect("a folder");
        std::fs::write(repos.join("notes/todo.md"), "").expect("a file");
        assert!(cloned_already(&protected, &source, &repos.join("notes")).is_err());
        assert!(cloned_already(&protected, &source, &home.path().join("agents/new")).is_err());
    }
}
