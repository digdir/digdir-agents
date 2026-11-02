//! `agentctl setup`: one guided way to start using an agent and make it yours.
//!
//! A team's agent stays in the team's checkout. Setup writes the person's choices as a variant of
//! its manifest of their own, `agent.mine.yaml` beside it, which Git ignores, and applies it, as
//! `agentctl apply --variant mine` does. The values the definition declares go in a file in the
//! control-plane home, outside any checkout. Every question has a flag, so scripts get the same
//! result without a terminal.

use crate::sign_in::remove_dead_sign_in_agents;
use create::{Review, finish, review};
use github::{choose_github, give_identity};
use mounts::{Protected, check_mounts, check_sources, choose_shared, protected_folders};
use shape::{Names, Shape};
use sign_in::{choose_logins, sign_in_where_needed};
use source::choose_draft;

mod answers;
mod create;
mod docker;
mod edit;
mod env_file;
mod fetch;
mod github;
mod mounts;
mod repeat;
mod shape;
mod sign_in;
mod sizing;
mod source;
mod values;

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use agent::{
    AccessSpec, Agent, AgentVariantName, Harness, control_api::Client, harness::LoginName,
    local::home::ControlPlaneHome,
};
use clap::{Args, ValueEnum};
use zeroize::Zeroizing;

use crate::{CommandError, CommandResult, command_line};
use answers::{Answers, Choice};
use sizing::Whole;

/// The variant setup writes the person's choices to, unless `--save-as` names another.
const PERSONAL_VARIANT: &str = "mine";

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
    /// Where the agent comes from.
    #[arg(long, value_enum)]
    source: Option<SourceChoice>,
    /// Create the agent again when a change needs it, as a change of its image does, deleting its
    /// disk and sessions.
    #[arg(long)]
    replace: bool,
    /// The agent to start from: an agent manifest, a folder holding one, or a GitHub address such
    /// as `https://github.com/OWNER/REPO//agents/NAME@REF`, or the address GitHub shows for a
    /// folder or a manifest at any branch, tag or commit, which setup clones.
    #[arg(long)]
    from: Option<String>,
    /// Where to clone the repository a GitHub `--from` names; a checkout of it there is used as it
    /// is. Defaults to a folder named after the repository here.
    #[arg(long)]
    clone_into: Option<PathBuf>,
    /// Variant of a team's agent to start from, such as `nested` for its `agent.nested.yaml`.
    #[arg(long, value_parser = crate::parse_variant_name)]
    variant: Option<AgentVariantName>,
    /// The variant your choices are written to, beside the manifest they change, such as `mine`
    /// for `agent.mine.yaml`. Git should ignore it.
    #[arg(long, value_parser = crate::parse_variant_name)]
    save_as: Option<AgentVariantName>,
    /// Agent name; defaults to the manifest's followed by the variant's, numbered when taken.
    #[arg(long)]
    name: Option<String>,
    /// Size of the agent's sandbox, measured against this machine.
    #[arg(long, value_enum)]
    size: Option<SizeChoice>,
    /// CPUs of the agent's sandbox, for a size of your own instead of --size. The parts of it
    /// not given are asked for.
    #[arg(long, conflicts_with = "size")]
    cpus: Option<u64>,
    /// Memory of the agent's sandbox in GiB, for a size of your own instead of --size.
    #[arg(long, conflicts_with = "size")]
    memory_gib: Option<u64>,
    /// Disk of the agent's sandbox in GiB, for a size of your own instead of --size.
    #[arg(long, conflicts_with = "size")]
    disk_gib: Option<u64>,
    /// Harnesses the agent has, of those the manifest declares; all of them unless named. Repeat
    /// the flag, or separate them with commas.
    #[arg(long, value_parser = crate::parse_harness, value_delimiter = ',')]
    harnesses: Vec<Harness>,
    /// Harness new sessions use by default; one the agent has.
    #[arg(long, value_parser = crate::parse_harness)]
    harness: Option<Harness>,
    /// The stored login a harness uses, as `HARNESS=LOGIN`, such as `claude-code=work`.
    #[arg(long, value_parser = parse_login, value_name = "HARNESS=LOGIN")]
    login: Vec<(Harness, LoginName)>,
    /// Edit your variant in $VISUAL or $EDITOR before the agent is created, for anything the
    /// questions leave at its recommended value, such as an exact size, the model or denied hosts.
    #[arg(long)]
    edit: bool,
    /// Fill in the values the agent's manifest declares in $VISUAL or $EDITOR, instead of one
    /// question at a time.
    #[arg(long)]
    edit_values: bool,
    /// Folder on this machine to share with the agent, read-write, in its working directory.
    #[arg(long)]
    mount: Option<PathBuf>,
    /// A folder the agent's definition shares that this agent should not, named by where the agent
    /// sees it, as setup lists it.
    #[arg(long, value_name = "PATH_IN_AGENT")]
    unshare: Vec<String>,
    /// How the agent reaches GitHub, when its definition declares `GITHUB_TOKEN`.
    #[arg(long, value_enum)]
    github: Option<GithubChoice>,
    /// The GitHub account the agent's token must act as; one made for agents keeps yours out.
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
    /// Print the variant that would be written, and change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Write your variant and values but do not apply them.
    #[arg(long)]
    no_apply: bool,
    /// Maximum wait for the agent to become ready, written as seconds, minutes, or hours.
    #[arg(long, default_value = "20m", value_parser = crate::parse_duration)]
    timeout: Duration,
}

