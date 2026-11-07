//! `agentctl new`: making a new agent with the agent builder.
//!
//! The builder is an agent made from a definition agentctl carries, so it needs nothing downloaded
//! or checked out. It works on one checkout at a time and keeps running until the person deletes
//! it, so running `new` again goes back to the same conversation.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use agent::{
    Agent, Error, ModelSelection, RunState,
    control_api::Client,
    control_plane::WaitPolicy,
    local::home::ControlPlaneHome,
    manifest,
    sessions::{SessionName, SessionRequest},
};
use clap::Args;

use crate::{CommandError, CommandResult};

/// The builder's name, which its definition gives.
const NAME: &str = "agent-builder";

/// The folder in the control-plane home that holds the builder's definition, which agentd reads its
/// home and instructions from for as long as it runs.
const FOLDER: &str = "agent-builder";

/// The variant beside the definition that gives the builder the checkout it works on.
const VARIANT: &str = "agent.builder.yaml";

/// The folder in the person's home where the builder writes personal agents, as its definition
/// mounts it.
const DEFINITIONS: &str = "agent-definitions";

/// The builder's session, since it makes one agent at a time.
const SESSION: &str = "new-agent";

/// How long the builder may take to start, which the first time includes pulling its image.
const START_TIMEOUT: Duration = Duration::from_mins(30);

/// The builder's definition, from agents/agent-builder, without what only its image's build reads.
/// It follows this agentctl, and its image follows the default branch.
// CI skips Markdown changes, except where a build bundles them. Bundling another file from there
// needs it added to `bundled` in ci.yml, the paths in agentctl-image.yml and
// images/agentctl/Dockerfile.dockerignore.
const DEFINITION: [(&str, &[u8]); 3] = [
    (
        "agent.yaml",
        include_bytes!("../../../../agents/agent-builder/agent.yaml"),
    ),
    (
        "instructions.md",
        include_bytes!("../../../../agents/agent-builder/instructions.md"),
    ),
    (
        "home/.claude/settings.json",
        include_bytes!("../../../../agents/agent-builder/home/.claude/settings.json"),
    ),
];

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
    let variant = home.path().join(FOLDER).join(VARIANT);
    let agents = client.list_agents().await?;
    if let Some(builder) = agents.iter().find(|agent| agent.metadata.name == NAME) {
        same_builder(builder, &variant, &checkout)?;
        resume(client, &variant, &checkout).await?;
        println!("Back to the agent builder, where you left it.");
    } else {
        start(home, client, &variant, &checkout).await?;
    }
    talk(home, client).await?;
    println!(
        "The agent builder keeps running, and `agentctl new` in {} goes back to it.\nApply the agent it made with \
         `agentctl apply -f` and the manifest it named. Run `agentctl delete {NAME}` once you are done with it.",
        checkout.display()
    );
    Ok(())
}

/// The checkout `directory` names, or the current folder, as the folder it really is, so that `..`
/// and links still give it a name to mount it under.
fn checkout(directory: Option<PathBuf>) -> CommandResult<PathBuf> {
    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    let not_a_folder = || {
        CommandError::Message(format!(
            "{} is no folder. Name the checkout of the repository the new agent is for.",
            directory.display()
        ))
    };
    let checkout = std::fs::canonicalize(&directory).map_err(|_| not_a_folder())?;
    if !checkout.is_dir() {
        return Err(not_a_folder());
    }
    if let Some(home) = agent::local::home::user_home_directory() {
        refuse_home(&checkout, &home)?;
    }
    Ok(checkout)
}

/// Refuses `checkout` when it holds the person's `home` folder. The builder may write anywhere in
/// its checkout, and the home holds the person's settings and credentials.
fn refuse_home(checkout: &Path, home: &Path) -> CommandResult<()> {
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if home.starts_with(checkout) {
        return Err(CommandError::Message(format!(
            "the agent builder could write anywhere in {}, your home folder included. Run it in the repository's \
             checkout.",
            checkout.display()
        )));
    }
    Ok(())
}

