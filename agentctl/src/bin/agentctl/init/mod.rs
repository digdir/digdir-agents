//! `agentctl init`: sets up an Agent for the current directory.
//!
//! Asks for the host logins an Agent needs, writes an agent directory under `agents/` when the
//! working tree has no manifest, and creates the Agent.

use std::path::{Path, PathBuf};

use agent::{Error, control_api::Client, harness::AuthenticationStatus, local::home::ControlPlaneHome};

use crate::{CommandResult, prompt};

pub(crate) mod image;
mod sizing;
pub(crate) mod template;
pub(crate) mod toolchains;

use crate::host::Docker;
use image::Tier;
use template::{Checkout, Choices, Github};

/// Where a newcomer gets Docker.
const DOCKER_DESKTOP: &str = "https://docs.docker.com/get-started/get-docker/";
/// How often the wizard looks again for Docker to start.
const DOCKER_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// What has been decided before `run` starts.
#[derive(Clone, Debug)]
pub(crate) enum Start {
    /// Nothing: `agentctl init` was run.
    Ask,
    /// The person agreed to set up an agent here, so it is not asked again.
    Confirmed,
    /// Write a new agent directory, whatever is here already.
    New,
    /// Create the agent from this existing manifest.
    From(PathBuf),
    /// Choose one of the existing manifests in the TUI's create form.
    Form,
}

/// What to do where agent manifests exist but no agent runs from them.
#[derive(Clone, Debug)]
pub(crate) enum Choice {
    /// Go on with `run` from this start.
    Start(Start),
    /// Show agentctl's help instead.
    Help,
    /// Do nothing.
    Cancel,
}

/// How many existing manifests the menu lists one by one before it offers the create form.
const LISTED_MANIFESTS: usize = 4;

/// Asks whether to create an agent from one of `manifests`, or set up a new one, and with `help`
/// whether to show agentctl's help instead.
pub(crate) fn choose_from_manifests(manifests: &[PathBuf], help: bool) -> Result<Choice, Error> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let root = crate::tui::repository_root(&directory)
        .unwrap_or(&directory)
        .to_path_buf();
    let mut options = Vec::new();
    let mut choices = Vec::new();
    if manifests.len() <= LISTED_MANIFESTS {
        for manifest in manifests {
            options.push(format!("Create the agent in {}", manifest_label(&root, manifest)));
            choices.push(Choice::Start(Start::From(manifest.clone())));
        }
    } else {
        options.push(format!("Choose one of the {} agents set up here", manifests.len()));
        choices.push(Choice::Start(Start::Form));
    }
    options.push("Set up a new agent".to_owned());
    choices.push(Choice::Start(Start::New));
    if help {
        options.push("Show agentctl's help".to_owned());
        choices.push(Choice::Help);
    }
    let question = if manifests.len() == 1 {
        "An agent is set up here, but none runs. What would you like to do?"
    } else {
        "Agents are set up here, but none runs. What would you like to do?"
    };
    Ok(prompt::choose(question, &options, Some(0))?
        .and_then(|index| choices.get(index).cloned())
        .unwrap_or(Choice::Cancel))
}

/// A manifest as the menu names it: its directory below `root`, and the variant when it is one.
fn manifest_label(root: &Path, manifest: &Path) -> String {
    let directory = manifest.parent().unwrap_or(manifest);
    let relative = directory.strip_prefix(root).unwrap_or(directory);
    let place = if relative.as_os_str().is_empty() {
        "this directory".to_owned()
    } else {
        relative.display().to_string()
    };
    match manifest.file_name().and_then(|name| name.to_str()) {
        Some(agent::manifest::MANIFEST_FILE) | None => place,
        Some(file) => format!("{place} ({file})"),
    }
}