/// Reads `HARNESS=LOGIN`, as `--login` takes it.
fn parse_login(value: &str) -> Result<(Harness, LoginName), String> {
    let (harness, login) = value
        .split_once('=')
        .ok_or_else(|| format!("{value:?} is not HARNESS=LOGIN"))?;
    let login = LoginName::new(login).map_err(|error| error.to_string())?;
    Ok((crate::parse_harness(harness)?, login))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceChoice {
    /// An agent in the repository of the folder setup runs in.
    Here,
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

/// The agent being set up, before the person's variant of it is written.
struct Draft {
    /// The agent as the person makes it.
    agent: Agent,
    /// The agent the manifest the variant extends resolves to.
    base: Agent,
    /// Where it comes from.
    source: Source,
    minimum: Whole,
    recommended: Whole,
    /// Its values and logins.
    access: Access,
    /// Whether the person's variant exists already, so setup changes it.
    changing: bool,
    /// The file the agent reads its values from, when one is known already.
    values_file: Option<PathBuf>,
}

impl Draft {
    /// Where the agent's values end up: the file it reads them from already, else one named after
    /// it in the control-plane home, outside any checkout.
    fn values_path(&self, home: &ControlPlaneHome) -> PathBuf {
        self.values_file
            .clone()
            .unwrap_or_else(|| home.path().join(format!("{}.env", self.agent.metadata.name)))
    }
}

/// Where an agent comes from.
struct Source {
    /// The folder of the manifest the person's variant extends, where its relative paths resolve.
    directory: PathBuf,
    /// The manifest the person's variant extends, such as `agent.yaml`.
    extends: String,
    /// The person's variant beside it, such as `agent.mine.yaml`, which may exist already.
    variant: PathBuf,
    /// What the agent came from, as the review names it.
    origin: String,
}

/// An agent's values and logins, gathered before it is created.
#[derive(Default)]
struct Access {
    /// Values for the agent's values file.
    values: Vec<(String, Zeroizing<String>)>,
    /// Values already known, such as from `--env-file` or the agent's own values file, used before
    /// anything is asked.
    known_values: BTreeMap<String, Zeroizing<String>>,
    /// Signing in to GitHub in the browser, as this account when one is named, once the sign-in
    /// agent runs.
    github_browser: Option<BrowserSignIn>,
    /// The GitHub account the agent's token acts as, once it is checked.
    github_account: Option<String>,
    /// The harnesses to sign in to once the sign-in agent runs.
    sign_in_to: Vec<Harness>,
    /// The harnesses that use a login agentd holds already.
    reused_logins: Vec<Harness>,
}

/// A GitHub sign-in through the browser, waiting for the sign-in agent.
struct BrowserSignIn {
    /// The account it must be, when one is named.
    account: Option<String>,
}

pub(crate) async fn run(arguments: SetupArguments, home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    let interactive = crate::prompt::interactive() && !arguments.non_interactive;
    // The recommended answers usually fit, so setup fills them in and shows them for review, until
    // the person asks to change something. A dry run shows the variant instead of a review.
    let mut fill_in = interactive && !arguments.yes && !arguments.dry_run;
    let mut replay = None;
    loop {
        let mut answers = Answers::new(interactive, arguments.yes);
        if let Some(replay) = replay.take() {
            answers = answers.replaying(replay);
        }
        let result = Box::pin(run_once(&arguments, &answers, fill_in, home, client)).await;
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
    home: &ControlPlaneHome,
    client: &Client,
) -> CommandResult<()> {
    // What a run needs for a while, such as the values file a person edits, lives under the
    // control-plane home, so that it never lands in a folder a person mounts.
    home.prepare()?;
    let leftovers = crate::sign_in::sweep(home.path());
    let staging = crate::sign_in::Staging::create(home.path())?;
    let agents = list_agents(arguments, home, client, &leftovers).await?;
    let save_as = arguments
        .save_as
        .clone()
        .unwrap_or_else(|| PERSONAL_VARIANT.parse().unwrap_or_else(|_| unreachable!()));
    let here = std::env::current_dir()
        .map(|directory| crate::discovery::working_tree_manifests(&directory))
        .unwrap_or_default();
    let source = choose_source(arguments, answers, !here.is_empty())?;
    let mut draft = choose_draft(arguments, answers, source, &save_as).await?;
    if draft.changing {
        // agentd remembers the values file the agent was applied with, which may be another.
        draft.values_file = agents
            .iter()
            .find(|agent| agent.metadata.name == draft.agent.metadata.name)
            .and_then(|agent| agent.status.provenance.as_ref()?.env_file.clone());
    }
    if fill_in {
        answers.assume_defaults(true);
    }
    let names = Names { agents: &agents };
    let shape = Shape {
        arguments,
        answers,
        storage: home.path(),
        names: &names,
    };
    shape.apply(&mut draft)?;
    let protected = protected_folders(home);
    choose_shared(arguments, answers, &mut draft, &protected)?;
    if !draft.agent.spec.access.contains(&AccessSpec::Ssh {}) {
        draft.agent.spec.access.push(AccessSpec::Ssh {});
    }
    if arguments.edit {
        edit::variant(answers, &mut draft)?;
    }
    check_host_paths(&draft, &protected)?;
    if arguments.dry_run {
        let path = &draft.source.variant;
        println!("# {}", path.display());
        print!("{}", create::variant_text(&draft, path)?);
        return Ok(());
    }
    if let Some(env_file) = &arguments.env_file {
        if !env_file.is_file() {
            return Err(CommandError::Message(format!("{} does not exist", env_file.display())));
        }
        draft.access.known_values = agent::environment::read_or_empty(env_file).await?;
    }
    let values_path = draft.values_path(home);
    for (name, value) in agent::environment::read_or_empty(&values_path).await? {
        draft.access.known_values.entry(name).or_insert(value);
    }
    choose_github(arguments, answers, &mut draft).await?;
    give_identity(arguments, &mut draft);
    values::collect(answers, staging.path(), arguments.edit_values, &mut draft).await?;
    choose_logins(arguments, answers, client, &mut draft).await?;
    answers.assume_defaults(arguments.yes);
    loop {
        match review(answers, &draft, home, fill_in)? {
            Review::Create => break,
            Review::Change => {
                answers.start_over();
                return Ok(());
            }
            Review::Edit => {
                edit::variant(answers, &mut draft)?;
                check_host_paths(&draft, &protected)?;
            }
            Review::EditValues => values::edit(answers, staging.path(), &mut draft).await?,
            Review::Stop => {
                println!("Nothing was written.");
                return Ok(());
            }
        }
    }
    answers.no_way_back();
    sign_in_where_needed(arguments, answers, client, home, &staging, &mut draft).await?;
    // What can fail before anything is written is done first, so a failure leaves nothing behind.
    let builds = matches!(
        draft.agent.spec.sandbox.image,
        sandbox::image::ImageSource::Build { .. }
    );
    if builds && !arguments.no_apply {
        docker::ensure().await?;
    }
    finish(arguments, answers, client, home, &draft).await
}

/// Checks what of this machine the agent gets, the folders it mounts and the files its definition
/// reads from outside its repository, before anything is written.
fn check_host_paths(draft: &Draft, protected: &Protected) -> CommandResult<()> {
    check_mounts(draft, protected)?;
    check_sources(draft, protected)
}

/// agentd's agents, but not the short-lived sign-in agents. A dry run changes nothing, so it needs
/// no daemon and sees no agents.
async fn list_agents(
    arguments: &SetupArguments,
    home: &ControlPlaneHome,
    client: &Client,
    leftovers: &crate::sign_in::Leftovers,
) -> CommandResult<Vec<Agent>> {
    if arguments.dry_run {
        return Ok(Vec::new());
    }
    crate::ensure_daemon(home, client).await?;
    let mut agents = client.list_agents().await?;
    remove_dead_sign_in_agents(client, &agents, leftovers).await;
    agents.retain(|agent| !crate::sign_in::is_sign_in_agent(&agent.metadata.name));
    Ok(agents)
}

/// Where the agent comes from: a flag, else the person's choice, offering the agents the repository
/// setup runs in defines first when `offer_here` says it has some. One of them may be the person's
/// own variant, from an earlier setup, which setup then changes.
fn choose_source(arguments: &SetupArguments, answers: &Answers, offer_here: bool) -> CommandResult<SourceChoice> {
    let flagged = match (arguments.source, arguments.from.as_deref()) {
        (Some(source), _) => Some(source),
        (None, Some(_)) => Some(SourceChoice::Team),
        (None, None) => None,
    };
    if let Some(source) = flagged {
        return Ok(source);
    }
    let mut choices = Vec::new();
    if offer_here {
        choices.push(Choice::new(SourceChoice::Here, "With an agent this repository defines").suggested());
    }
    choices.push(Choice::new(
        SourceChoice::Team,
        "With an agent definition that exists, such as your team's, in a folder or on GitHub",
    ));
    let choices = choices
        .into_iter()
        .map(|choice| {
            let source = value_name(&choice.value);
            choice.flag("--source", &[&source])
        })
        .collect();
    Ok(answers.choose("--source", "How do you want to start?", choices, 0)?)
}

/// How `value` is spelled on the command line.
fn value_name(value: &impl ValueEnum) -> String {
    value
        .to_possible_value()
        .map_or_else(String::new, |value| value.get_name().to_owned())
}