/// Checks that `builder` is the one `new` started, from `variant`, on `checkout`, so `new` goes back
/// to it rather than to another conversation.
fn same_builder(builder: &Agent, variant: &Path, checkout: &Path) -> CommandResult<()> {
    let manifest = builder
        .status
        .provenance
        .as_ref()
        .and_then(|provenance| provenance.manifest_path.as_deref());
    if !manifest.is_some_and(|manifest| same_path(manifest, variant)) {
        return Err(CommandError::Message(format!(
            "an agent named {NAME} that `agentctl new` did not start is there already; delete or rename it to make a \
             new agent"
        )));
    }
    match checkout_of(builder) {
        Some(other) if !same_path(other, checkout) => Err(CommandError::Message(format!(
            "the agent builder is working on {}; run `agentctl delete {NAME}` to start it on {}",
            other.display(),
            checkout.display()
        ))),
        _ => Ok(()),
    }
}

/// The checkout `builder` works on, the folder it mounts in its working directory.
fn checkout_of(builder: &Agent) -> Option<&Path> {
    let working = agent::sandbox::platform::working_directory(&builder.spec.sandbox.platform.os)?;
    builder.spec.sandbox.mounts.iter().find_map(|mount| match mount {
        agent::MountSpec::Bind { source, target, .. }
            if Path::new(target.as_str()).parent() == Some(Path::new(working)) =>
        {
            Some(source.as_path())
        }
        _ => None,
    })
}

/// Where the builder finds `checkout`: in its working directory, under the checkout's own folder
/// name, as its instructions say, so the repositories it clones go beside it.
fn checkout_target(os: &str, checkout: &Path) -> CommandResult<String> {
    let working = agent::sandbox::platform::working_directory(os)
        .ok_or_else(|| Error::Invalid(format!("cannot mount a checkout into a {os} agent")))?;
    let name = checkout
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .replace(|character: char| character.is_control(), "-")
        })
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::Invalid(format!("{} has no folder name to mount it under", checkout.display())))?;
    Ok(format!("{working}/{name}"))
}

/// Whether `left` and `right` are the same file or folder, however each is spelled.
fn same_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Starts the builder on `checkout` from its definition, written beside `variant`.
async fn start(home: &ControlPlaneHome, client: &Client, variant: &Path, checkout: &Path) -> CommandResult<()> {
    home.prepare()?;
    write_definition(variant, checkout)?.keep()?;
    if let Some(user_home) = agent::local::home::user_home_directory() {
        std::fs::create_dir_all(user_home.join(DEFINITIONS)).map_err(Error::from)?;
    }
    eprintln!("Starting the agent builder on {}.", checkout.display());
    let mut request = crate::read_apply_request(variant.to_path_buf(), None)?;
    request.create_only = true;
    let started = match client.apply(request).await {
        Ok(_) => interruptible(crate::wait_until_converged(client, NAME, START_TIMEOUT))
            .await
            .map(drop),
        Err(error) => Err(error.into()),
    };
    if started.is_err() {
        // A builder that did not start would hold the name, and `new` would go back to it.
        client.delete(NAME).await.ok();
    }
    started
}

/// Goes back to the builder on `checkout`. It gets this agentctl's definition, unless that changes
/// what only making it again can change, and starts if it was stopped.
async fn resume(client: &Client, variant: &Path, checkout: &Path) -> CommandResult<()> {
    let definition = write_definition(variant, checkout)?;
    let applied = match crate::read_apply_request(variant.to_path_buf(), None) {
        Ok(request) => client.apply(request).await.map(drop),
        Err(error) => Err(error),
    };
    match applied {
        Ok(()) => definition.keep()?,
        Err(Error::Rpc(error)) if error.is_immutable() => {
            definition.restore()?;
            eprintln!(
                "This agentctl's agent builder differs from yours in what takes making it again ({}). `agentctl delete \
                 {NAME}` and `agentctl new` make a new one.",
                error.message
            );
        }
        Err(error) => {
            definition.restore()?;
            return Err(error.into());
        }
    }
    client.set_run_state(NAME, RunState::Running).await?;
    interruptible(async {
        crate::wait_until_converged(client, NAME, START_TIMEOUT)
            .await
            .map_err(|error| {
                CommandError::Message(format!(
                    "the agent builder did not start: {error}. `agentctl delete {NAME}` and `agentctl new` make a \
                     new one."
                ))
            })
    })
    .await
    .map(drop)
}

