//! Writing the person's variant and the agent's values, and applying them as `agentctl apply`
//! does: the review before, and after it what comes next and how to repeat it all.

use super::answers::{Answers, Choice};
use super::sign_in::login_command;
use super::sizing::Whole;
use super::{BrowserSignIn, Draft, SetupArguments, command_line, env_file, github, repeat};
use crate::{CommandError, CommandResult};
use agent::local::home::ControlPlaneHome;
use agent::{Error, control_api::Client, control_plane::ApplyRequest, manifest};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Writes the person's variant and the agent's values, then applies them.
pub(super) async fn finish(
    arguments: &SetupArguments,
    answers: &Answers,
    client: &Client,
    home: &ControlPlaneHome,
    draft: &Draft,
) -> CommandResult<()> {
    let cli = command_line(home);
    let variant = draft.variant_path();
    let values = draft.values_path(home);
    // What this run creates, removed again should the agent not be created.
    let mut created = Vec::new();
    match &draft.source.copy {
        Some(copy) if !copy.kept.exists() => created.push(copy.kept.clone()),
        None if !draft.changing => {
            ensure_ignored(answers, &variant)?;
            created.push(variant.clone());
        }
        _ => {}
    }
    if !values.exists() {
        created.push(values.clone());
    }
    // Values the file holds already and setup did not ask about, such as for another variant, stay.
    let mut kept = draft.access.values.clone();
    for (name, value) in agent::environment::read_or_empty(&values).await? {
        if !kept.iter().any(|(given, _)| *given == name) {
            kept.push((name, value));
        }
    }
    // A download's variant is written in it, then the download replaces the copy kept before.
    let written = std::fs::write(&draft.source.variant, variant_text(draft, &variant)?)
        .map_err(Error::from)
        .and_then(|()| {
            draft.source.copy.as_ref().map_or(Ok(()), |copy| {
                let relative = draft
                    .source
                    .variant
                    .strip_prefix(&copy.root)
                    .unwrap_or(&draft.source.variant);
                super::source::list_variants(&copy.root, &[relative.to_path_buf()])?;
                keep(&copy.root, &copy.kept)
            })
        })
        .and_then(|()| env_file::write(&values, &kept));
    if let Err(error) = written {
        remove(&created);
        return Err(error.into());
    }
    println!("Wrote {}", variant.display());
    if arguments.no_apply {
        println!(
            "Apply it with `{cli} apply -f {} --env-file {}`.",
            variant.display(),
            values.display()
        );
        print_repeat(&cli, answers, draft, home);
        return Ok(());
    }
    let mut request = crate::read_apply_request(variant, Some(values.clone()))?;
    request.create_only = !draft.changing;
    let name = apply(answers, client, &cli, request, arguments, &created).await?;
    print_next(&cli, &name, draft.agent.spec.ssh_access());
    print_repeat(&cli, answers, draft, home);
    Ok(())
}

/// The person's variant of `draft`, as written to `path`: what differs from the manifest it
/// extends, under a note on what the file is.
pub(super) fn variant_text(draft: &Draft, path: &Path) -> Result<String, Error> {
    let variant = manifest::variant_of(&draft.base, &draft.agent, &draft.source.extends)?;
    let yaml = serde_yaml_ng::to_string(&variant).map_err(Error::Yaml)?;
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(format!(
        "# Your own choices for this agent, over {} beside it, as `agentctl setup` wrote them.\n\
         # Change them here and run `agentctl apply -f {file}` in this folder.\n{yaml}",
        draft.source.extends
    ))
}

/// Refuses to write the person's variant where Git would commit it, unless they say to: it holds
/// their choices, such as folders on their machine, not the team's.
pub(super) fn ensure_ignored(answers: &Answers, variant: &Path) -> CommandResult<()> {
    if git_ignores(variant) != Some(false) {
        return Ok(());
    }
    let advice = format!(
        "Git does not ignore {}, so it could be committed with the team's files. Add `agent.*.yaml` to the \
         folder's .gitignore, or choose another name with --save-as.",
        variant.display()
    );
    if answers.interactive() && answers.confirm("--save-as", &[&advice, "Write it anyway?"], false)? {
        return Ok(());
    }
    Err(CommandError::Message(advice))
}

