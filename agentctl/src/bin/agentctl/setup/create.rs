//! Creating the agent: the review, writing its folder, swapping it for one it replaces, creating
//! and waiting for the agent, and saying what comes next and how to repeat it all.

use super::answers::{Answers, Choice};
use super::sign_in::login_command;
use super::sizing::Whole;
use super::{
    BrowserSignIn, Draft, SetupArguments, bundle, command_line, env_file, existing, github, ide, image, repeat, staging,
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
    /// Answer the questions the defaults answered, then review it again.
    Change,
    /// Edit its manifest first, for anything the questions leave at its recommended value, then
    /// review it again.
    Edit,
    Stop,
}

/// Shows what setup is about to create and asks whether to. When defaults `filled_in` the answers,
/// the person can change them.
pub(super) fn review(answers: &Answers, draft: &Draft, folder: &Path, filled_in: bool) -> CommandResult<Review> {
    let agent = &draft.agent;
    let resources = Whole::of_resources(&agent.spec.sandbox.resources);
    let mut lines = vec![
        format!("Agent {}, from {}:", agent.metadata.name, draft.source.origin),
        format!("Machine:   {}", resources.describe()),
        format!("Harnesses: {}", harnesses(draft)),
    ];
    let shared: Vec<String> = agent
        .spec
        .sandbox
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            agent::MountSpec::Bind {
                source,
                target,
                read_only,
            } => {
                let access = if *read_only { "read-only" } else { "read-write" };
                Some(format!("{} ({access}), at {}", source.display(), target.as_str()))
            }
            agent::MountSpec::Tmpfs { .. } => None,
        })
        .collect();
    if shared.is_empty() {
        lines.push("Shared:    nothing; the agent works on its own copies".to_owned());
    }
    lines.extend(shared.iter().map(|shared| format!("Shared:    {shared}")));
    if draft.built_here.is_some() {
        lines.push("Image:     built on this machine from the standard agent's Dockerfile".to_owned());
    }
    lines.push(format!("GitHub:    {}", github(draft)));
    let unset = unset_values(draft);
    if !unset.is_empty() {
        lines.push(format!(
            "Not set:   {}, which the agent can do without",
            unset.join(", ")
        ));
    }
    if let Some(replaced) = &draft.replace {
        let mut what = format!("Replaces:  {}, removed first", replaced.folder.display());
        if let Some(agent) = replaced.agent_here() {
            what = format!("{what}; agent/{agent} is deleted with its disk");
        }
        lines.push(what);
    }
    lines.push(format!("Folder:    {}", folder.display()));
    lines.push(format!("Set up agent {} like this?", agent.metadata.name));
    let mut choices = vec![Choice::new(Review::Create, "Yes, set it up")];
    if filled_in {
        choices.push(Choice::new(Review::Change, "Change something"));
    }
    choices.extend([
        Choice::new(Review::Edit, "Edit its agent.yaml first, then decide"),
        Choice::new(Review::Stop, "No, set up nothing"),
    ]);
    Ok(answers.choose("--yes", &lines.join("\n"), choices, 0)?)
}

/// The harnesses `draft` has, the default first, each with how it signs in.
fn harnesses(draft: &Draft) -> String {
    let spec = &draft.agent.spec;
    let default = spec.default_harness().map(|harness| harness.kind);
    let mut harnesses: Vec<_> = spec.harnesses.iter().collect();
    harnesses.sort_by_key(|harness| Some(harness.kind) != default);
    let described: Vec<String> = harnesses
        .into_iter()
        .map(|harness| {
            let kind = harness.kind;
            let mut notes = Vec::new();
            if Some(kind) == default && spec.harnesses.len() > 1 {
                notes.push("default".to_owned());
            }
            if draft.access.sign_in_to.contains(&kind) {
                notes.push("signed in next".to_owned());
            } else if draft.access.reused_logins.contains(&kind) {
                notes.push("with the login agentctl holds".to_owned());
            } else if harness.auth == agent::HarnessAuthMode::Mediated {
                notes.push(format!("waiting for {}", login_command(&draft.agent, kind)));
            }
            if notes.is_empty() {
                kind.display_name().to_owned()
            } else {
                format!("{} ({})", kind.display_name(), notes.join(", "))
            }
        })
        .collect();
    described.join(", ")
}

