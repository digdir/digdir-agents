//! `agentctl setup`: one guided way from nothing to a running agent connected to an editor.
//!
//! It starts from a standard agent in the embedded catalog, or from an agent a team keeps in its
//! repository, asks what can differ (name, size, harnesses, folders to share, GitHub
//! access, other declared values), writes a self-contained `~/agents/<name>/` and creates the
//! agent from it. Every question has a flag, so scripts get the same result without a terminal.

use create::{Review, bundle_into, finish, manifest_text, review};
use github::{choose_github, give_identity};
use mounts::{check_mounts, choose_shared, mount_unless_mounted, protected_folders, refuse_staged_mounts};
use shape::{Names, Shape};
use sign_in::{choose_logins, remove_dead_sign_in_agents, sign_in_where_needed};
use source::{choose_draft, choose_image, team_draft};

mod answers;
mod bundle;
mod create;
mod docker;
mod edit;
mod env_file;
mod existing;
mod fetch;
mod github;
mod ide;
mod image;
mod mounts;
mod repeat;
mod shape;
pub(crate) mod sign_in;
mod sizing;
mod source;
mod staging;
mod update;
mod values;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use agent::{AccessSpec, Agent, Error, Harness, catalog::Catalog, control_api::Client, local::home::ControlPlaneHome};
use clap::{Args, ValueEnum};
use zeroize::Zeroizing;

use crate::{CommandError, CommandResult, OutputFormat};
use answers::{Answers, Choice};
use sizing::Whole;