/// Whether Git ignores `path`, or `None` when it is in no checkout Git can read.
pub(super) fn git_ignores(path: &Path) -> Option<bool> {
    let (folder, file) = (path.parent()?, path.file_name()?);
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["check-ignore", "--quiet"])
        .arg(file)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Moves the download at `copy` to `kept`, replacing what an earlier setup kept there, which the
/// download holds the person's variants of already. The old copy goes only once the new one is in
/// place, and comes back should the move fail.
fn keep(copy: &Path, kept: &Path) -> Result<(), Error> {
    let parent = kept
        .parent()
        .ok_or_else(|| Error::Invalid(format!("{} has no parent folder", kept.display())))?;
    std::fs::create_dir_all(parent)?;
    let replaced = kept.exists().then(|| {
        let mut name = kept.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".replaced-{}", std::process::id()));
        parent.join(name)
    });
    if let Some(replaced) = &replaced {
        std::fs::rename(kept, replaced)?;
    }
    if let Err(error) = std::fs::rename(copy, kept) {
        if let Some(replaced) = &replaced {
            std::fs::rename(replaced, kept).ok();
        }
        return Err(error.into());
    }
    if let Some(replaced) = &replaced {
        std::fs::remove_dir_all(replaced).ok();
    }
    Ok(())
}

/// Removes what `created` names, after a failure.
fn remove(created: &[PathBuf]) {
    for path in created {
        if path.is_dir() {
            std::fs::remove_dir_all(path).ok();
        } else {
            std::fs::remove_file(path).ok();
        }
    }
}

/// Prints the command that sets up the same agent again without questions, such as on another
/// machine. It names the person's variant by the manifest it extends and its name, so it finds
/// the variant here and makes it again where there is none, its size exactly, and the values file
/// only when it is not the one setup reads on its own.
pub(super) fn print_repeat(cli: &str, answers: &Answers, draft: &Draft, home: &ControlPlaneHome) {
    if !answers.asked_anything() {
        return;
    }
    let mut pinned = Vec::new();
    if draft.changing
        && let Some(variant) = manifest::variant_from_filename(&draft.source.variant)
    {
        let base = draft.source.directory.join(&draft.source.extends);
        pinned.push(("--from".to_owned(), Some(vec![base.display().to_string()])));
        pinned.push(("--save-as".to_owned(), Some(vec![variant.to_string()])));
        pinned.push(("--name".to_owned(), Some(vec![draft.agent.metadata.name.clone()])));
    }
    // The presets are measured against the machine each time, so the size is pinned as it is.
    let size = Whole::of_resources(&draft.agent.spec.sandbox.resources);
    pinned.push(("--size".to_owned(), None));
    for (flag, value) in [
        ("--cpus", size.cpus),
        ("--memory-gib", size.memory_gib),
        ("--disk-gib", size.disk_gib),
    ] {
        pinned.push((flag.to_owned(), Some(vec![value.to_string()])));
    }
    let values = draft.values_path(home);
    let read_anyway = home.path().join(format!("{}.env", draft.agent.metadata.name));
    if !draft.access.values.is_empty() && values != read_anyway {
        pinned.push(("--env-file".to_owned(), Some(vec![values.display().to_string()])));
    }
    let command = repeat::command(cli, &repeat::given(), answers, pinned);
    println!("\nTo set up the same agent again without questions:\n\n  {command}");
}