/// Sets up an Agent for the current directory, returning its name once it is Ready, or `None`
/// when the person declined a step. Where an agent exists already it sets up another one in a new
/// directory; where manifests exist but no agent runs, it offers to create one from them.
///
/// # Errors
///
/// Returns an error when there is no terminal to ask on, a login or apply fails, or the Agent
/// does not become Ready.
pub(crate) async fn run(home: &ControlPlaneHome, client: &Client, start: Start) -> CommandResult<Option<String>> {
    if !prompt::interactive() {
        return Err(Error::Invalid("agentctl init needs an interactive terminal".into()).into());
    }
    let directory = std::env::current_dir().map_err(Error::from)?;
    // An agent already here, or several (which resolve as ambiguous), means setting up another.
    let existing = match client.resolve_agent_variant(directory.clone(), None).await {
        Ok(agent) => Some(Some(agent.metadata.name)),
        Err(Error::Rpc(error)) if error.is_not_found() => None,
        Err(_) => Some(None),
    };
    let mut start = start;
    if let Some(agent) = &existing
        && matches!(start, Start::Ask)
    {
        let here = agent.as_ref().map_or_else(
            || "This directory already has agents.".to_owned(),
            |name| format!("This directory already has the agent {name}."),
        );
        if !prompt::confirm_lines(&[&here, "Set up another one?"])? {
            return Ok(None);
        }
        start = Start::New;
    }
    if existing.is_none() && matches!(start, Start::Ask | Start::Confirmed) {
        let manifests = crate::tui::working_tree_manifests(&directory);
        if !manifests.is_empty() {
            start = match choose_from_manifests(&manifests, false)? {
                Choice::Start(chosen) => chosen,
                Choice::Help | Choice::Cancel => return Ok(None),
            };
        }
    }
    if matches!(start, Start::Ask | Start::Confirmed | Start::New) {
        if matches!(start, Start::Ask) && !prompt::confirm("No agent here. Set one up?")? {
            return Ok(None);
        }
        if !docker_ready().await {
            return Ok(None);
        }
    }
    let mut status = client.auth_status().await?;
    if !status.any_harness() {
        if !sign_in_to_a_harness(home, client).await? {
            return Ok(None);
        }
        status = client.auth_status().await?;
    }
    let name = match start {
        Start::From(manifest) => create_written(home, client, &manifest).await?,
        Start::Form => {
            eprintln!();
            eprintln!("Choose one of the agents set up here, and a name, in the form that opens next.");
            prompt::wait_for_enter("Press Enter to open it…")?;
            let Some(name) = crate::tui::create_agent_here(home, client).await? else {
                return Ok(None);
            };
            name
        }
        Start::Ask | Start::Confirmed | Start::New => {
            let github = github_account(client, &status).await;
            let Some(manifest) = write_agent_directory(home, &directory, &github, &status)? else {
                return Ok(None);
            };
            create_written(home, client, &manifest).await?
        }
    };
    crate::wait_until_converged_simply(client, &name, std::time::Duration::from_mins(10)).await?;
    eprintln!("The agent {name} is ready.");
    Ok(Some(name))
}