/// Writes the builder's definition into the folder of `variant`, and `variant`, which mounts
/// `checkout` beside the folders the definition mounts. The folder is written beside the one there
/// and swapped in, so a running builder never reads half of one.
fn write_definition(variant: &Path, checkout: &Path) -> CommandResult<Definition> {
    let folder = variant
        .parent()
        .ok_or_else(|| Error::Invalid("the agent builder's variant has no folder".into()))?;
    let staged = folder.with_file_name(format!("{FOLDER}.new"));
    let earlier = folder.with_file_name(format!("{FOLDER}.earlier"));
    for leftover in [&staged, &earlier] {
        if leftover.exists() {
            std::fs::remove_dir_all(leftover).map_err(Error::from)?;
        }
    }
    for (path, contents) in DEFINITION {
        let path = staged.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::from)?;
        }
        std::fs::write(path, contents).map_err(Error::from)?;
    }
    let earlier = if folder.exists() {
        std::fs::rename(folder, &earlier).map_err(Error::from)?;
        Some(earlier)
    } else {
        None
    };
    std::fs::rename(&staged, folder).map_err(Error::from)?;
    let definition = Definition {
        folder: folder.to_path_buf(),
        earlier,
    };
    if let Err(error) = write_variant(variant, checkout) {
        definition.restore()?;
        return Err(error);
    }
    Ok(definition)
}

/// Writes `variant`, which mounts `checkout` beside the folders the definition beside it mounts.
fn write_variant(variant: &Path, checkout: &Path) -> CommandResult<()> {
    let definition = manifest::resolve(&variant.with_file_name(manifest::MANIFEST_FILE))?.agent;
    let target = checkout_target(&definition.spec.sandbox.platform.os, checkout)?;
    // Mounts replace the definition's, so the variant lists them all.
    let mut mounts = serde_json::to_value(definition.spec.sandbox.mounts).map_err(Error::from)?;
    if let Some(mounts) = mounts.as_array_mut() {
        mounts.push(serde_json::json!({
            "type": "bind",
            "source": checkout,
            "target": target,
            "readOnly": false,
        }));
    }
    let text = serde_yaml_ng::to_string(&serde_json::json!({
        "apiVersion": manifest::API_VERSION,
        "kind": manifest::VARIANT_KIND,
        "extends": manifest::MANIFEST_FILE,
        "metadata": { "name": NAME },
        "spec": { "sandbox": { "mounts": mounts } },
    }))
    .map_err(Error::from)?;
    std::fs::write(
        variant,
        format!("# Written by `agentctl new`, which replaces it.\n{text}"),
    )
    .map_err(Error::from)?;
    Ok(())
}

/// The builder's definition folder, written anew with the earlier one set aside, until `new` keeps
/// the new one or puts the earlier one back.
#[must_use]
struct Definition {
    folder: PathBuf,
    earlier: Option<PathBuf>,
}

impl Definition {
    fn keep(self) -> CommandResult<()> {
        if let Some(earlier) = self.earlier {
            std::fs::remove_dir_all(earlier).map_err(Error::from)?;
        }
        Ok(())
    }

    fn restore(self) -> CommandResult<()> {
        if let Some(earlier) = self.earlier {
            std::fs::remove_dir_all(&self.folder).map_err(Error::from)?;
            std::fs::rename(earlier, &self.folder).map_err(Error::from)?;
        }
        Ok(())
    }
}

/// Runs `wait` until it ends or the person presses Ctrl-C. Once agentctl listens for Ctrl-C, Ctrl-C
/// no longer ends it, so each wait after the first listens too.
async fn interruptible<T>(wait: impl Future<Output = CommandResult<T>>) -> CommandResult<T> {
    tokio::select! {
        result = wait => result,
        Ok(()) = tokio::signal::ctrl_c() => Err(CommandError::Message("interrupted".into())),
    }
}

