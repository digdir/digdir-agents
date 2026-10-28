//! Creating the agent: the review, writing its folder, swapping it for one it replaces, creating
//! and waiting for the agent, and saying what comes next and how to repeat it all.

use super::answers::{Answers, Choice};
use super::sign_in::login_command;
use super::sizing::Whole;
use super::{
    BrowserSignIn, Draft, SetupArguments, bundle, command_line, env_file, existing, ide, image, repeat, staging,
};
use crate::{CommandError, CommandResult};
use agent::local::home::ControlPlaneHome;
use agent::{Agent, Error, manifest};
use agent::{control_api::Client, control_plane::ApplyRequest};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Writes the agent's folder, creates the agent, and connects it to SSH and an editor.
pub(super) async fn finish(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    home: &ControlPlaneHome,
    staging: &staging::Staging,
    draft: &mut Draft,
    folder: &Path,
) -> CommandResult<()> {
    let cli = command_line(home);
    let backup = staging.path().join("replaced");
    let (manifest_path, on_failure) = if let Some(replaced) = draft.replace.clone() {
        // The new folder is written whole before anything of the old one goes.
        let staged = staging.path().join("replacement");
        write_folder(draft, &staged)?;
        existing::swap(client, &replaced, &staged, &backup, arguments.timeout).await?;
        (folder.join(manifest::MANIFEST_FILE), OnFailure::Restore(backup))
    } else {
        (write_folder(draft, folder)?, OnFailure::Remove)
    };
    println!("Wrote {}", folder.display());
    if arguments.no_apply {
        println!("Create the agent with `{cli} apply -f {}`.", manifest_path.display());
        print_repeat(&cli, answers, draft, folder);
        return Ok(());
    }
    let name = create(
        answers,
        client,
        &cli,
        &draft.agent,
        folder,
        &manifest_path,
        arguments.timeout,
        on_failure,
    )
    .await?;
    if !arguments.no_ssh_config {
        crate::install_ssh_config(home)?;
    }
    ide::offer(answers, arguments.ide, client, &name).await?;
    print_next(&cli, &name);
    print_repeat(&cli, answers, draft, folder);
    Ok(())
}

/// Prints the command that sets up the same agent again without questions, when questions made
/// this one, reading the values they asked for from the agent's `.env`.
pub(super) fn print_repeat(cli: &str, answers: &Answers, draft: &Draft, folder: &Path) {
    if !answers.asked_anything() {
        return;
    }
    let env_file = (!draft.access.values.is_empty()).then(|| folder.join(agent::control_plane::ENV_FILE));
    let command = repeat::command(cli, &repeat::given(), answers, env_file.as_deref());
    println!(
        "
To set up the same agent again without questions, such as on another machine:"
    );
    println!("  {command}");
    if let Some(env_file) = env_file {
        println!(
            "It reads the values you gave from {}; copy that file along, and keep it private.",
            env_file.display()
        );
    }
}

/// Prints the commands that come next for a ready agent.
pub(super) fn print_next(cli: &str, name: &str) {
    println!("\nagent/{name} is ready. Next:");
    let next = [
        (format!("{cli} attach session/main --agent {name}"), "start a session"),
        (format!("{cli} ssh {name}"), "open a shell"),
        (format!("{cli} tui"), "see every agent"),
    ];
    let width = next.iter().map(|(command, _)| command.len()).max().unwrap_or_default();
    for (command, what) in next {
        println!("  {command:<width$}   {what}");
    }
}

/// What the person does with the agent the review shows.
pub(super) enum Review {
    Create,
    /// Edit its manifest first, for anything the questions leave at its recommended value, then
    /// review it again.
    Edit,
    Stop,
}