/// Asks what the new agent needs and writes its directory, `agents/<name>/` at the repository
/// root, returning the manifest to create the agent from, or `None` when the person declined.
fn write_agent_directory(
    home: &ControlPlaneHome,
    directory: &Path,
    github: &Github,
    status: &AuthenticationStatus,
) -> Result<Option<PathBuf>, Error> {
    let root = crate::tui::repository_root(directory)
        .unwrap_or(directory)
        .to_path_buf();
    let repository = agent_name(&root);
    let (short_name, folder) = choose_short_name(&root, &repository)?;
    let name = qualified_name(&repository, &short_name);
    let tier = choose_tier()?;
    let toolchains = toolchains::detect(&root);
    if !toolchains.is_empty() {
        let found: Vec<String> = toolchains.iter().map(toolchains::Toolchain::describe).collect();
        eprintln!("\nThe agent also gets {}.", found.join(", "));
    }
    let (checkout, worktree) = choose_checkout(&root)?;
    let size = choose_size(home.path(), tier.most())?;
    let workspace = match &checkout {
        Checkout::Clone(remote) => repository_directory(remote).unwrap_or(repository),
        Checkout::Mount => repository,
    };
    let choices = Choices {
        name,
        workspace,
        github: github.clone(),
        tier,
        checkout,
        size,
        skills: repository_skills(&root),
    };

    std::fs::create_dir_all(folder.join("home"))?;
    let manifest = folder.join(agent::manifest::MANIFEST_FILE);
    let mut written = vec![(manifest.clone(), template::manifest(&choices, status))];
    if matches!(choices.checkout, Checkout::Clone(_)) {
        written.push((
            folder.join(template::WORKTREE_VARIANT),
            template::worktree_variant(&choices),
        ));
    }
    let tools = crate::tools::Tools::from_detected(
        &toolchains,
        tier.has_containers(),
        tier.has_browser(),
        tier.has_desktop(),
    );
    written.push((folder.join(crate::tools::TOOLS_FILE), tools.render()));
    written.push((folder.join(crate::tools::DOCKERFILE), template::dockerfile(&tools)));
    let cloned = match &choices.checkout {
        Checkout::Clone(repository) => vec![agent::repositories::Repository {
            repo: repository.clone(),
            path: None,
            about: None,
        }],
        Checkout::Mount => Vec::new(),
    };
    let repositories = folder.join(agent::repositories::REPOSITORIES_FILE);
    written.push((repositories.clone(), agent::repositories::render(&cloned)));
    written.push((folder.join(".dockerignore"), template::DOCKERIGNORE.to_owned()));
    let instructions = folder.join("instructions.md");
    written.extend([
        (instructions.clone(), template::instructions(&choices)),
        (folder.join(".env.sample"), template::ENV_SAMPLE.to_owned()),
        (folder.join(".gitignore"), template::GITIGNORE.to_owned()),
        (folder.join("home").join(".gitkeep"), String::new()),
    ]);
    for (path, contents) in &written {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_new(path, contents)?;
    }
    eprintln!("\nWrote the agent's files to {}.", folder.display());
    let chosen = if worktree {
        folder.join(template::WORKTREE_VARIANT)
    } else {
        manifest.clone()
    };
    let tools_file = folder.join(crate::tools::TOOLS_FILE);
    let editable = [
        manifest.as_path(),
        tools_file.as_path(),
        repositories.as_path(),
        instructions.as_path(),
    ];
    if prompt::confirm_lines(&[
        "Changing the image or mounts later means creating the agent again, which starts its disk over.",
        "Edit agent.yaml, tools.yaml, repositories.yaml and instructions.md before creating the agent?",
    ])? && !edit_manifest(&chosen, &editable)?
    {
        return Ok(None);
    }
    Ok(Some(chosen))
}

/// Asks what the agent should be called, which names its directory under `agents/`, until the
/// answer names a directory that does not exist yet. A name typed with the repository in front,
/// as the agent's full name has it, counts without it.
fn choose_short_name(root: &Path, repository: &str) -> Result<(String, PathBuf), Error> {
    let agents = root.join(template::AGENTS_DIRECTORY);
    let suggestion = ["dev", "agent"]
        .into_iter()
        .find(|name| !agents.join(name).exists())
        .map_or_else(|| "dev-2".to_owned(), str::to_owned);
    loop {
        let answer = prompt::ask(&format!("What should the agent be called? [{suggestion}]"))?;
        let answer = if answer.is_empty() {
            suggestion.clone()
        } else {
            answer.to_lowercase()
        };
        let short_name = answer
            .strip_prefix(&format!("{repository}-"))
            .filter(|rest| !rest.is_empty())
            .unwrap_or(&answer)
            .to_owned();
        if !is_short_name(&short_name) {
            eprintln!("Use lowercase letters, digits and hyphens, starting with a letter or digit.");
            continue;
        }
        let folder = agents.join(&short_name);
        if folder.exists() {
            eprintln!("This repository already has an agent called {short_name}; choose another name.");
            continue;
        }
        return Ok((short_name, folder));
    }
}

fn is_short_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// The agent's full name: the repository and the short name, as in `altinn-minimal`, or the short
/// name alone when together they are too long for a name.
fn qualified_name(repository: &str, short_name: &str) -> String {
    let qualified = format!("{repository}-{short_name}");
    if ::sandbox::SandboxName::new(qualified.clone()).is_ok() {
        qualified
    } else {
        short_name.to_owned()
    }
}

/// The directory a clone of `repository` lands in: its last path segment without `.git`.
fn repository_directory(repository: &str) -> Option<String> {
    let name = repository.trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (!name.is_empty()).then(|| name.to_owned())
}