/// Attaches to the builder's session, which opens with its first question.
async fn talk(home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    let target = interruptible(async {
        client
            .ensure_session(
                NAME,
                SessionName::new(SESSION)?,
                SessionRequest {
                    harness: None,
                    model_selection: ModelSelection::default(),
                    initial_prompt: Some("I want to make a new agent for the repository you have checked out.".into()),
                },
                WaitPolicy::UntilConverged,
            )
            .await
            .map_err(CommandError::from)
    })
    .await?;
    agent::sessions::attach(home.path(), &target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use agent::{MountSpec, manifest};

    use super::{NAME, VARIANT, checkout, checkout_of, refuse_home, same_builder, write_definition};

    #[test]
    fn the_builder_gets_the_checkout_beside_its_own_mounts() {
        let home = tempfile::tempdir().expect("home");
        let checkout = tempfile::tempdir().expect("checkout");
        let variant = home.path().join("agent-builder").join(VARIANT);
        write_definition(&variant, checkout.path())
            .expect("definition")
            .keep()
            .expect("kept");
        let agent = manifest::resolve(&variant).expect("resolves").agent;

        let definition = manifest::resolve(&variant.with_file_name(manifest::MANIFEST_FILE))
            .expect("definition resolves")
            .agent;

        assert_eq!(agent.metadata.name, NAME);
        assert_eq!(checkout_of(&agent), Some(checkout.path()));
        let mounts = &agent.spec.sandbox.mounts;
        assert_eq!(
            mounts[..mounts.len() - 1],
            definition.spec.sandbox.mounts[..],
            "the definition's mounts stay"
        );
        let Some(MountSpec::Bind { target, .. }) = mounts.last() else {
            panic!("the checkout is mounted last");
        };
        let name = checkout.path().file_name().expect("name").to_string_lossy();
        assert!(target.as_str().ends_with(&format!("/{name}")), "{target:?}");
    }

    #[test]
    fn only_the_builder_new_started_on_this_checkout_is_gone_back_to() {
        let home = tempfile::tempdir().expect("home");
        let checkout = tempfile::tempdir().expect("checkout");
        let other = tempfile::tempdir().expect("other checkout");
        let variant = home.path().join("agent-builder").join(VARIANT);
        write_definition(&variant, checkout.path())
            .expect("definition")
            .keep()
            .expect("kept");
        let mut builder = manifest::resolve(&variant).expect("resolves").agent;

        assert!(
            same_builder(&builder, &variant, checkout.path()).is_err(),
            "not applied by new"
        );
        builder.status.provenance = Some(manifest::Provenance {
            source_directory: variant.parent().expect("folder").to_path_buf(),
            manifest_path: Some(variant.clone()),
            env_file: None,
        });
        assert!(same_builder(&builder, &variant, checkout.path()).is_ok());
        assert!(
            same_builder(&builder, &variant, other.path()).is_err(),
            "another checkout"
        );
        assert!(same_builder(&builder, Path::new("/elsewhere/agent.yaml"), checkout.path()).is_err());

        let respelled = home.path().join("agent-builder").join("..").join("agent-builder");
        assert!(
            same_builder(&builder, &respelled.join(VARIANT), checkout.path()).is_ok(),
            "the variant however its path is spelled"
        );
    }

    #[test]
    fn a_definition_the_builder_cannot_take_is_put_back() {
        let home = tempfile::tempdir().expect("home");
        let checkout = tempfile::tempdir().expect("checkout");
        let variant = home.path().join("agent-builder").join(VARIANT);
        let earlier = variant.with_file_name("earlier.md");
        write_definition(&variant, checkout.path())
            .expect("definition")
            .keep()
            .expect("kept");
        std::fs::write(&earlier, "earlier").expect("mark the definition");

        write_definition(&variant, checkout.path())
            .expect("definition")
            .restore()
            .expect("restored");
        assert!(earlier.exists(), "the earlier definition is back");
        assert!(variant.exists());

        write_definition(&variant, checkout.path())
            .expect("definition")
            .keep()
            .expect("kept");
        assert!(!earlier.exists(), "the new definition replaces it");
        let names: Vec<_> = std::fs::read_dir(home.path())
            .expect("home")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["agent-builder"], "nothing is left beside it");
    }

    #[test]
    fn the_checkout_is_the_folder_a_path_leads_to() {
        let folder = tempfile::tempdir().expect("folder");
        let repository = folder.path().join("repository");
        std::fs::create_dir_all(repository.join("src")).expect("checkout");

        assert_eq!(
            checkout(Some(repository.join("src").join(".."))).expect("checkout"),
            std::fs::canonicalize(&repository).expect("canonical")
        );
        assert!(checkout(Some(repository.join("missing"))).is_err());
    }

    #[test]
    fn a_checkout_that_holds_the_home_folder_is_refused() {
        let folder = tempfile::tempdir().expect("folder");
        let folder = std::fs::canonicalize(folder.path()).expect("canonical");
        let home = folder.join("home");
        std::fs::create_dir_all(home.join("repository")).expect("home");

        assert!(refuse_home(&home, &home).is_err(), "the home folder");
        assert!(refuse_home(&folder, &home).is_err(), "a folder that holds it");
        let root = folder.ancestors().last().expect("root");
        assert!(refuse_home(root, &home).is_err(), "the root");
        assert!(refuse_home(&home.join("repository"), &home).is_ok(), "a checkout in it");
        assert!(
            refuse_home(&folder.join("elsewhere"), &home).is_ok(),
            "a checkout beside it"
        );
    }
}