/// Prints the commands that come next for a ready agent.
pub(super) fn print_next(cli: &str, name: &str, ssh: bool) {
    println!("\nagent/{name} is ready!\n\nNext steps:");
    let mut next = vec![(format!("{cli} attach session/main --agent {name}"), "start a session")];
    // `agentctl ssh` reaches only an agent whose manifest declares SSH access.
    if ssh {
        next.push((format!("{cli} ssh {name}"), "open a shell"));
    }
    next.push((format!("{cli} tui"), "see every agent"));
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
    /// Edit the values in the person's editor, then review it again.
    EditValues,
    Stop,
}

/// Shows what setup is about to write and apply, and asks whether to. When defaults `filled_in`
/// the answers, the person can change them.
pub(super) fn review(
    answers: &Answers,
    draft: &Draft,
    home: &ControlPlaneHome,
    filled_in: bool,
) -> CommandResult<Review> {
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
    lines.push(format!("GitHub:    {}", github(draft)));
    let unset = unset_values(draft);
    if !unset.is_empty() {
        lines.push(format!(
            "Not set:   {}, which the agent can do without",
            unset.join(", ")
        ));
    }
    let variant = &draft.variant_path();
    let verb = if draft.changing { "Changes" } else { "Writes" };
    lines.push(format!(
        "{verb}:{:width$}{}",
        "",
        variant.display(),
        width = 10 - verb.len()
    ));
    lines.push(format!("Values:    {}", draft.values_path(home).display()));
    lines.push(format!("Keep agent {} like this?", agent.metadata.name));
    let mut choices = vec![Choice::new(Review::Create, "Yes, keep it like this")];
    if filled_in {
        choices.push(Choice::new(Review::Change, "Change something first"));
    }
    choices.extend([
        Choice::new(
            Review::Edit,
            "Let me edit the variant definition in a text editor, then decide",
        ),
        Choice::new(
            Review::EditValues,
            "Let me edit the secrets and/or environment values first in a text editor, then decide",
        ),
        Choice::new(Review::Stop, "No, write nothing"),
    ]);
    // What the agent gets of this machine is shown even when nothing is asked.
    if answers.assumes_defaults() {
        println!("{}", lines[..lines.len() - 1].join("\n"));
    }
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

/// Applies `request` and waits until the agent is ready. When agentd cannot change the agent as
/// asked, as for a new image, it offers to create the agent again, which deletes its disk; a
/// script does so only with `--replace`. Should applying fail, what this run `created` goes again.
/// Should the agent not become ready, the person decides whether to keep it to look into or remove
/// it. Ctrl-C stops the wait and leaves it starting.
async fn apply(
    answers: &Answers,
    client: &Client,
    cli: &str,
    request: ApplyRequest,
    arguments: &SetupArguments,
    created: &[PathBuf],
) -> CommandResult<String> {
    let name = request.agent.metadata.name.clone();
    let applied = match client.apply(request.clone()).await {
        Err(Error::Rpc(error)) if error.is_immutable() && !request.create_only => {
            let question = format!(
                "agent/{name} cannot change as asked: {}.\nCreate it again, deleting its disk and sessions?",
                error.message
            );
            let again = arguments.replace
                || (answers.interactive()
                    && answers.choose(
                        "--replace",
                        &question,
                        vec![
                            Choice::new(false, "No, leave it as it is"),
                            Choice::new(true, "Yes, create it again").flag("--replace", &[]),
                        ],
                        0,
                    )?);
            if !again {
                return Err(CommandError::Message(format!(
                    "agent/{name} cannot change as asked: {}; pass --replace to create it again, deleting its \
                     disk and sessions",
                    error.message
                )));
            }
            client.delete(&name).await?;
            wait_until_deleted(client, &name, arguments.timeout).await?;
            client
                .apply(ApplyRequest {
                    create_only: true,
                    ..request
                })
                .await
        }
        other => other,
    };
    if let Err(error) = applied {
        remove(created);
        return Err(error.into());
    }
    println!("agent/{name} applied; waiting until it is ready");
    tokio::select! {
        waited = crate::wait_until_converged(client, &name, arguments.timeout) => match waited {
            Ok(_) => Ok(name),
            Err(error) => {
                eprintln!("\nagent/{name} did not become ready: {error}");
                remove_if_wanted(answers, client, cli, &name).await?;
                Err(CommandError::Message(format!("agent/{name} is not ready")))
            }
        },
        _ = tokio::signal::ctrl_c() => {
            eprintln!(
                "\nStopped waiting. agent/{name} keeps starting: follow it with `{cli} get agent/{name} --watch`, \
                 or remove it with `{cli} delete agent/{name}`."
            );
            Err(CommandError::Message("interrupted".into()))
        }
    }
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
        Err(_) => Err(CommandError::Message(format!("agent/{name} was not deleted in time"))),
    }
}