/// The repository's own skills: each directory under `.claude/skills` with a `SKILL.md`, relative
/// to `root`, in name order.
fn repository_skills(root: &Path) -> Vec<String> {
    let mut skills: Vec<String> = std::fs::read_dir(root.join(".claude").join("skills"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("SKILL.md").is_file())
        .filter_map(|entry| entry.file_name().to_str().map(|name| format!(".claude/skills/{name}")))
        .collect();
    skills.sort();
    skills
}

/// Opens `files` in `$VISUAL` or `$EDITOR`, or asks for them to be edited elsewhere, until
/// `manifest` is valid. Returns `false` when the person gives up on an invalid one.
fn edit_manifest(manifest: &Path, files: &[&Path]) -> Result<bool, Error> {
    loop {
        prompt::edit_files(files)?;
        match agent::manifest::resolve(manifest) {
            Ok(_) => return Ok(true),
            Err(error) => {
                let reason = match error {
                    Error::Invalid(reason) => reason,
                    error => error.to_string(),
                };
                if !prompt::confirm_lines(&[&format!("The manifest is not valid: {reason}"), "Edit it again?"])? {
                    eprintln!("Fix {} and run `agentctl init` again.", manifest.display());
                    return Ok(false);
                }
            }
        }
    }
}

/// Whether Docker, which builds the agent's image, runs here; when it is installed but stopped,
/// waits for it to start.
async fn docker_ready() -> bool {
    match crate::host::docker() {
        Docker::Running => return true,
        Docker::Missing => {
            eprintln!("\nThe agent's image is built with Docker, which is not installed here.");
            eprintln!("Install Docker Desktop from {DOCKER_DESKTOP}, then run agentctl again.");
            return false;
        }
        Docker::Stopped => {}
    }
    eprintln!("\nThe agent's image is built with Docker, which is installed but not running.");
    eprintln!("Start Docker, and setup continues once it runs. Press Ctrl-C to stop.");
    while crate::host::docker() != Docker::Running {
        tokio::time::sleep(DOCKER_POLL).await;
    }
    eprintln!("Docker is running.");
    true
}

/// Asks what the agent should be able to do, which picks its image's stages.
fn choose_tier() -> Result<Tier, Error> {
    let recommended = Tier::Developer;
    let options: Vec<String> = Tier::ALL
        .iter()
        .map(|tier| {
            if *tier == recommended {
                format!("{} (recommended)", tier.describe())
            } else {
                tier.describe().to_owned()
            }
        })
        .collect();
    let default = Tier::ALL.iter().position(|tier| *tier == recommended);
    Ok(
        prompt::choose("What should the agent be able to do?", &options, default)?
            .and_then(|index| Tier::ALL.get(index).copied())
            .unwrap_or(recommended),
    )
}

/// Asks whether the agent clones the repository or works on this checkout, returning
/// `agent.yaml`'s checkout and whether to create the agent from the worktree variant instead.
/// Only a repository on GitHub can be cloned.
fn choose_checkout(root: &Path) -> Result<(Checkout, bool), Error> {
    let folder = crate::format::home_relative(root);
    if !crate::host::git_available() {
        eprintln!(
            "\nThe agent gets {folder} mounted. To offer a fresh clone instead, agentctl needs Git, which is not installed here. {}.",
            crate::host::git_install_hint()
        );
        return Ok((Checkout::Mount, false));
    }
    let Some(remote) = remote(root) else {
        eprintln!("\nThe agent gets {folder} mounted, since there's no GitHub repository to clone.");
        return Ok((Checkout::Mount, false));
    };
    let options = [
        format!("A fresh clone of {remote}, inside the agent (recommended)"),
        format!("{folder}, mounted into the agent: its edits change your files directly"),
    ];
    let worktree = prompt::choose("Which files should the agent work on?", &options, Some(0))? == Some(1);
    Ok((Checkout::Clone(remote), worktree))
}

/// The repository to clone, as `owner/name`: the base images clone only from GitHub.
fn remote(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "-v"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    crate::github::repository_from_remotes(&String::from_utf8_lossy(&output.stdout))
        .map(|repository| repository.to_string())
}

/// Asks for the agent's size, recommending one within `most` that fits this host and the free
/// space beside agentd's state in `state`.
fn choose_size(state: &Path, most: sizing::Size) -> Result<sizing::Size, Error> {
    let Some(host) = sizing::Host::measure(state) else {
        let size = sizing::Host {
            cpus: 4,
            memory_gib: 16,
            free_disk_gib: 32,
        }
        .recommend(most);
        eprintln!("Could not measure this machine, so the agent gets {}.", size.describe());
        return Ok(size);
    };
    let recommended = host.recommend(most);
    let mut lines = vec![
        "How large should the agent be?".to_owned(),
        format!(
            "This machine has {} CPUs, {} GiB memory and {} GiB free disk. Edit agent.yaml next for other sizes.",
            host.cpus, host.memory_gib, host.free_disk_gib
        ),
    ];
    if host.free_disk_gib < sizing::LOW_DISK_GIB {
        lines.push(
            "⚠ Disk space is low here. An agent's disk uses space on this machine as it fills up, up to the size \
             you pick below."
                .to_owned(),
        );
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let options = [
        format!("{} (recommended)", recommended.describe()),
        format!("{}, for running several agents", recommended.smaller().describe()),
    ];
    Ok(match prompt::choose_lines(&lines, &options, Some(0))? {
        Some(1) => recommended.smaller(),
        _ => recommended,
    })
}

/// Creates `path` with `contents`, refusing to replace a file that appeared meanwhile.
fn write_new(path: &Path, contents: &str) -> Result<(), Error> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// Makes sure Agents of this repository have a working GitHub token: present, not expired, and
/// able to see the repository. Offers the login when not; GitHub is optional, so a failure
/// leaves the Agent without it rather than stopping init.
async fn github_account(client: &Client, status: &AuthenticationStatus) -> Github {
    let Some(repository) = crate::github::current_repository() else {
        return match status.github.as_slice() {
            [_] => Github::Only,
            [] => offer_github_login(client, None).await,
            _ => Github::None,
        };
    };
    let owner = repository.owner.clone();
    match client.github_check(Some(owner.clone()), Some(repository.clone())).await {
        Ok(check) => {
            let now = time::OffsetDateTime::now_utc();
            let visible = check
                .inspection
                .repository
                .as_ref()
                .is_some_and(|(_, visible)| *visible);
            let expiring = check.token.expires_soon(now)
                || check
                    .inspection
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= now + agent::github::EXPIRY_WARNING);
            if visible && !expiring {
                eprintln!(
                    "GitHub: agents act as {} in {owner}, and can see {repository}.",
                    check.inspection.account.login
                );
                return Github::Owner(owner);
            }
            let reason = format!(
                "The GitHub token for {} {}.",
                check.token.scope(),
                if visible {
                    "expires within a week"
                } else {
                    "cannot see this repository"
                }
            );
            match offer_github_login(client, Some(&reason)).await {
                Github::None if visible => Github::Owner(owner),
                chosen => chosen,
            }
        }
        // No token for this owner yet, which the offer below explains by itself.
        Err(_) => offer_github_login(client, None).await,
    }
}

/// Offers setting up a GitHub token, saying first why when there is a reason beyond having none.
async fn offer_github_login(client: &Client, reason: Option<&str>) -> Github {
    let asked = reason.map_or_else(
        || prompt::confirm("Should agents be able to work on GitHub, such as pushing code and opening pull requests?"),
        |reason| prompt::confirm_lines(&[reason, "Set up a new GitHub token for agents now?"]),
    );
    if !asked.unwrap_or(false) {
        return Github::None;
    }
    match crate::github::login(client, crate::github::Source::Form, None).await {
        Ok(Some(token)) => token.owner.map_or(Github::Only, Github::Owner),
        Ok(None) => Github::None,
        Err(error) => {
            eprintln!("agentctl: {error}; continuing without GitHub");
            Github::None
        }
    }
}

/// Creates the agent from `manifest` under its own name, or the first free
/// numbered one when an agent already has it.
async fn create_written(home: &ControlPlaneHome, client: &Client, manifest: &Path) -> CommandResult<String> {
    let manifest_name = agent::manifest::resolve(manifest)?.agent.metadata.name;
    let agents = client.list_agents().await?;
    let name = if agents.iter().any(|agent| agent.metadata.name == manifest_name) {
        let numbered = crate::tui::numbered_name(&manifest_name, &agents);
        eprintln!("An agent named {manifest_name} already exists, so this one is {numbered}.");
        numbered
    } else {
        manifest_name
    };
    crate::tui::create_agent(home, client, manifest.to_path_buf(), name.clone(), None).await?;
    Ok(name)
}

/// Offers the Claude Code login, then the Codex one, returning whether either was stored.
async fn sign_in_to_a_harness(home: &ControlPlaneHome, client: &Client) -> CommandResult<bool> {
    let options = [
        "Claude Code".to_owned(),
        "Codex".to_owned(),
        "Neither: stop setting up for now".to_owned(),
    ];
    let harness = match prompt::choose(
        "Neither Claude Code nor Codex is signed in on this host. Which should agents use?",
        &options,
        Some(0),
    )? {
        Some(0) => Some(agent::Harness::ClaudeCode),
        Some(1) => Some(agent::Harness::Codex),
        _ => None,
    };
    if let Some(harness) = harness {
        crate::harness_login(home, client, harness, false).await?;
        return Ok(true);
    }
    eprintln!(
        "Agents need one of them to work. Sign in with `agentctl claude login` or `agentctl codex login`, then run `agentctl init` again."
    );
    Ok(false)
}

/// The Agent name a new manifest suggests: the directory's name in Sandbox name characters.
fn agent_name(root: &Path) -> String {
    let name: String = root
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if ::sandbox::SandboxName::accepts(character) {
                character
            } else {
                '-'
            }
        })
        .take(::sandbox::MAX_SANDBOX_NAME_BYTES)
        .collect();
    let name = name.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    if ::sandbox::SandboxName::new(name.to_owned()).is_ok() {
        name.to_owned()
    } else {
        "agent".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_suggested_name_is_the_directory_in_sandbox_name_characters() {
        assert_eq!(agent_name(Path::new("/src/digdir-agents")), "digdir-agents");
        assert_eq!(agent_name(Path::new("/src/My Repo.rs")), "my-repo-rs");
        assert_eq!(agent_name(Path::new("/src/_hidden_")), "hidden");
        assert_eq!(agent_name(Path::new("/")), "agent");
        assert_eq!(agent_name(Path::new("/src/---")), "agent");
    }

    #[test]
    fn the_menu_names_a_manifest_by_its_directory_and_variant() {
        let root = Path::new("/src/app");
        assert_eq!(manifest_label(root, &root.join("agents/dev/agent.yaml")), "agents/dev");
        assert_eq!(
            manifest_label(root, &root.join("agents/dev/agent.worktree.yaml")),
            "agents/dev (agent.worktree.yaml)"
        );
        assert_eq!(manifest_label(root, &root.join("agent.yaml")), "this directory");
    }

    #[test]
    fn a_short_name_names_a_directory_and_qualifies_the_agent_name() {
        for name in ["dev", "minimal", "full-2", "2nd"] {
            assert!(is_short_name(name), "{name}");
        }
        for name in ["", "Dev", "-dev", "my agent", "a/b", "..", "dev_x"] {
            assert!(!is_short_name(name), "{name:?}");
        }
        assert_eq!(qualified_name("altinn", "minimal"), "altinn-minimal");
        let long = "r".repeat(::sandbox::MAX_SANDBOX_NAME_BYTES);
        assert_eq!(qualified_name(&long, "dev"), "dev", "too long together");
    }

    #[test]
    fn a_clone_lands_in_its_repository_name() {
        assert_eq!(
            repository_directory("digdir/digdir-agents").as_deref(),
            Some("digdir-agents")
        );
        assert_eq!(
            repository_directory("https://dev.azure.com/org/project/_git/app.git").as_deref(),
            Some("app")
        );
        assert_eq!(
            repository_directory("git@example.com:team/tool.git").as_deref(),
            Some("tool")
        );
        assert_eq!(repository_directory("host:repo").as_deref(), Some("repo"));
        assert_eq!(repository_directory("").as_deref(), None);
    }

    #[test]
    fn the_repositorys_skills_are_the_directories_with_a_skill_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        assert!(repository_skills(directory.path()).is_empty());
        let skills = directory.path().join(".claude").join("skills");
        for (skill, has_file) in [("review", true), ("changelog", true), ("notes", false)] {
            std::fs::create_dir_all(skills.join(skill)).expect("skill directory");
            if has_file {
                std::fs::write(skills.join(skill).join("SKILL.md"), "---\nname: x\n---\n").expect("skill");
            }
        }
        assert_eq!(
            repository_skills(directory.path()),
            [".claude/skills/changelog", ".claude/skills/review"]
        );
    }

    #[test]
    fn a_new_manifest_never_replaces_an_existing_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("agent.yaml");
        std::fs::write(&path, "mine").expect("existing manifest");
        write_new(&path, "template").expect_err("an existing file is kept");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "mine");
    }
}