pub(super) fn review(answers: &Answers, draft: &Draft, folder: &Path) -> CommandResult<Review> {
    let agent = &draft.agent;
    let resources = Whole::of_resources(&agent.spec.sandbox.resources);
    let default = agent.spec.default_harness().map_or_else(
        || "none".to_owned(),
        |harness| {
            let name = harness.kind.display_name();
            if draft.access.sign_in_to.contains(&harness.kind) {
                format!("{name}, signed in next")
            } else if draft.access.reused_logins.contains(&harness.kind) {
                format!("{name}, with the login agentctl holds already")
            } else if harness.auth == agent::HarnessAuthMode::Mediated {
                format!("{name}, waiting for `agentctl {} login`", login_command(harness.kind))
            } else {
                name.to_owned()
            }
        },
    );
    let mut lines = vec![
        format!("Agent {}, from {}:", agent.metadata.name, draft.source.origin),
        format!("Machine:  {}", resources.describe()),
        format!("Harness:  {default}"),
    ];
    for mount in &agent.spec.sandbox.mounts {
        if let agent::MountSpec::Bind { source, target, .. } = mount {
            lines.push(format!("Mount:    {} at {}", source.display(), target.as_str()));
        }
    }
    let github = match &draft.access.github_browser {
        Some(BrowserSignIn { account: Some(account) }) => format!("signed in through the browser as {account}"),
        Some(BrowserSignIn { account: None }) => "signed in through the browser".to_owned(),
        None => draft
            .access
            .github_account
            .as_ref()
            .map_or_else(|| "none".to_owned(), |account| format!("as {account}")),
    };
    if draft.built_here.is_some() {
        lines.push("Image:    built on this machine from the standard agent's Dockerfile".to_owned());
    }
    lines.push(format!("GitHub:   {github}"));
    if let Some(replaced) = &draft.replace {
        let mut what = format!("Replaces: {}, removed first", replaced.folder.display());
        if let Some(agent) = replaced.agent_here() {
            what = format!("{what}; agent/{agent} is deleted with its disk");
        }
        lines.push(what);
    }
    lines.push(format!("Folder:   {}", folder.display()));
    lines.push(format!("Create agent {}?", agent.metadata.name));
    let question = lines.join("\n");
    Ok(answers.choose(
        "--yes",
        &question,
        vec![
            Choice::new(Review::Create, "Yes, create it"),
            Choice::new(Review::Edit, "Edit its agent.yaml first, then decide"),
            Choice::new(Review::Stop, "No, create nothing"),
        ],
        0,
    )?)
}

/// Writes the agent's folder: its manifest, the files it reads, and its `.env`.
pub(super) fn write_folder(draft: &mut Draft, folder: &Path) -> CommandResult<PathBuf> {
    if folder.exists() {
        return Err(CommandError::Message(format!("{} already exists", folder.display())));
    }
    std::fs::create_dir_all(folder).map_err(Error::from)?;
    let written = (|| {
        let mut agent = draft.agent.clone();
        bundle_into(&mut agent, draft, folder)?;
        draft.agent = agent;
        let path = folder.join(manifest::MANIFEST_FILE);
        std::fs::write(&path, manifest_text(&draft.agent, draft)?)?;
        env_file::write(folder, &draft.access.values)?;
        Ok::<_, Error>(path)
    })();
    if written.is_err() {
        std::fs::remove_dir_all(folder).ok();
    }
    Ok(written?)
}

/// Copies what `agent` reads into `folder`, and the sources of its image when it is built here.
pub(super) fn bundle_into(agent: &mut Agent, draft: &Draft, folder: &Path) -> Result<(), Error> {
    bundle::bundle(agent, &draft.source.directory, folder)?;
    if draft.built_here.is_some()
        && let Some(sources) = &draft.source.image_sources
    {
        image::copy_sources(sources, &folder.join(image::SOURCES))?;
        agent.spec.sandbox.image = image::built_from(PathBuf::from(image::SOURCES));
    }
    Ok(())
}