/// Offers to delete an agent that did not become ready; without a terminal, says how instead. The
/// person's variant stays, to look into or apply again.
async fn remove_if_wanted(answers: &Answers, client: &Client, cli: &str, name: &str) -> CommandResult<()> {
    let remove = answers.interactive()
        && answers.confirm(
            "--yes",
            &[
                &format!("Delete agent/{name}?"),
                "No keeps it, to look into or apply again once fixed.",
            ],
            false,
        )?;
    if !remove {
        eprintln!("`{cli} describe agent/{name}` shows why; `{cli} delete agent/{name}` removes it.");
        return Ok(());
    }
    client.delete(name).await?;
    eprintln!("Deleted agent/{name}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use zeroize::Zeroizing;

    use super::{git_ignores, github, harnesses, unset_values, variant_text};
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
        let save_as = "mine".parse().expect("a variant name");
        let mut draft = team_draft(&root.path().join("agent.yaml"), "a test", &save_as).expect("a draft");
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

    #[test]
    fn a_team_agents_variant_holds_the_persons_choices_where_git_ignores_it() {
        let repository = tempfile::tempdir().expect("a repository");
        let initialized = std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(repository.path())
            .status();
        if !initialized.is_ok_and(|status| status.success()) {
            return;
        }
        let folder = repository.path().join("agents/worker");
        std::fs::create_dir_all(folder.join("home")).expect("a home");
        std::fs::write(folder.join("agent.yaml"), MANIFEST).expect("a manifest");
        std::fs::write(folder.join(".gitignore"), "agent.*.yaml\n").expect("an ignore file");
        let save_as = "mine".parse().expect("a variant name");
        let mut draft = team_draft(&folder.join("agent.yaml"), "a test", &save_as).expect("a draft");
        draft.agent.spec.sandbox.resources.cpu = "6".parse().expect("a CPU count");
        let variant = folder.join("agent.mine.yaml");

        let text = variant_text(&draft, &variant).expect("a variant");
        std::fs::write(&variant, &text).expect("written");

        let resolved = agent::manifest::resolve(&variant).expect("the variant resolves").agent;
        assert_eq!(resolved, draft.agent, "{text}");
        assert_eq!(resolved.metadata.name, "worker-mine");
        assert!(!text.contains("harnesses"), "only what differs is written:\n{text}");
        assert_eq!(git_ignores(&variant), Some(true));
        assert_eq!(git_ignores(&folder.join("agent.yaml")), Some(false));
    }

    #[test]
    fn a_download_replaces_the_copy_kept_before() {
        let home = tempfile::tempdir().expect("a home");
        let kept = home.path().join("sources/github.com/team/agents");
        std::fs::create_dir_all(&kept).expect("an earlier copy");
        std::fs::write(kept.join("old.md"), "old").expect("an old file");
        let copy = home.path().join("run-1/download");
        std::fs::create_dir_all(&copy).expect("a download");
        std::fs::write(copy.join("new.md"), "new").expect("a new file");

        super::keep(&copy, &kept).expect("kept");
        assert!(kept.join("new.md").is_file());
        assert!(!kept.join("old.md").exists());
        assert!(!copy.exists());
        let left: Vec<_> = std::fs::read_dir(kept.parent().expect("a parent"))
            .expect("listed")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(left, ["agents"], "the old copy is removed");
    }
}