/// How `draft` reaches GitHub, and who its commits are from, which follows from it.
fn github(draft: &Draft) -> String {
    if let Some(BrowserSignIn { account }) = &draft.access.github_browser {
        let account = account
            .as_ref()
            .map(|account| format!(" as {account}"))
            .unwrap_or_default();
        return format!("signed in through the browser{account} next; commits as that account");
    }
    let value = |name: &str| {
        draft
            .access
            .values
            .iter()
            .find(|(given, _)| given == name)
            .map(|(_, value)| value.as_str())
    };
    let commits = match (value(github::GIT_USER_NAME), value(github::GIT_USER_EMAIL)) {
        (Some(name), Some(email)) => Some(format!("commits as {name} <{email}>")),
        _ => None,
    };
    let access = draft
        .access
        .github_account
        .as_ref()
        .map_or_else(|| "none".to_owned(), |account| format!("as {account}"));
    commits.map_or_else(|| access.clone(), |commits| format!("{access}; {commits}"))
}

/// The values `draft` declares that are left empty, which only optional ones can be. GitHub's own
/// are left out, as the GitHub line says what becomes of them.
fn unset_values(draft: &Draft) -> Vec<String> {
    let spec = &draft.agent.spec;
    let declared = spec
        .secrets
        .iter()
        .map(agent::SecretSpec::source)
        .chain(spec.environment.iter().map(agent::EnvironmentSpec::source));
    declared
        .filter(|name| ![github::TOKEN_VARIABLE, github::GIT_USER_NAME, github::GIT_USER_EMAIL].contains(name))
        .filter(|name| !draft.access.values.iter().any(|(given, _)| given == name))
        .map(str::to_owned)
        .collect()
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use zeroize::Zeroizing;

    use super::{github, harnesses, unset_values};
    use crate::setup::{BrowserSignIn, source::team_draft};

    const MANIFEST: &str = "apiVersion: agents.platform/v1alpha1\nkind: Agent\nmetadata: {name: worker}\nspec:\n  \
                            sandbox:\n    image: {type: reference, reference: example.com/agent:latest}\n    \
                            platform: {os: linux}\n    resources: {cpu: '2', memory: 4Gi, rootFilesystem: \
                            {capacity: 20Gi, mode: layered}}\n  home: {source: home}\n  harnesses:\n    - {type: \
                            claudeCode, auth: mediated}\n    - {type: pi, default: true}\n  environment:\n    - \
                            {name: GIT_USER_NAME}\n    - {name: GIT_USER_EMAIL}\n  secrets:\n    - {environment: \
                            AIVAR_API_KEY, optional: true, allowedHosts: [api.aivar.no]}\n  network: {mode: \
                            mediated, allow: all}\n";

    #[test]
    fn the_summary_names_the_default_harness_first_and_what_github_and_commits_come_to() {
        let root = tempfile::tempdir().expect("a root");
        std::fs::create_dir_all(root.path().join("home")).expect("a home");
        std::fs::write(root.path().join("agent.yaml"), MANIFEST).expect("a manifest");
        let mut draft = team_draft(&root.path().join("agent.yaml"), "a test").expect("a draft");
        draft.access.reused_logins.push(agent::Harness::ClaudeCode);

        assert_eq!(
            harnesses(&draft),
            "Pi (default), Claude Code (with the login agentctl holds)"
        );
        assert_eq!(github(&draft), "none");
        assert_eq!(unset_values(&draft), ["AIVAR_API_KEY"]);

        for (name, value) in [("GIT_USER_NAME", "Ada"), ("GIT_USER_EMAIL", "ada@example.com")] {
            draft.access.values.push((name.into(), Zeroizing::new(value.into())));
        }
        assert_eq!(github(&draft), "none; commits as Ada <ada@example.com>");
        draft.access.github_account = Some("ada-agent".into());
        assert_eq!(github(&draft), "as ada-agent; commits as Ada <ada@example.com>");
        draft.access.github_browser = Some(BrowserSignIn { account: None });
        assert_eq!(
            github(&draft),
            "signed in through the browser next; commits as that account"
        );
    }
}