/// The folder below the person's home that holds the agents setup creates, one folder each.
const AGENTS_FOLDER: &str = "agents";

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag answers one setup question; they are not a state machine"
)]
pub(crate) struct SetupArguments {
    /// Accept the default for every question no flag answers.
    #[arg(short = 'y', long)]
    yes: bool,
    /// Never ask: fail, naming the flag, when an answer is missing. Implied without a terminal.
    #[arg(long)]
    non_interactive: bool,
    /// List the standard agents and exit.
    #[arg(long)]
    list_archetypes: bool,
    /// Output format for `--list-archetypes`.
    #[arg(short = 'o', long, default_value = "table", value_enum)]
    output: OutputFormat,
    /// Where the agent comes from.
    #[arg(long, value_enum)]
    source: Option<SourceChoice>,
    /// An agent setup made before, in ~/agents, to carry on with. With `--action replace`,
    /// `--archetype` or `--from` says what replaces it.
    #[arg(long)]
    existing: Option<String>,
    /// What to do with the agent `--existing` names.
    #[arg(long, value_enum, requires = "existing")]
    action: Option<existing::ExistingAction>,
    /// Replace the agent definition in ~/agents/<name> when the name is one already there: its
    /// folder is removed first, and the agent created from it is deleted, disk and all.
    #[arg(long)]
    replace: bool,
    /// Standard agent to start from (see `--list-archetypes`).
    #[arg(long, conflicts_with = "from")]
    archetype: Option<String>,
    /// A team's agent: an agent manifest, a folder holding one, or a GitHub address such as
    /// `https://github.com/OWNER/REPO//agents/NAME@REF`, or the address GitHub shows for a folder
    /// or a manifest at any branch, tag or commit, fetched into a temporary checkout.
    #[arg(long)]
    from: Option<String>,
    /// Variant of a team's agent, such as `nested` for its `agent.nested.yaml`.
    #[arg(long, value_parser = crate::parse_variant_name)]
    variant: Option<agent::AgentVariantName>,
    /// Agent name; defaults to the standard agent's or the manifest's, numbered when taken.
    #[arg(long)]
    name: Option<String>,
    /// Size of the agent's sandbox, measured against this machine.
    #[arg(long, value_enum)]
    size: Option<SizeChoice>,
    /// Harnesses the agent has, of those the manifest declares; all of them unless named. Repeat
    /// the flag, or separate them with commas.
    #[arg(long, value_parser = crate::parse_harness, value_delimiter = ',')]
    harnesses: Vec<Harness>,
    /// Harness new sessions use by default; one the agent has.
    #[arg(long, value_parser = crate::parse_harness)]
    harness: Option<Harness>,
    /// Edit the whole manifest in $VISUAL or $EDITOR before the agent is created, for anything the
    /// questions leave at its recommended value, such as an exact size, the model or denied hosts.
    #[arg(long)]
    edit: bool,
    /// Folder on this machine to share with the agent, read-write, in its working directory.
    #[arg(long, conflicts_with = "no_mount")]
    mount: Option<PathBuf>,
    /// Share no folder besides those the agent's definition shares.
    #[arg(long)]
    no_mount: bool,
    /// The checkout of a team agent's repository to share where its manifest mounts that
    /// repository, such as Altinn's `worktree` variant does. Defaults to the checkout a local
    /// manifest is in.
    #[arg(long)]
    checkout: Option<PathBuf>,
    /// A folder the agent's definition shares that this agent should not, named by where the agent
    /// sees it, as setup lists it.
    #[arg(long, value_name = "PATH_IN_AGENT")]
    unshare: Vec<String>,
    /// How the agent reaches GitHub.
    #[arg(long, value_enum)]
    github: Option<GithubChoice>,
    /// The GitHub account to sign in as in the browser; one made for agents keeps yours out.
    #[arg(long)]
    github_user: Option<String>,
    /// Whether to sign in to harnesses in a short-lived sandbox, so this machine needs neither them
    /// nor `gh`; logins agentd holds already are used unless this is `again`.
    #[arg(long, value_enum)]
    sign_in: Option<SignInChoice>,
    /// Read the GitHub token from standard input instead of asking for it.
    #[arg(long)]
    github_token_stdin: bool,
    /// Name the agent's commits carry; defaults to the GitHub account's, when it signs in to one,
    /// else to Git's `user.name` on this machine.
    #[arg(long)]
    git_user_name: Option<String>,
    /// Address the agent's commits carry; defaults to the GitHub account's private address, when it
    /// signs in to one, else to Git's `user.email` on this machine.
    #[arg(long)]
    git_user_email: Option<String>,
    /// File with values for what the manifest declares, as `NAME=value` lines.
    #[arg(long)]
    env_file: Option<PathBuf>,
    /// Print the manifest that would be written, and change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Write the agent's folder but do not create the agent.
    #[arg(long)]
    no_apply: bool,
    /// Maximum wait for the agent to become ready, written as seconds, minutes, or hours.
    #[arg(long, default_value = "20m", value_parser = crate::parse_duration)]
    timeout: Duration,
    /// Editor to open the agent in once it is ready.
    #[arg(long, value_enum)]
    ide: Option<ide::IdeChoice>,
    /// Do not add the agents' SSH configuration to ~/.ssh/config.
    #[arg(long)]
    no_ssh_config: bool,
    /// Build a standard agent's image on this machine from its Dockerfile, instead of pulling the
    /// published one. Setup offers it on its own when the published one cannot be pulled.
    #[arg(long)]
    build_image: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceChoice {
    /// An agent setup made before, in ~/agents.
    Existing,
    /// A standard agent from the catalog.
    Standard,
    /// An agent definition that exists, such as one a team keeps in its repository.
    Team,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SizeChoice {
    S,
    M,
    L,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum GithubChoice {
    /// Signing in through the browser, as an account you choose.
    Browser,
    /// A token pasted here, checked with GitHub.
    Token,
    /// The account another agent in ~/agents uses, the one `--github-user` names when given.
    Existing,
    /// No GitHub access.
    Skip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SignInChoice {
    /// Sign in in a short-lived sandbox to harnesses with no login yet.
    Sandbox,
    /// Sign in in a short-lived sandbox to every harness, replacing logins agentd holds.
    Again,
    /// Do not sign in; the agent waits for `agentctl <harness> login`.
    Skip,
}

/// The agent being set up, before it is written.
struct Draft {
    agent: Agent,
    /// Where it comes from.
    source: Source,
    minimum: Whole,
    recommended: Whole,
    /// The definition in ~/agents this one replaces.
    replace: Option<existing::Existing>,
    /// Its values and logins.
    access: Access,
    /// The published image the agent's image is built here in place of.
    built_here: Option<String>,
    /// What its source defines that setup left out, so an update leaves it out again.
    left_out: update::LeftOut,
}

impl Draft {
    fn new(agent: Agent, source: Source, minimum: Whole, recommended: Whole) -> Self {
        Self {
            agent,
            source,
            minimum,
            recommended,
            replace: None,
            access: Access::default(),
            built_here: None,
            left_out: update::LeftOut::default(),
        }
    }
}

/// Where an agent comes from.
struct Source {
    /// Where the manifest's relative paths resolve.
    directory: PathBuf,
    /// What the agent came from, named in the written manifest.
    origin: String,
    /// Where to fetch it again to update the agent from it.
    recorded: Option<update::Recorded>,
    /// The repository the manifest is in, whose checkout its relative bind mounts mount.
    repository: Option<mounts::Repository>,
    /// Whether it is a standard agent, which a mounted folder's own agents may replace.
    from_catalog: bool,
    /// For a standard agent, the folder its published image is built from, so it can be built
    /// here instead.
    image_sources: Option<PathBuf>,
}

/// An agent's values and logins, gathered before it is created.
#[derive(Default)]
struct Access {
    /// Values for the agent's `.env`.
    values: Vec<(String, Zeroizing<String>)>,
    /// Values already known, such as those of the definition this one replaces, used before
    /// anything is asked.
    known_values: BTreeMap<String, Zeroizing<String>>,
    /// Signing in to GitHub in the browser, as this account when one is named, once the sign-in
    /// agent runs.
    github_browser: Option<BrowserSignIn>,
    /// The GitHub account the agent's token acts as, once it is checked.
    github_account: Option<String>,
    /// The harnesses to sign in to once the sign-in agent runs.
    sign_in_to: Vec<Harness>,
    /// The harnesses that use the login agentd holds already.
    reused_logins: Vec<Harness>,
}

/// A GitHub sign-in through the browser, waiting for the sign-in agent.
struct BrowserSignIn {
    /// The account it must be, when one is named.
    account: Option<String>,
}

pub(crate) async fn run(arguments: SetupArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    let catalog = Catalog::load()?;
    if arguments.list_archetypes {
        return list_archetypes(&catalog, arguments.output);
    }
    let interactive = crate::prompt::interactive() && !arguments.non_interactive;
    // The recommended answers usually fit, so setup fills them in and shows them for review, until
    // the person asks to change something. A dry run shows the manifest instead of a review.
    let mut fill_in = interactive && !arguments.yes && !arguments.dry_run;
    let mut replay = None;
    loop {
        let mut answers = Answers::new(interactive, arguments.yes);
        if let Some(replay) = replay.take() {
            answers = answers.replaying(replay);
        }
        let result = Box::pin(run_once(&arguments, &answers, fill_in, &catalog, home, client)).await;
        if answers.cancelled() {
            return Err(CommandError::Message("setup was cancelled".into()));
        }
        if answers.starting_over() {
            fill_in = false;
        } else if !answers.went_back() {
            return result;
        }
        replay = Some(answers.rewind());
    }
}

/// One run of setup, which going back to the question before ends, for [`run`] to start again.
/// With `fill_in`, defaults answer the questions after the source, and the review shows them.
#[allow(
    clippy::too_many_lines,
    reason = "the wizard's steps in the order people meet them; each step is a function of its own"
)]
async fn run_once(
    arguments: &SetupArguments,
    answers: &Answers,
    fill_in: bool,
    catalog: &Catalog,
    home: &ControlPlaneHome,
    client: &Client,
) -> CommandResult<()> {
    let agents_root = agent::local::home::user_home_directory()
        .ok_or_else(|| Error::Invalid("the home directory is not set".into()))?
        .join(AGENTS_FOLDER);
    // Staged sources, such as an extracted standard agent, live under the control-plane home so
    // that they never land in a folder a person mounts.
    home.prepare()?;
    let leftovers = staging::sweep(home.path());
    let staging = staging::Staging::create(home.path())?;

    let (agents, found) = agents_and_definitions(arguments, home, client, &agents_root, &leftovers).await?;
    let context = existing::Context {
        arguments,
        answers,
        catalog,
        client,
        home,
        staging: &staging,
    };
    let Some((source, replace_target)) = Box::pin(choose_start(&context, &found)).await? else {
        return Ok(());
    };
    let mut draft = choose_draft(arguments, answers, catalog, staging.path(), source).await?;
    if fill_in {
        answers.assume_defaults(true);
    }
    let names = Names {
        agents: &agents,
        root: &agents_root,
        found: &found,
        target: replace_target.as_ref(),
        allow_replace: !arguments.dry_run,
    };
    let shape = Shape {
        arguments,
        answers,
        storage: home.path(),
        names: &names,
    };
    shape.apply(&mut draft)?;
    let protected = protected_folders(&agents_root, home);
    if let Some((redirect, folder)) = choose_shared(arguments, answers, &mut draft, &protected, true)? {
        draft = team_draft(&redirect, &redirect.display().to_string())?;
        shape.apply(&mut draft)?;
        // The person chose the folder to share already, and the agent in it shares it.
        choose_shared(arguments, answers, &mut draft, &protected, false)?;
        mount_unless_mounted(&mut draft, &folder)?;
    }
    refuse_staged_mounts(&draft, staging.path())?;
    choose_image(arguments, answers, &mut draft).await?;
    if arguments.edit {
        edit::manifest(answers, &mut draft)?;
    }
    if !draft.agent.spec.access.contains(&AccessSpec::Ssh {}) {
        draft.agent.spec.access.push(AccessSpec::Ssh {});
    }
    check_mounts(&draft, &protected)?;
    if arguments.dry_run {
        let mut preview = draft.agent.clone();
        bundle_into(&mut preview, &draft, staging.path().join("preview").as_path())?;
        print!("{}", manifest_text(&preview, &draft)?);
        return Ok(());
    }
    if let Some(replaced) = &draft.replace {
        draft.access.known_values = existing::reusable_values(answers, replaced).await?;
    }
    choose_github(arguments, answers, &mut draft, &found).await?;
    give_identity(arguments, &mut draft);
    values::collect(answers, arguments.env_file.as_deref(), &mut draft).await?;
    choose_logins(arguments, answers, client, &mut draft).await?;
    let folder = agents_root.join(&draft.agent.metadata.name);
    answers.assume_defaults(arguments.yes);
    loop {
        match review(answers, &draft, &folder, fill_in)? {
            Review::Create => break,
            Review::Change => {
                answers.start_over();
                return Ok(());
            }
            Review::Edit => {
                edit::manifest(answers, &mut draft)?;
                check_mounts(&draft, &protected)?;
            }
            Review::Stop => {
                println!("Nothing was created.");
                return Ok(());
            }
        }
    }
    answers.no_way_back();
    sign_in_where_needed(arguments, answers, client, home, &staging, &mut draft).await?;
    // What can fail before the agent exists is done before its folder is written, so a failure
    // leaves nothing behind.
    let builds = draft.built_here.is_some()
        || matches!(
            draft.agent.spec.sandbox.image,
            sandbox::image::ImageSource::Build { .. }
        );
    if builds && !arguments.no_apply {
        docker::ensure().await?;
    }
    finish(arguments, answers, client, home, &staging, &mut draft, &folder).await
}

/// agentd's agents, and the definitions in ~/agents with their state. A dry run changes nothing,
/// so it needs no daemon: it sees no agents, and every definition as not created.
async fn agents_and_definitions(
    arguments: &SetupArguments,
    home: &ControlPlaneHome,
    client: &Client,
    root: &Path,
    leftovers: &staging::Leftovers,
) -> CommandResult<(Vec<Agent>, Vec<existing::Existing>)> {
    if arguments.dry_run {
        return Ok((Vec::new(), existing::scan(None, root, &[]).await));
    }
    crate::ensure_daemon(home, client).await?;
    let agents = client.list_agents().await?;
    remove_dead_sign_in_agents(client, &agents, leftovers).await;
    let found = existing::scan(Some(client), root, &agents).await;
    Ok((agents, found))
}

/// Where setup starts from: the source of a new agent, and the existing definition it replaces;
/// `None` when an action on an existing agent was all there was to do.
async fn choose_start(
    context: &existing::Context<'_>,
    found: &[existing::Existing],
) -> CommandResult<Option<(SourceChoice, Option<existing::Existing>)>> {
    let source = choose_source(context.arguments, context.answers, !found.is_empty())?;
    if source != SourceChoice::Existing {
        return Ok(Some((source, None)));
    }
    if context.arguments.dry_run {
        return Err(CommandError::Message(
            "a dry run previews a new agent; it cannot carry on with an existing one".into(),
        ));
    }
    let chosen = existing::pick(context.arguments, context.answers, found)?;
    match Box::pin(existing::proceed(context, chosen)).await? {
        existing::Proceed::Done => Ok(None),
        existing::Proceed::Replace(replaced) => {
            println!("Set up the agent that replaces {}:", replaced.name);
            let source = choose_source(context.arguments, context.answers, false)?;
            Ok(Some((source, Some(replaced))))
        }
    }
}

/// Where the agent comes from: a flag, else the person's choice. The agents setup made before are
/// offered first when there are any and `offer_existing` allows them.
fn choose_source(arguments: &SetupArguments, answers: &Answers, offer_existing: bool) -> CommandResult<SourceChoice> {
    let wants_existing = arguments.source == Some(SourceChoice::Existing)
        || (arguments.source.is_none() && arguments.existing.is_some());
    if offer_existing && wants_existing {
        return Ok(SourceChoice::Existing);
    }
    let flagged = match (
        arguments.source.filter(|source| *source != SourceChoice::Existing),
        &arguments.archetype,
        &arguments.from,
    ) {
        (Some(source), ..) => Some(source),
        (None, Some(_), _) => Some(SourceChoice::Standard),
        (None, None, Some(_)) => Some(SourceChoice::Team),
        (None, None, None) => None,
    };
    if let Some(source) = flagged {
        return Ok(source);
    }
    let mut choices = Vec::new();
    if offer_existing {
        choices.push(Choice::new(
            SourceChoice::Existing,
            "Carry on with an agent you set up before, in ~/agents",
        ));
    }
    choices.extend([
        Choice::new(SourceChoice::Standard, "With a standard agent").suggested(),
        Choice::new(
            SourceChoice::Team,
            "With an agent definition that exists, such as your team's, in a folder or on GitHub",
        ),
    ]);
    let choices = choices
        .into_iter()
        .map(|choice| {
            let source = value_name(&choice.value);
            choice.flag("--source", &[&source])
        })
        .collect();
    Ok(answers.choose("--source", "How do you want to start?", choices, 0)?)
}

/// How to run agentctl on the control-plane home this setup used: with `--home` when a plain
/// `agentctl` would reach another one, so the commands setup suggests work as printed.
fn command_line(home: &ControlPlaneHome) -> String {
    let program = program();
    let plain = ControlPlaneHome::resolve(None).is_ok_and(|plain| plain == *home);
    if plain {
        program
    } else {
        format!("{program} --home {}", shell_word(&home.path().to_string_lossy()))
    }
}

/// `word` as one word of a command printed for this machine's shell.
fn shell_word(word: &str) -> String {
    agent::ssh::shell_word(word, agent::ssh::CommandShell::host())
}

/// How to run this agentctl: by name when that finds it, otherwise by its path. Another agentctl
/// first on `PATH`, such as an installed one while this is a development build, cannot talk to
/// the agentd this one starts.
fn program() -> String {
    let Ok(running) = std::env::current_exe().and_then(std::fs::canonicalize) else {
        return "agentctl".to_owned();
    };
    let name = format!("agentctl{}", std::env::consts::EXE_SUFFIX);
    let found = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&name))
        .find(|candidate| candidate.is_file());
    if found.is_some_and(|found| std::fs::canonicalize(found).is_ok_and(|found| found == running)) {
        return "agentctl".to_owned();
    }
    shell_word(&running.to_string_lossy())
}

/// How `value` is spelled on the command line.
fn value_name(value: &impl ValueEnum) -> String {
    value
        .to_possible_value()
        .map_or_else(String::new, |value| value.get_name().to_owned())
}

fn list_archetypes(catalog: &Catalog, output: OutputFormat) -> CommandResult<()> {
    let visible: Vec<_> = catalog.visible().collect();
    match output {
        OutputFormat::Json => crate::print_json(&visible)?,
        OutputFormat::Table => {
            if visible.is_empty() {
                println!("This agentctl has no standard agents.");
            }
            for archetype in visible {
                println!("{:<28} {}", archetype.id, archetype.title);
                println!("{:<28} {}", "", archetype.description);
            }
        }
    }
    Ok(())
}