pub(super) fn manifest_text(agent: &Agent, draft: &Draft) -> Result<String, Error> {
    let yaml = serde_yaml_ng::to_string(agent).map_err(Error::Yaml)?;
    let built_here = draft.built_here.as_ref().map_or_else(String::new, |published| {
        format!(
            "# Its image is built here from {}/, in place of {published},\n\
             # which could not be pulled. Once it can, a new agent can use it instead.\n",
            image::SOURCES
        )
    });
    Ok(format!(
        "# Written by `agentctl setup` from {}.\n\
         # Change it and run `agentctl apply` in this folder; values it declares are in .env.\n\
         {built_here}{yaml}",
        draft.source.origin
    ))
}

/// What becomes of the agent's folder when the agent cannot be created from it.
pub(super) enum OnFailure {
    /// The folder was written for it, so it goes.
    Remove,
    /// The folder replaced the one moved aside here, which comes back.
    Restore(PathBuf),
}

/// Creates the agent from its folder and waits until it is ready. Should creating it fail, the
/// folder is removed again, or the one it replaced restored. Should it not become ready, the
/// person decides whether to keep it to look into or remove it and its folder. Ctrl-C stops the
/// wait and leaves it starting.
#[allow(
    clippy::too_many_arguments,
    reason = "each is one input of creating the agent; grouping them would only rename them"
)]
pub(super) async fn create(
    answers: &Answers,
    client: &Client,
    cli: &str,
    agent: &Agent,
    folder: &Path,
    manifest_path: &Path,
    timeout: Duration,
    on_failure: OnFailure,
) -> CommandResult<String> {
    let applied = client
        .apply(ApplyRequest {
            source_directory: folder.to_path_buf(),
            manifest_path: Some(manifest_path.to_path_buf()),
            env_file: None,
            create_only: true,
            agent: agent.clone(),
        })
        .await;
    let name = match applied {
        Ok(applied) => applied.metadata.name,
        Err(error) => {
            match on_failure {
                OnFailure::Remove => {
                    std::fs::remove_dir_all(folder).ok();
                    eprintln!("The agent was not created, so {} is removed again.", folder.display());
                }
                OnFailure::Restore(backup) => {
                    std::fs::remove_dir_all(folder).ok();
                    if existing::move_folder(&backup, folder).is_ok() {
                        eprintln!(
                            "The agent was not created; {} holds the definition it replaced again.",
                            folder.display()
                        );
                    }
                }
            }
            return Err(error.into());
        }
    };
    println!("agent/{name} created; starting it");
    tokio::select! {
        waited = crate::wait_until_converged(client, &name, timeout) => match waited {
            Ok(_) => Ok(name),
            Err(error) => {
                eprintln!("\nagent/{name} did not become ready: {error}");
                remove_if_wanted(answers, client, cli, &name, folder).await?;
                Err(CommandError::Message(format!("agent/{name} is not ready")))
            }
        },
        _ = tokio::signal::ctrl_c() => {
            eprintln!(
                "\nStopped waiting. agent/{name} keeps starting: follow it with `{cli} get agent/{name} --watch`, \
                 or remove it with `{cli} delete agent/{name}` and `rm -r {}`.",
                folder.display()
            );
            Err(CommandError::Message("interrupted".into()))
        }
    }
}

/// Offers to delete an agent that did not become ready, and its folder; without a terminal, says
/// how instead.
pub(super) async fn remove_if_wanted(
    answers: &Answers,
    client: &Client,
    cli: &str,
    name: &str,
    folder: &Path,
) -> CommandResult<()> {
    let how = format!(
        "`{cli} describe agent/{name}` shows why; `{cli} delete agent/{name}` and `rm -r {}` remove it",
        folder.display()
    );
    let remove = answers.interactive()
        && answers.confirm(
            "--yes",
            &[
                &format!("Delete agent/{name} and {}?", folder.display()),
                "No keeps both, to look into or retry with `agentctl apply` in the folder.",
            ],
            false,
        )?;
    if !remove {
        eprintln!("{how}.");
        return Ok(());
    }
    client.delete(name).await?;
    std::fs::remove_dir_all(folder).map_err(Error::from)?;
    eprintln!("Deleted agent/{name} and removed {}.", folder.display());
    Ok(())
}
