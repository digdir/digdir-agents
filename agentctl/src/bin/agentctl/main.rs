use std::{
    io::IsTerminal as _,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, ExitCode, Stdio},
    time::Duration,
};

use agent::{
    Agent, AgentVariantName, Error, RunState,
    control_api::Client,
    control_plane::ApplyRequest,
    control_plane::WaitPolicy,
    local::home::ControlPlaneHome,
    manifest,
    sandbox::forward,
    sessions::{Session, SessionName, SessionRequest},
};
use clap::{Parser, Subcommand, ValueEnum};

mod format;
mod github;
mod host;
mod init;
mod launch;
mod progress;
mod prompt;
mod self_update;
mod tools;
mod tui;

use format::{condition_status, format_age, format_harnesses, session_state};
use futures_util::StreamExt as _;
use sandbox::{execution::ExecutionEvent, terminal::TerminalAttachOutcome};
use tokio::io::AsyncWriteExt as _;
use tokio::runtime::LocalRuntime;

#[derive(Parser)]
#[command(
    name = "agentctl",
    about = "Run coding agents in sandboxes on this machine",
    after_help = "New here? Run `agentctl` in a repository: it sets up an agent and opens a session.\nTo stop every agent at once: agentctl stop --all --now",
    version = agent::build_version()
)]
struct Arguments {
    /// Agent control-plane home.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Without one, `agentctl` offers to open or set up the current directory's agent.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Set up an agent for this repository, signing in as needed.
    Init,
    /// Show the logins, agentd and this directory's agent, with what to do next.
    Status,
    /// Open the terminal UI with every agent and session.
    Tui,
    /// Open a session on this directory's agent: its only one, a new one, or one by name.
    Attach {
        /// Session resource, optionally combined with its name (for example `session/s1`); without
        /// one, the current directory's agent's only session, a new one, or one you choose.
        resource: Option<String>,
        /// Optional session name when it is not part of `resource`.
        name: Option<String>,
        /// Owning agent; inferred from the current directory when omitted.
        #[arg(long, conflicts_with = "variant")]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with = "agent")]
        variant: Option<AgentVariantName>,
        #[command(flatten)]
        selection: SessionSelection,
    },
    /// Stop an agent, or every agent; disks are kept, and sessions resume after a start.
    ///
    /// Running harnesses are stopped with the VM. Applying the manifest again keeps the agent stopped.
    /// Without an agent, asks whether to stop every running agent; `--all --now` does so at once.
    Stop {
        #[command(flatten)]
        target: StopTarget,
    },
    /// Stop an agent, or every agent, at once, as `stop --now` does: an emergency stop.
    ///
    /// The VMs end without shutting down; disks are kept, but what an agent wrote in its last
    /// seconds may be lost. Without an agent, asks whether to stop every agent not stopped yet.
    Kill {
        #[command(flatten)]
        target: KillTarget,
    },
    /// Start a stopped agent on its kept disk and wait until it is ready.
    Start {
        #[command(flatten)]
        target: RunStateTarget,
    },
    /// Show or change the tools an agent's image installs.
    ///
    /// The current directory's agent, or the one `--agent` names. After a change, it offers to
    /// recreate the agent with the new tools.
    Tools {
        /// The agent whose tools to show or change.
        #[arg(long, global = true)]
        agent: Option<String>,
        #[command(subcommand)]
        command: Option<ToolsCommand>,
    },
    /// Let agents work on GitHub, and manage the accounts they use.
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
    /// Sign agents in to Claude Code.
    Claude {
        #[command(subcommand)]
        command: ClaudeCommand,
    },
    /// Sign agents in to Codex.
    Codex {
        #[command(subcommand)]
        command: CodexCommand,
    },
    /// Edit the instructions every agent on this machine gets, ahead of its own.
    Instructions {
        #[command(subcommand)]
        command: InstructionsCommand,
    },
    /// Run a command in an agent.
    Exec {
        /// Pass stdin to an allocated terminal.
        #[arg(short = 'i', long, requires = "tty")]
        stdin: bool,
        /// Allocate a terminal; currently used together with --stdin.
        #[arg(short = 't', long, requires = "stdin")]
        tty: bool,
        /// Agent resource or name; inferred from the current directory when omitted.
        resource: Option<String>,
        /// Agent name, as an alternative to the positional resource.
        #[arg(long, conflicts_with_all = ["resource", "variant"])]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["agent", "resource"])]
        variant: Option<AgentVariantName>,
        /// Command and arguments to execute after `--`.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// Open an SSH session to an agent.
    Ssh {
        /// Agent name, as an alternative to the positional resource.
        #[arg(long, conflicts_with_all = ["resource", "variant"])]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["agent", "resource"])]
        variant: Option<AgentVariantName>,
        /// Agent resource or name; inferred from the current directory when omitted.
        resource: Option<String>,
        /// Remote command and arguments after `--`; an interactive shell when omitted.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Forward an agent's desktop to a local VNC port until interrupted.
    Vnc {
        /// Agent name, as an alternative to the positional resource.
        #[arg(long, conflicts_with_all = ["resource", "variant"])]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["agent", "resource"])]
        variant: Option<AgentVariantName>,
        /// Agent resource or name; inferred from the current directory when omitted.
        resource: Option<String>,
        /// Local port to listen on. Defaults to a free port, which is printed.
        #[arg(long)]
        port: Option<u16>,
        /// Forward the browser-based viewer instead of the raw RFB port, so no VNC client is needed.
        #[arg(long)]
        web: bool,
        /// Hand the address to the local browser or VNC handler instead of only printing it.
        #[arg(long)]
        open: bool,
    },
    /// Forward local ports to a running agent sandbox until interrupted.
    PortForward {
        /// Agent name, as an alternative to a leading agent argument.
        #[arg(long, conflicts_with = "variant")]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with = "agent")]
        variant: Option<AgentVariantName>,
        /// Optional leading agent resource or name, followed by port mappings
        /// written as GUEST, LOCAL:GUEST, or ADDRESS:LOCAL:GUEST. An empty
        /// local port (`:GUEST`) selects an ephemeral local port. The agent is
        /// inferred from the current directory when no leading agent is given.
        #[arg(required = true, num_args = 1..)]
        arguments: Vec<String>,
    },
    /// Create a session and wait until its harness is ready, without attaching.
    Create {
        #[command(flatten)]
        target: SessionTarget,
        #[command(flatten)]
        selection: SessionSelection,
        /// Maximum wait, written as seconds, minutes, or hours.
        #[arg(long, default_value = "10m", value_parser = parse_duration)]
        timeout: Duration,
        /// First prompt, handed to the harness at launch.
        #[command(flatten)]
        input: PromptInput,
    },
    /// Deliver a prompt to a running session's harness.
    Prompt {
        #[command(flatten)]
        target: SessionTarget,
        #[command(flatten)]
        input: PromptInput,
        #[command(flatten)]
        completion: CompletionOptions,
    },
    /// Read a session's conversation as turns.
    Turns {
        #[command(flatten)]
        target: SessionTarget,
        /// Print only the last N turns.
        #[arg(long)]
        last: Option<usize>,
    },
    /// Display one or more resources.
    Get {
        /// Resource kind, optionally combined with a name (for example `agent/worker`).
        resource: String,
        /// Optional resource name when it is not part of `resource`.
        name: Option<String>,
        /// Owning agent for session resources; inferred from the current directory when omitted.
        #[arg(long, conflicts_with = "variant")]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with = "agent")]
        variant: Option<AgentVariantName>,
        /// List sessions across every agent instead of resolving one owner.
        #[arg(short = 'A', long, conflicts_with_all = ["agent", "variant"])]
        all_agents: bool,
        /// Include archived sessions in a session listing.
        #[arg(long)]
        archived: bool,
        /// Output format.
        #[arg(short = 'o', long, default_value = "table", value_enum)]
        output: OutputFormat,
    },
    /// Show detailed state and conditions for one resource.
    Describe {
        /// Agent resource, optionally combined with its name (for example `agent/worker`).
        resource: String,
        /// Optional agent name when it is not part of `resource`.
        name: Option<String>,
        /// Output format.
        #[arg(short = 'o', long, default_value = "table", value_enum)]
        output: OutputFormat,
    },
    /// Create or update an agent from a manifest.
    Apply {
        /// Agent manifest path; defaults to ./agent.yaml.
        #[arg(short = 'f', long = "filename", conflicts_with = "variant")]
        filename: Option<PathBuf>,
        /// Variant of the agent in the current directory.
        #[arg(long, value_parser = parse_variant_name, conflicts_with = "filename")]
        variant: Option<AgentVariantName>,
        /// Override metadata.name so one manifest can create multiple agents.
        #[arg(long)]
        name: Option<String>,
        /// File supplying declared manifest environment and secret values; defaults to `.env`
        /// beside the manifest. Keep files containing secrets outside bind-mounted directories.
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// Stay attached after applying and show provisioning progress until the agent is Ready.
        #[arg(long)]
        wait: bool,
        /// Maximum wait with `--wait`, written as seconds, minutes, or hours (for example `10m`).
        #[arg(long, default_value = "10m", value_parser = parse_duration, requires = "wait")]
        timeout: Duration,
    },
    /// Request deletion of a resource.
    Delete {
        /// Resource kind, optionally combined with a name (for example `agent/worker`).
        resource: String,
        /// Optional resource name when it is not part of `resource`.
        name: Option<String>,
        /// Owning agent for session resources; inferred from the current directory when omitted.
        #[arg(long, conflicts_with = "variant")]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with = "agent")]
        variant: Option<AgentVariantName>,
    },
    /// Archive a session: stop its harness and hide it from listings, keeping its name and conversation.
    Archive {
        #[command(flatten)]
        target: SessionTarget,
    },
    /// Unarchive a session; the next attach resumes its conversation.
    Unarchive {
        #[command(flatten)]
        target: SessionTarget,
    },
    /// Wait for a resource condition.
    Wait {
        /// Condition expression. Only `condition=Ready` is currently supported.
        #[arg(long = "for", default_value = "condition=Ready")]
        condition: String,
        /// Maximum wait, written as seconds, minutes, or hours (for example `30s` or `10m`).
        #[arg(long, default_value = "10m", value_parser = parse_duration)]
        timeout: Duration,
        /// Agent resource, optionally combined with its name (for example `agent/worker`).
        resource: String,
        /// Optional agent name when it is not part of `resource`.
        name: Option<String>,
    },
    /// Manage the OpenSSH client configuration for agents.
    SshConfig {
        #[command(subcommand)]
        command: SshConfigCommand,
    },
    /// Describe how to reach an agent over SSH.
    SshInfo {
        /// Agent resource or name; inferred from the current directory when omitted.
        resource: Option<String>,
        /// Agent name, as an alternative to the positional resource.
        #[arg(long, conflicts_with_all = ["resource", "variant"])]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["agent", "resource"])]
        variant: Option<AgentVariantName>,
        /// Output format.
        #[arg(short = 'o', long, default_value = "table", value_enum)]
        output: OutputFormat,
    },
    /// Describe how to reach an agent's desktop over VNC.
    VncInfo {
        /// Agent resource or name; inferred from the current directory when omitted.
        resource: Option<String>,
        /// Agent name, as an alternative to the positional resource.
        #[arg(long, conflicts_with_all = ["resource", "variant"])]
        agent: Option<String>,
        /// Select the closest agent by its applied leaf variant.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["agent", "resource"])]
        variant: Option<AgentVariantName>,
        /// Output format.
        #[arg(short = 'o', long, default_value = "table", value_enum)]
        output: OutputFormat,
    },
    /// Update agentctl and agentd.
    Self_ {
        #[command(subcommand)]
        command: self_update::SelfCommand,
    },
    /// Relay one connection to an agent's SSH server over standard input and output.
    ///
    /// The generated OpenSSH client configuration runs this as its `ProxyCommand`.
    #[command(hide = true)]
    SshProxy {
        /// Agent resource or name.
        resource: String,
    },
    /// Relay one connection to an agent's desktop over standard input and output.
    #[command(hide = true)]
    VncProxy {
        /// Agent resource or name.
        resource: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resource {
    Agent,
    Session,
}

/// The Agent `stop` or `start` acts on: `agent/NAME` or `agent NAME`, and how long to wait.
#[derive(clap::Args)]
struct RunStateTarget {
    /// Agent resource, optionally combined with its name (for example `agent/worker`).
    resource: String,
    /// Optional agent name when it is not part of `resource`.
    name: Option<String>,
    /// Maximum wait, written as seconds, minutes, or hours (for example `2m`).
    #[arg(long, default_value = "10m", value_parser = parse_duration)]
    timeout: Duration,
}

/// The agents `stop` acts on: one, every running one, or, unnamed, every running one after asking.
#[derive(clap::Args)]
struct StopTarget {
    /// Agent resource, optionally combined with its name (for example `agent/worker`).
    #[arg(conflicts_with = "all")]
    resource: Option<String>,
    /// Optional agent name when it is not part of `resource`.
    name: Option<String>,
    /// Stop every agent not stopped yet, including one still stopping, without asking.
    #[arg(long)]
    all: bool,
    /// End the VMs at once instead of letting them shut down, as an emergency stop. Whatever an
    /// agent wrote in its last seconds may be lost.
    #[arg(long)]
    now: bool,
    /// Maximum wait, written as seconds, minutes, or hours (for example `2m`).
    #[arg(long, default_value = "10m", value_parser = parse_duration)]
    timeout: Duration,
}

/// The Agents `kill` acts on, as for `stop`; it always stops them at once.
#[derive(clap::Args)]
struct KillTarget {
    /// Agent resource, optionally combined with its name (for example `agent/worker`).
    #[arg(conflicts_with = "all")]
    resource: Option<String>,
    /// Optional agent name when it is not part of `resource`.
    name: Option<String>,
    /// Stop every agent not stopped yet, including one still stopping, without asking.
    #[arg(long)]
    all: bool,
    /// Maximum wait, written as seconds, minutes, or hours (for example `2m`).
    #[arg(long, default_value = "10m", value_parser = parse_duration)]
    timeout: Duration,
}

impl From<KillTarget> for StopTarget {
    fn from(target: KillTarget) -> Self {
        Self {
            resource: target.resource,
            name: target.name,
            all: target.all,
            now: true,
            timeout: target.timeout,
        }
    }
}

/// The Session a verb acts on: `session/NAME` or `session NAME`, plus its owning Agent.
#[derive(clap::Args)]
struct SessionTarget {
    /// Session resource, optionally combined with its name (for example `session/s1`).
    resource: String,
    /// Optional session name when it is not part of `resource`.
    name: Option<String>,
    /// Owning agent; inferred from the current directory when omitted.
    #[arg(long, conflicts_with = "variant")]
    agent: Option<String>,
    /// Select the closest agent by its applied leaf variant.
    #[arg(long, value_parser = parse_variant_name, conflicts_with = "agent")]
    variant: Option<AgentVariantName>,
}

/// Selections fixed when a command creates a Session. An existing Session keeps
/// its recorded values; naming different ones is an error.
#[derive(Default, clap::Args)]
struct SessionSelection {
    /// Harness installation to bind when creating the session.
    #[arg(long, value_parser = parse_harness)]
    harness: Option<agent::Harness>,
    /// Model the harness launches with, in the harness's own spelling (for example
    /// `fable` for Claude Code). Defaults to the installation's manifest `model`,
    /// else the harness default.
    #[arg(long, value_parser = parse_model)]
    model: Option<agent::Model>,
    /// Effort level the harness launches with, in the harness's own spelling (for
    /// example `high`). Defaults to the installation's manifest `effort`, else the
    /// harness default.
    #[arg(long, value_parser = parse_effort)]
    effort: Option<agent::Effort>,
}

impl SessionSelection {
    fn request(self, initial_prompt: Option<String>) -> SessionRequest {
        SessionRequest {
            harness: self.harness,
            model_selection: agent::ModelSelection {
                model: self.model,
                effort: self.effort,
            },
            initial_prompt,
        }
    }
}

/// Whether a prompt waits for the next turn completion.
#[derive(clap::Args)]
struct CompletionOptions {
    /// Wait for a turn completion and identical waiting activity in two polls
    /// 250 ms apart, following newly observed work. Inspect output with `turns`.
    #[arg(long)]
    wait: bool,
    /// Completion wait after submission, excluding setup and delivery; seconds, minutes, or hours.
    #[arg(long, default_value = "10m", value_parser = parse_duration, requires = "wait")]
    timeout: Duration,
}

/// Prompt text from --prompt, --file, or piped standard input.
#[derive(clap::Args)]
struct PromptInput {
    /// Prompt text. Read from a file with --file, or from standard input when
    /// neither is given and stdin is piped.
    #[arg(long, conflicts_with = "file", allow_hyphen_values = true)]
    prompt: Option<String>,
    /// Read the prompt from a file instead of --prompt.
    #[arg(short = 'f', long, conflicts_with = "prompt")]
    file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

#[derive(Debug, thiserror::Error)]
enum CommandError {
    #[error(transparent)]
    Agent(#[from] Error),
    #[error("{0}")]
    Message(String),
}

type CommandResult<T> = Result<T, CommandError>;

#[derive(Subcommand)]
enum ClaudeCommand {
    /// Mint a long-lived Claude token on the host and store it for agents.
    Login {
        /// Read an existing credential from standard input instead of signing in. Inside an agent
        /// this accepts the mediated placeholder the session holds as `AGENT_CLAUDE_ACCESS_TOKEN`,
        /// so a nested `agentd` chains through the outer mediation without ever holding a real
        /// credential.
        #[arg(long)]
        from_stdin: bool,
    },
    /// Say whether agents can use Claude Code; exits with 1 when no login is stored.
    Status,
}

#[derive(Subcommand)]
enum SshConfigCommand {
    /// Include the generated agent configuration from `~/.ssh/config`, once, at the top.
    Install,
}

#[derive(Subcommand)]
enum CodexCommand {
    /// Sign in with `ChatGPT` and store an agent-only grant.
    Login {
        /// Read the harness's credential file from standard input instead of signing in. Inside
        /// an agent this accepts the file the harness already has, whose placeholders let a nested
        /// `agentd` chain through the outer mediation without ever holding a real credential.
        #[arg(long)]
        from_stdin: bool,
    },
    /// Say whether agents can use Codex; exits with 1 when no login is stored.
    Status,
}

#[derive(Subcommand)]
enum ToolsCommand {
    /// Add tools: an Ubuntu package such as `jq`, a toolchain such as `go@1.26`, `node@20`,
    /// `dotnet`, `rust`, `python`, `java` or `maven`, an npm package as `npm:pnpm`, or
    /// `containers` or `browser`.
    Add {
        #[arg(required = true)]
        tools: Vec<String>,
    },
    /// Remove tools, named as for `add`.
    Remove {
        #[arg(required = true)]
        tools: Vec<String>,
    },
}

#[derive(Subcommand)]
enum InstructionsCommand {
    /// Open the host-wide instructions in your editor, creating the file if needed.
    ///
    /// Every agent's harness receives them ahead of the agent's own instructions, within one
    /// reconciliation pass and without recreating the agent. Empty the file to give agents none.
    Edit,
}

#[derive(Subcommand)]
enum GithubCommand {
    /// Store a GitHub token for agents, separate from any `gh` login on this host.
    ///
    /// By default it walks you through creating a fine-grained token in GitHub's pre-filled
    /// form, for the owner of the current directory's repository. The token is checked and
    /// summarized before anything is stored. Agents whose manifest lists
    /// `accounts: [{type: github, owner: …}]` use the owner's token: they see an inert
    /// `GITHUB_TOKEN` that only requests to GitHub receive, and commit as its account.
    Login {
        /// The user or organization the token is for; by default the owner of the current
        /// directory's GitHub repository.
        #[arg(long, value_parser = parse_github_owner)]
        owner: Option<String>,
        /// Read the token from standard input instead of creating one.
        #[arg(long, group = "source")]
        from_stdin: bool,
        /// Use the token of the host's own `gh` login, which is usually your own account and
        /// reaches every repository it can.
        #[arg(long, group = "source")]
        from_gh: bool,
        /// Sign in through the GitHub CLI in the browser instead. The token reaches every
        /// repository the account can, so you name the account first.
        #[arg(long, group = "source")]
        browser: bool,
    },
    /// List the stored tokens: owner, account, kind and expiry.
    List,
    /// Forget a stored token, named by its owner or account or chosen from a list, or every one.
    ///
    /// Agents lose GitHub access on their next pass. The token stays valid on GitHub until you
    /// revoke it there.
    Logout {
        /// The token to forget, by its owner or the account it acts as, as `agentctl github list`
        /// shows them.
        #[arg(conflicts_with_all = ["owner", "everywhere"])]
        name: Option<String>,
        /// Forget the token for this user or organization, without asking which.
        #[arg(long, value_parser = parse_github_owner, conflicts_with = "everywhere")]
        owner: Option<String>,
        /// Forget every stored token, without asking which.
        #[arg(long)]
        everywhere: bool,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Forget every stored GitHub token, to start over.
    Reset {
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

fn parse_github_owner(value: &str) -> Result<String, String> {
    if agent::github::valid_owner(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(format!("{value:?} is not a GitHub user or organization"))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        // agentd's message is the whole story, apart from how to change what cannot be changed.
        Err(CommandError::Agent(Error::Rpc(error))) => {
            eprintln!("agentctl: {}", error.message);
            if error.message.starts_with("immutable Agent field changed") {
                eprintln!(
                    "That changes only by creating the agent again, which starts its disk over: `agentctl delete agent/NAME`, then apply."
                );
            }
            ExitCode::FAILURE
        }
        // Most of agentctl's own refusals are `Invalid`, and few are about an Agent.
        Err(CommandError::Agent(Error::Invalid(message))) => {
            eprintln!("agentctl: {message}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("agentctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> CommandResult<ExitCode> {
    let arguments = Arguments::parse();
    let home = ControlPlaneHome::resolve(arguments.home.as_deref())?;
    let client = Client::for_path(home.socket_path());
    LocalRuntime::new().map_err(Error::from)?.block_on(async move {
        let Some(command) = arguments.command else {
            return without_command(&home, &client).await;
        };
        if !matches!(command, Command::Self_ { .. }) {
            self_update::resume_pending_before_command(&home)?;
        }
        if !matches!(
            command,
            Command::Create { .. } | Command::Prompt { .. } | Command::Self_ { .. } | Command::SshConfig { .. }
        ) {
            ensure_daemon(&home, &client).await?;
        }
        // Boxed, as the future of every command's handler together is large on some targets.
        Box::pin(execute(command, &home, &client)).await
    })
}

/// `agentctl` on its own: asks whether to open the current directory's Agent in the TUI, or to
/// set one up when there is none, and prints the help otherwise. Nothing starts unasked, so a
/// mistyped command costs one answer.
async fn without_command(home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    use clap::CommandFactory as _;

    if !prompt::interactive() {
        // As clap reports a missing command: the help on standard error, exit code 2.
        eprint!("{}", Arguments::command().render_help());
        return Ok(ExitCode::from(2));
    }
    self_update::resume_pending_before_command(home)?;
    ensure_daemon(home, client).await?;
    let directory = std::env::current_dir().map_err(Error::from)?;
    match client.resolve_agent_variant(directory, None).await {
        Ok(agent) => {
            let name = agent.metadata.name;
            if agent.spec.is_stopped() {
                let question = format!("This directory has the agent {name}, which is stopped. Start it?");
                if prompt::confirm(&question)? {
                    client.set_run_state(&name, RunState::Running).await?;
                    wait_until_converged_simply(client, &name, Duration::from_mins(10)).await?;
                    attach(home, client, None, None, Some(name), None, SessionSelection::default()).await?;
                    return Ok(ExitCode::SUCCESS);
                }
            }
            let question = format!("This directory has the agent {name}. What would you like to do?");
            if let Some(code) = agents_here_menu(home, client, &[&question]).await? {
                return Ok(code);
            }
        }
        // Several agents match this directory, such as a repository with more than one.
        Err(Error::Rpc(error)) if error.is_invalid_params() => {
            if let Some(code) = agents_here_menu(
                home,
                client,
                &["This directory has several agents. What would you like to do?"],
            )
            .await?
            {
                return Ok(code);
            }
        }
        Err(Error::Rpc(error)) if error.is_not_found() => {
            let manifests = tui::working_tree_manifests(&std::env::current_dir().map_err(Error::from)?);
            let start = if manifests.is_empty() {
                prompt::confirm("No agent was set up for this directory. Set one up now?")?
                    .then_some(init::Start::Confirmed)
            } else {
                match init::choose_from_manifests(&manifests, true)? {
                    init::Choice::Start(start) => Some(start),
                    init::Choice::Help => None,
                    init::Choice::Cancel => return Ok(ExitCode::SUCCESS),
                }
            };
            if let Some(start) = start {
                if let Some(name) = Box::pin(init::run(home, client, start)).await?
                    && prompt::confirm(&format!("Open a session on {name} now?"))?
                {
                    attach_new_session(home, client, &name).await?;
                }
                return Ok(ExitCode::SUCCESS);
            }
        }
        Err(error) => return Err(inference_error(error)),
    }
    Arguments::command().print_help().map_err(Error::from)?;
    Ok(ExitCode::SUCCESS)
}

/// Offers what to do where agents already exist: open the TUI, set up another agent, or neither,
/// which leaves the help to the caller.
async fn agents_here_menu(home: &ControlPlaneHome, client: &Client, lines: &[&str]) -> CommandResult<Option<ExitCode>> {
    let options = [
        "Open agentctl tui (recommended)".to_owned(),
        "Set up another agent here".to_owned(),
        "Show agentctl's help".to_owned(),
    ];
    match prompt::choose_lines(lines, &options, Some(0))? {
        Some(0) => tui::run(home, client).await.map(Some),
        Some(1) => {
            if let Some(name) = Box::pin(init::run(home, client, init::Start::New)).await?
                && prompt::confirm(&format!("Open a session on {name} now?"))?
            {
                attach_new_session(home, client, &name).await?;
            }
            Ok(Some(ExitCode::SUCCESS))
        }
        _ => Ok(None),
    }
}

/// Offers `agentctl init` before a command that would use the current directory's Agent finds
/// none, returning the Agent it set up. The command then targets that Agent by name, which also
/// covers one created from a manifest in another directory of the checkout. Only when a person
/// is at the terminal and no Agent was named: scripts get the usual error.
async fn offer_init_if_missing(
    home: &ControlPlaneHome,
    client: &Client,
    inferred: bool,
) -> CommandResult<Option<String>> {
    if !inferred || !prompt::interactive() {
        return Ok(None);
    }
    let directory = std::env::current_dir().map_err(Error::from)?;
    if let Err(Error::Rpc(error)) = client.resolve_agent_variant(directory.clone(), None).await
        && error.is_not_found()
    {
        // With agents set up here, init's own menu offers them or a new one, and can be cancelled.
        if !tui::working_tree_manifests(&directory).is_empty() {
            return Box::pin(init::run(home, client, init::Start::Ask)).await;
        }
        if prompt::confirm("No agent was set up for this directory. Set one up now?")? {
            return Box::pin(init::run(home, client, init::Start::Confirmed)).await;
        }
    }
    Ok(None)
}

#[allow(
    clippy::too_many_lines,
    reason = "keep command dispatch together; behavior lives in the handlers"
)]
async fn execute(command: Command, home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    match command {
        Command::Self_ { command } => self_update::execute(command, home).await?,
        Command::Claude {
            command: ClaudeCommand::Login { from_stdin },
        } => harness_login(home, client, agent::Harness::ClaudeCode, from_stdin).await?,
        Command::Codex {
            command: CodexCommand::Login { from_stdin },
        } => harness_login(home, client, agent::Harness::Codex, from_stdin).await?,
        Command::Claude {
            command: ClaudeCommand::Status,
        } => return harness_status(client, agent::Harness::ClaudeCode).await,
        Command::Codex {
            command: CodexCommand::Status,
        } => return harness_status(client, agent::Harness::Codex).await,
        Command::Status => status(home, client).await?,
        Command::Instructions {
            command: InstructionsCommand::Edit,
        } => edit_instructions(home)?,
        Command::Tools { agent, command } => {
            let (add, remove) = match command {
                Some(ToolsCommand::Add { tools }) => (tools, Vec::new()),
                Some(ToolsCommand::Remove { tools }) => (Vec::new(), tools),
                None => (Vec::new(), Vec::new()),
            };
            tools::run(home, client, agent.as_deref(), &add, &remove).await?;
        }
        Command::Github {
            command:
                GithubCommand::Login {
                    owner,
                    from_stdin,
                    from_gh,
                    browser,
                },
        } => {
            let source = if from_stdin {
                github::Source::Stdin
            } else if from_gh {
                github::Source::HostGh
            } else if browser {
                github::Source::Browser
            } else {
                github::Source::Form
            };
            github::login(client, source, owner).await?;
        }
        Command::Github {
            command: GithubCommand::List,
        } => github::list(client).await?,
        Command::Github {
            command:
                GithubCommand::Logout {
                    name,
                    owner,
                    everywhere,
                    yes,
                },
        } => {
            let target = if everywhere {
                Some(github::Target::Every)
            } else if let Some(owner) = owner {
                Some(github::Target::Owner(owner))
            } else {
                name.map(github::Target::Named)
            };
            github::logout(client, target, yes).await?;
        }
        Command::Github {
            command: GithubCommand::Reset { yes },
        } => github::reset(client, yes).await?,
        Command::Init => {
            if let Some(name) = Box::pin(init::run(home, client, init::Start::Ask)).await?
                && prompt::confirm(&format!("Open a session on {name} now?"))?
            {
                attach_new_session(home, client, &name).await?;
            }
        }
        Command::Apply {
            filename,
            variant,
            name,
            env_file,
            wait,
            timeout,
        } => {
            let filename = apply_manifest_path(filename, variant)?;
            let folder = std::path::absolute(&filename)
                .map_err(Error::from)?
                .parent()
                .map(Path::to_path_buf);
            if let Some(folder) = &folder {
                tools::sync(folder)?;
            }
            let mut request = read_apply_request(filename, env_file)?;
            if let Some(name) = name {
                request.agent.metadata.name = name;
            }
            let new = matches!(
                client.get(&request.agent.metadata.name).await,
                Err(Error::Rpc(ref error)) if error.is_not_found()
            );
            let applied = client.apply(request).await?;
            let name = applied.metadata.name;
            if new && let Some(folder) = &folder {
                tools::record(home, &name, folder);
            }
            println!("agent/{name} applied");
            if wait {
                let converged = wait_until_converged(client, &name, timeout).await?;
                let outcome = if converged.spec.is_stopped() {
                    "stopped"
                } else {
                    "ready"
                };
                println!("agent/{name} {outcome}");
            }
        }
        Command::Get {
            resource,
            name,
            agent,
            variant,
            all_agents,
            archived,
            output,
        } => get_resources(client, &resource, name, agent, variant, all_agents, archived, output).await?,
        Command::Describe { resource, name, output } => describe(client, &resource, name, output).await?,
        Command::Delete {
            resource,
            name,
            agent,
            variant,
        } => {
            let (resource, name) = resource_reference(&resource, name)?;
            if resource == Resource::Agent {
                reject_session_scope(agent.as_deref(), variant.as_ref(), false)?;
                let name = require_name(name, "Agent")?;
                client.delete(&name).await?;
                println!("agent/{name} deleted");
            } else {
                let name = SessionName::new(require_name(name, "Session")?)?;
                let agent = resolve_agent_name(client, agent, variant).await?;
                client.delete_session(&agent, name.clone()).await?;
                println!("session/{agent}/{name} deleted");
            }
        }
        Command::Stop { target } => stop(client, target).await?,
        Command::Kill { target } => stop(client, target.into()).await?,
        Command::Start { target } => set_run_state(client, target, RunState::Running).await?,
        Command::Archive { target } => set_archived(client, target, true).await?,
        Command::Unarchive { target } => set_archived(client, target, false).await?,
        Command::Attach {
            resource,
            name,
            agent,
            variant,
            selection,
        } => attach(home, client, resource.as_deref(), name, agent, variant, selection).await?,
        Command::Exec {
            stdin,
            tty,
            resource,
            agent,
            variant,
            command,
        } => return exec_command(home, client, resource, agent, variant, &command, stdin, tty).await,
        Command::PortForward {
            agent,
            variant,
            arguments,
        } => {
            return port_forward(home, client, agent, variant, &arguments).await;
        }
        Command::Ssh {
            agent,
            variant,
            resource,
            command,
        } => {
            let inferred = resource.is_none() && agent.is_none() && variant.is_none();
            let agent = agent.or(offer_init_if_missing(home, client, inferred).await?);
            return ssh(client, resource, agent, variant, &command).await;
        }
        Command::SshProxy { resource } => return ssh_proxy(home, client, resource).await,
        Command::SshConfig {
            command: SshConfigCommand::Install,
        } => install_ssh_config(home)?,
        Command::SshInfo {
            resource,
            agent,
            variant,
            output,
        } => ssh_info(client, resource, agent, variant, output).await?,
        Command::Vnc {
            agent,
            variant,
            resource,
            port,
            web,
            open,
        } => {
            let options = VncOptions { port, web, open };
            return vnc(home, client, resource, agent, variant, options).await;
        }
        Command::VncProxy { resource } => return vnc_proxy(home, client, resource).await,
        Command::VncInfo {
            resource,
            agent,
            variant,
            output,
        } => vnc_info(client, resource, agent, variant, output).await?,
        Command::Create {
            target,
            selection,
            input,
            timeout,
        } => create_session(home, client, target, selection, input, timeout).await?,
        Command::Prompt {
            target,
            input,
            completion,
        } => prompt_session(home, client, target, input, completion).await?,
        Command::Turns { target, last } => turns(client, target, last).await?,
        Command::Tui => return tui::run(home, client).await,
        Command::Wait {
            condition,
            timeout,
            resource,
            name,
        } => wait(client, &condition, timeout, &resource, name).await?,
    }
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments, reason = "mirrors the get command's flags")]
async fn get_resources(
    client: &Client,
    resource: &str,
    name: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    all_agents: bool,
    archived: bool,
    output: OutputFormat,
) -> CommandResult<()> {
    let (resource, name) = resource_reference(resource, name)?;
    match resource {
        Resource::Agent => {
            reject_session_scope(agent.as_deref(), variant.as_ref(), all_agents)?;
            let agents = if let Some(name) = name {
                vec![client.get(&name).await?]
            } else {
                client.list_agents().await?
            };
            match output {
                OutputFormat::Json => print_json(&agents)?,
                OutputFormat::Table => print_agents(&agents),
            }
        }
        Resource::Session => {
            let sessions = if name.is_some() {
                if all_agents {
                    return Err(Error::Invalid(
                        "a named session requires --agent or current-directory inference".into(),
                    )
                    .into());
                }
                let agent = resolve_agent_name(client, agent, variant).await?;
                vec![
                    client
                        .get_session(&agent, SessionName::new(require_name(name, "Session")?)?)
                        .await?,
                ]
            } else {
                let sessions = if all_agents {
                    client.list_sessions(None).await?
                } else {
                    let agent = resolve_agent_name(client, agent, variant).await?;
                    client.list_sessions(Some(&agent)).await?
                };
                sessions
                    .into_iter()
                    .filter(|session| archived || !session.is_archived())
                    .collect()
            };
            match output {
                OutputFormat::Json => print_json(&sessions)?,
                OutputFormat::Table => print_sessions(&sessions, all_agents),
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> CommandResult<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| Error::Invalid(error.to_string()))?
    );
    Ok(())
}

async fn attach(
    home: &ControlPlaneHome,
    client: &Client,
    resource: Option<&str>,
    name: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    selection: SessionSelection,
) -> CommandResult<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(CommandError::Message(
            "attach needs a terminal; `agentctl prompt` sends a session a prompt without one".into(),
        ));
    }
    let named = match resource {
        Some(resource) => {
            let (resource, name) = resource_reference(resource, name)?;
            if resource != Resource::Session {
                return Err(Error::Invalid("attach requires a session resource".into()).into());
            }
            Some(SessionName::new(require_name(name, "Session")?)?)
        }
        None => None,
    };
    let inferred = agent.is_none() && variant.is_none();
    let agent = agent.or(offer_init_if_missing(home, client, inferred).await?);
    let agent = resolve_agent_name(client, agent, variant).await?;
    offer_start_if_stopped(client, &agent).await?;
    let session = match named {
        Some(session) => session,
        None => SessionName::new(choose_session(client, &agent).await?)?,
    };
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_session(&agent, session, selection.request(None), WaitPolicy::UntilConverged),
        )
        .await?;
    agent::sessions::attach(home.path(), &target).await?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "command flags remain explicit at the execution boundary"
)]
async fn exec_command(
    home: &ControlPlaneHome,
    client: &Client,
    resource: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    command: &[String],
    stdin: bool,
    tty: bool,
) -> CommandResult<ExitCode> {
    let inferred = resource.is_none() && agent.is_none() && variant.is_none();
    let agent = agent.or(offer_init_if_missing(home, client, inferred).await?);
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    offer_start_if_stopped(client, &agent).await?;
    if tty && (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()) {
        return Err(Error::Invalid("-it requires an interactive local terminal".into()).into());
    }
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    let spec = agent::sandbox::platform::execution_spec(&target.operating_system, command, tty)?;
    let status = if stdin && tty {
        match agent::sandbox::attach_terminal(
            home.path(),
            &target.sandbox,
            ::sandbox::terminal::AttachTerminalRequest::new(spec),
        )
        .await?
        {
            TerminalAttachOutcome::Exited(status) => status,
            TerminalAttachOutcome::Detached => return Ok(ExitCode::SUCCESS),
            _ => return Err(Error::Session("terminal execution returned an unsupported outcome".into()).into()),
        }
    } else {
        let execution = agent::sandbox::start_execution(home.path(), &target, spec).await?;
        stream_execution(execution).await?
    };
    Ok(exit_code(status.code))
}

/// Splits a leading Agent reference from the port mappings.
///
/// A first argument containing ':' or made only of digits is a port mapping;
/// anything else names the Agent. Agent names cannot contain ':' and port
/// mappings cannot contain letters, so the shapes never overlap.
fn split_forward_arguments(arguments: &[String]) -> (Option<String>, &[String]) {
    match arguments.split_first() {
        Some((first, rest)) if !first.contains(':') && !first.bytes().all(|byte| byte.is_ascii_digit()) => {
            (Some(first.clone()), rest)
        }
        _ => (None, arguments),
    }
}

async fn port_forward(
    home: &ControlPlaneHome,
    client: &Client,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    arguments: &[String],
) -> CommandResult<ExitCode> {
    let (resource, ports) = split_forward_arguments(arguments);
    if agent.is_some() && resource.is_some() {
        return Err(Error::Invalid("the agent was supplied both as an argument and with --agent".into()).into());
    }
    if variant.is_some() && resource.is_some() {
        return Err(Error::Invalid("the agent was supplied both as an argument and with --variant".into()).into());
    }
    if ports.is_empty() {
        return Err(Error::Invalid("at least one port mapping is required".into()).into());
    }
    let specs = ports
        .iter()
        .map(|port| forward::ForwardSpec::parse(port))
        .collect::<Result<Vec<_>, String>>()
        .map_err(CommandError::Message)?;
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    let mut forwards = Vec::new();
    for spec in specs {
        let forward = forward::PortForward::start(home.path().to_path_buf(), target.sandbox.clone(), spec).await?;
        println!(
            "Forwarding from {} -> {} (agent {agent:?})",
            forward.local_address(),
            forward.spec().guest_port
        );
        forwards.push(forward);
    }
    hold_forwards(&forwards).await
}

/// Holds forwards open until interrupted, reporting each connection failure
/// once, and fails when every forward has stopped serving.
async fn hold_forwards(forwards: &[forward::PortForward]) -> CommandResult<ExitCode> {
    let mut reported = vec![None; forwards.len()];
    let mut poll = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.map_err(Error::from)?;
                return Ok(ExitCode::SUCCESS);
            }
            _ = poll.tick() => {
                for (forward, reported) in forwards.iter().zip(reported.iter_mut()) {
                    let status = forward.status();
                    if status != *reported {
                        if let Some(message) = &status {
                            eprintln!("{} -> {}: {message}", forward.local_address(), forward.spec().guest_port);
                        }
                        *reported = status;
                    }
                }
                if forwards.iter().all(forward::PortForward::finished) {
                    eprintln!("every port forward has stopped");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
    }
}

/// Opens the local OpenSSH client against the Agent's generated alias.
///
/// The Agent is converged first, so the server and the key material exist by
/// the time `ssh` runs the `ProxyCommand`.
async fn ssh(
    client: &Client,
    resource: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    command: &[String],
) -> CommandResult<ExitCode> {
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    let wait = progress::Wait::start();
    wait.until(
        client,
        &agent,
        client.ensure_execution(&agent, WaitPolicy::UntilConverged),
    )
    .await?;
    let access = client.ssh_access(&agent).await?;
    let mut ssh = ProcessCommand::new(ssh_client_executable());
    ssh.args(ssh_client_arguments(&access)).args(command);
    run_ssh_client(ssh)
}

fn ssh_client_executable() -> String {
    format!("ssh{}", std::env::consts::EXE_SUFFIX)
}

/// Arguments that point the OpenSSH client at an Agent's generated alias.
fn ssh_client_arguments(access: &agent::ssh::AccessInfo) -> [&std::ffi::OsStr; 3] {
    ["-F".as_ref(), access.config_file.as_os_str(), access.alias.as_ref()]
}

#[cfg(unix)]
fn run_ssh_client(mut ssh: ProcessCommand) -> CommandResult<ExitCode> {
    use std::os::unix::process::CommandExt as _;

    Err(ssh_client_error(&ssh.exec()))
}

#[cfg(not(unix))]
fn run_ssh_client(mut ssh: ProcessCommand) -> CommandResult<ExitCode> {
    let status = ssh.status().map_err(|error| ssh_client_error(&error))?;
    Ok(status.code().map_or(ExitCode::FAILURE, exit_code))
}

fn ssh_client_error(error: &std::io::Error) -> CommandError {
    CommandError::Message(ssh_client_failure(error))
}

/// Why the OpenSSH client could not be started.
fn ssh_client_failure(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        "the OpenSSH client `ssh` was not found on PATH; install OpenSSH".into()
    } else {
        format!("could not run the OpenSSH client: {error}")
    }
}

/// Relays one SSH connection over standard input and output; the `ProxyCommand` entry point.
///
/// Progress and errors go to standard error, which `ssh` shows to the user;
/// standard output carries only the SSH byte stream.
async fn ssh_proxy(home: &ControlPlaneHome, client: &Client, resource: String) -> CommandResult<ExitCode> {
    let agent = resolve_execution_agent(client, Some(resource), None, None).await?;
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    forward::relay_guest_port(
        home.path(),
        &target.sandbox,
        agent::ssh::GUEST_PORT,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await?;
    // A blocked standard-input read would keep the runtime from shutting down;
    // the relay is finished, so leave immediately.
    std::process::exit(0)
}

fn install_ssh_config(home: &ControlPlaneHome) -> CommandResult<()> {
    let include = agent::ssh::UserInclude::for_home(home)?;
    let (line, user_config) = (&include.line, include.user_config.display());
    match include.install()? {
        agent::ssh::IncludeOutcome::Installed => println!("added `{line}` at the top of {user_config}"),
        agent::ssh::IncludeOutcome::AlreadyInstalled => println!("{user_config} already contains `{line}`"),
    }
    Ok(())
}

async fn ssh_info(
    client: &Client,
    resource: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    output: OutputFormat,
) -> CommandResult<()> {
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    let access = client.ssh_access(&agent).await?;
    match output {
        OutputFormat::Json => print_json(&access)?,
        OutputFormat::Table => {
            for line in format::ssh_access_lines(&access) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

/// How one `agentctl vnc` invocation should expose the desktop.
struct VncOptions {
    /// Local port to listen on; a free port when omitted.
    port: Option<u16>,
    /// Forward the browser-based viewer rather than the raw RFB port.
    web: bool,
    /// Also hand the address to whichever local application handles its scheme.
    open: bool,
}

/// Forwards the Agent's desktop to a local port and holds it open.
///
/// The Agent is converged first, so the platform-owned bridge from the guest
/// port to the image's display socket exists before anything dials it.
async fn vnc(
    home: &ControlPlaneHome,
    client: &Client,
    resource: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    options: VncOptions,
) -> CommandResult<ExitCode> {
    let VncOptions { port, web, open } = options;
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    // Refuses early, with the remedy, when the Agent declares no VNC access.
    client.vnc_access(&agent).await?;
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    // Read again now the Agent is Ready: which ports its image offers is something a
    // reconciliation pass observes, so before converging the browser viewer's port is unknown
    // rather than absent.
    let access = client.vnc_access(&agent).await?;
    let guest_port = if web {
        access.web_guest_port.ok_or_else(|| {
            Error::Invalid(format!(
                "the image of agent {agent:?} serves no browser viewer; use `agentctl vnc {agent}` with a VNC client"
            ))
        })?
    } else {
        access.guest_port
    };
    let spec = forward::ForwardSpec {
        address: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        // A free port by default: 5900 and 6080 are often taken locally, by a screen-sharing
        // server or by another `agentctl vnc`, and the address is printed either way.
        local_port: port.unwrap_or(0),
        guest_port,
    };
    let forward = forward::PortForward::start(home.path().to_path_buf(), target.sandbox.clone(), spec).await?;
    let address = forward.local_address();
    // The image decides what its viewer port serves and where the root redirects, so the caller is
    // pointed at the root rather than a path this side would have to keep in step with it.
    let url = launch::forward_url(address, guest_port);
    println!("Desktop of agent {agent:?} is at {url}");
    if web {
        println!("Open that address in a browser; nothing needs installing.");
    } else {
        println!("Open it with any VNC viewer, for example `vncviewer {address}`, or pass --web for a browser.");
    }
    if open {
        open_locally(&url).await;
    }
    hold_forwards(std::slice::from_ref(&forward)).await
}

/// Hands the address to whichever local application handles its scheme.
///
/// Best effort by design: there is no portable VNC viewer, the address is
/// already printed, and a missing handler must not fail the forward.
async fn open_locally(url: &str) {
    match launch::open_url(url).await {
        Ok(()) => println!("Asked {} to open {url}.", launch::opener()),
        Err(error) => eprintln!("{error}"),
    }
}

/// Relays one desktop connection over standard input and output.
///
/// This is the seam for a viewer that dials through a command rather than a
/// port, and for tooling that wants the RFB stream without a listening socket.
async fn vnc_proxy(home: &ControlPlaneHome, client: &Client, resource: String) -> CommandResult<ExitCode> {
    let agent = resolve_execution_agent(client, Some(resource), None, None).await?;
    let access = client.vnc_access(&agent).await?;
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    forward::relay_guest_port(
        home.path(),
        &target.sandbox,
        access.guest_port,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await?;
    // A blocked standard-input read would keep the runtime from shutting down;
    // the relay is finished, so leave immediately.
    std::process::exit(0)
}

async fn vnc_info(
    client: &Client,
    resource: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    output: OutputFormat,
) -> CommandResult<()> {
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
    let access = client.vnc_access(&agent).await?;
    match output {
        OutputFormat::Json => print_json(&access)?,
        OutputFormat::Table => {
            for line in format::vnc_access_lines(&access) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

/// Resolves a [`SessionTarget`] into the owning Agent and Session name.
async fn session_target(client: &Client, target: SessionTarget) -> CommandResult<(String, SessionName)> {
    let (resource, name) = resource_reference(&target.resource, target.name)?;
    if resource != Resource::Session {
        return Err(Error::Invalid("this command requires a session resource".into()).into());
    }
    let session = SessionName::new(require_name(name, "Session")?)?;
    let agent = resolve_agent_name(client, target.agent, target.variant).await?;
    Ok((agent, session))
}

/// Fails with what is missing when `agent` has no session `name`, as agentd's own answer names
/// neither.
async fn require_session(client: &Client, agent: &str, name: &SessionName) -> CommandResult<()> {
    let sessions = client.list_sessions(Some(agent)).await?;
    if sessions.iter().any(|session| &session.name == name) {
        return Ok(());
    }
    Err(CommandError::Message(format!(
        "{agent} has no session {name}; `agentctl get sessions --agent {agent}` lists them"
    )))
}

async fn create_session(
    home: &ControlPlaneHome,
    client: &Client,
    target: SessionTarget,
    selection: SessionSelection,
    input: PromptInput,
    timeout: Duration,
) -> CommandResult<()> {
    let resource = target.resource.clone();
    let request = selection.request(read_prompt_arg(input)?);
    let deadline = tokio::time::Instant::now() + timeout;
    let timed_out = || {
        CommandError::Message(format!(
            "timed out creating {resource}; agent resolution or provisioning did not finish; provisioning may continue"
        ))
    };
    let (agent, session) = tokio::time::timeout_at(deadline, async {
        ensure_daemon(home, client).await?;
        session_target(client, target).await
    })
    .await
    .map_err(|_| timed_out())??;
    let wait = progress::Wait::start();
    wait.until(
        client,
        &agent,
        tokio::time::timeout_at(
            deadline,
            client.ensure_session(&agent, session.clone(), request, WaitPolicy::UntilConverged),
        ),
    )
    .await
    .map_err(|_| timed_out())??;
    println!("session/{agent}/{session} ready");
    Ok(())
}

async fn prompt_session(
    home: &ControlPlaneHome,
    client: &Client,
    target: SessionTarget,
    input: PromptInput,
    completion: CompletionOptions,
) -> CommandResult<()> {
    ensure_daemon(home, client).await?;
    let (agent, session) = session_target(client, target).await?;
    let prompt = read_prompt_arg(input)?.ok_or_else(|| Error::Invalid("a prompt is required".into()))?;
    client
        .prompt_session(
            &agent,
            session.clone(),
            prompt,
            completion.wait,
            completion.wait.then_some(completion.timeout),
        )
        .await?;
    println!("session/{agent}/{session} prompted");
    Ok(())
}

async fn set_archived(client: &Client, target: SessionTarget, archived: bool) -> CommandResult<()> {
    let (agent, name) = session_target(client, target).await?;
    require_session(client, &agent, &name).await?;
    let session = client.set_session_archived(&agent, name.clone(), archived).await?;
    if !archived {
        println!("session/{agent}/{name} unarchived");
    } else if session.status.state == agent::sessions::State::Archived {
        println!("session/{agent}/{name} archived");
    } else {
        println!("session/{agent}/{name} archived; its harness stops once it is idle");
    }
    Ok(())
}

async fn turns(client: &Client, target: SessionTarget, last: Option<usize>) -> CommandResult<()> {
    let (agent, session) = session_target(client, target).await?;
    require_session(client, &agent, &session).await?;
    print_turns(&client.session_turns(&agent, session, last).await?);
    Ok(())
}

/// Resolves the prompt from --prompt, --file, or piped standard input.
///
/// With neither flag and an interactive terminal there is no prompt.
fn read_prompt_arg(input: PromptInput) -> CommandResult<Option<String>> {
    if let Some(prompt) = input.prompt {
        return Ok(Some(prompt));
    }
    if let Some(file) = input.file {
        return Ok(Some(std::fs::read_to_string(&file).map_err(Error::from)?));
    }
    if !std::io::stdin().is_terminal() {
        let mut buffer = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer).map_err(Error::from)?;
        if !buffer.is_empty() {
            return Ok(Some(buffer));
        }
    }
    Ok(None)
}

fn print_turns(turns: &[agent::sessions::Turn]) {
    if turns.is_empty() {
        eprintln!("No turns yet.");
    }
    for line in format::turn_lines(turns) {
        println!("{line}");
    }
}

async fn resolve_execution_agent(
    client: &Client,
    resource: Option<String>,
    explicit: Option<String>,
    variant: Option<AgentVariantName>,
) -> CommandResult<String> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    let Some(resource) = resource else {
        return resolve_agent_name(client, None, variant).await;
    };
    if !resource.contains('/') {
        return Ok(resource);
    }
    let (kind, name) = resource_reference(&resource, None)?;
    if kind != Resource::Agent {
        return Err(Error::Invalid("this command requires an agent resource".into()).into());
    }
    require_name(name, "Agent").map_err(CommandError::from)
}

async fn stream_execution(
    mut execution: ::sandbox::execution::StartedExecution,
) -> Result<::sandbox::execution::ExitStatus, Error> {
    let id = execution.id.clone();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    while let Some(event) = execution.events.next().await {
        match event? {
            ExecutionEvent::Started { .. } => {}
            ExecutionEvent::Stdout(bytes) => stdout.write_all(&bytes).await?,
            ExecutionEvent::Stderr(bytes) => stderr.write_all(&bytes).await?,
            ExecutionEvent::Exited(status) => {
                stdout.flush().await?;
                stderr.flush().await?;
                return Ok(status);
            }
            ExecutionEvent::Failed { message } => {
                return Err(::sandbox::Error::ExecutionFailed { id, message }.into());
            }
            _ => {
                return Err(Error::Sandbox(::sandbox::Error::Backend(
                    "unsupported Execution event".into(),
                )));
            }
        }
    }
    Err(::sandbox::Error::ExecutionStreamEnded { id }.into())
}

fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

async fn describe(client: &Client, resource: &str, name: Option<String>, output: OutputFormat) -> CommandResult<()> {
    let (resource, name) = resource_reference(resource, name)?;
    if resource != Resource::Agent {
        return Err(Error::Invalid("describe currently supports only agent resources".into()).into());
    }
    let agent = client.get(&require_name(name, "Agent")?).await?;
    match output {
        OutputFormat::Json => print_json(&agent)?,
        OutputFormat::Table => print_agent_description(&agent),
    }
    Ok(())
}

async fn wait(
    client: &Client,
    condition: &str,
    timeout: Duration,
    resource: &str,
    name: Option<String>,
) -> CommandResult<()> {
    let (resource, name) = resource_reference(resource, name)?;
    if resource != Resource::Agent {
        return Err(Error::Invalid("wait currently supports only agent resources".into()).into());
    }
    if condition != "condition=Ready" {
        return Err(Error::Invalid("only --for=condition=Ready is supported".into()).into());
    }
    let name = require_name(name, "Agent")?;
    // A stopped Agent is never Ready, also while its stop is still in progress.
    if client.get(&name).await?.spec.is_stopped()
        || wait_until_converged(client, &name, timeout).await?.spec.is_stopped()
    {
        return Err(Error::Stopped(name).into());
    }
    println!("agent/{name} condition met");
    Ok(())
}

/// Resolves `agent/NAME` or `agent NAME` for a verb that acts only on Agents.
fn agent_reference(resource: &str, name: Option<String>) -> Result<String, Error> {
    let (resource, name) = resource_reference(resource, name)?;
    if resource != Resource::Agent {
        return Err(Error::Invalid("this command requires an agent resource".into()));
    }
    require_name(name, "Agent")
}

fn resource_reference(resource: &str, name: Option<String>) -> Result<(Resource, Option<String>), Error> {
    let (kind, embedded_name) = resource.split_once('/').map_or((resource, None), |(kind, name)| {
        (kind, (!name.is_empty()).then(|| name.to_owned()))
    });
    if resource.matches('/').count() > 1 || (resource.contains('/') && embedded_name.is_none()) {
        return Err(Error::Invalid("resource references must use TYPE/NAME".into()));
    }
    if embedded_name.is_some() && name.is_some() {
        return Err(Error::Invalid("resource name was supplied twice".into()));
    }
    let resource = match kind.to_ascii_lowercase().as_str() {
        "agent" | "agents" | "ag" => Resource::Agent,
        "session" | "sessions" => Resource::Session,
        _ => return Err(Error::Invalid(format!("unknown resource type {kind:?}"))),
    };
    Ok((resource, embedded_name.or(name)))
}

fn require_name(name: Option<String>, resource: &str) -> Result<String, Error> {
    name.ok_or_else(|| Error::Invalid(format!("{resource} name is required")))
}

fn reject_session_scope(
    agent: Option<&str>,
    variant: Option<&AgentVariantName>,
    all_agents: bool,
) -> Result<(), Error> {
    if agent.is_some() || variant.is_some() || all_agents {
        Err(Error::Invalid(
            "--agent, --variant, and --all-agents apply only to session resources".into(),
        ))
    } else {
        Ok(())
    }
}

async fn resolve_agent_name(
    client: &Client,
    explicit: Option<String>,
    variant: Option<AgentVariantName>,
) -> CommandResult<String> {
    if let Some(agent) = explicit {
        return Ok(agent);
    }
    let directory = std::env::current_dir().map_err(Error::from)?;
    match client.resolve_agent_variant(directory, variant).await {
        Ok(agent) => Ok(agent.metadata.name),
        Err(error) => Err(inference_error(error)),
    }
}

fn inference_error(error: Error) -> CommandError {
    match error {
        Error::Rpc(error) if error.is_not_found() => {
            CommandError::Message("no agent here; `agentctl init` sets one up, or pass --agent NAME".into())
        }
        Error::Rpc(error) => CommandError::Message(error.message),
        error => error.into(),
    }
}

/// Follows an Agent's convergence with live progress until it has its desired
/// run state, Ready or stopped, and returns it then; or until a terminal
/// error, the timeout, or Ctrl-C.
async fn wait_until_converged(client: &Client, name: &str, timeout: Duration) -> CommandResult<Agent> {
    follow_until_converged(progress::Wait::start(), client, name, timeout).await
}

/// [`wait_until_converged`] for people new to agents, with [`progress::Wait::simple`].
async fn wait_until_converged_simply(client: &Client, name: &str, timeout: Duration) -> CommandResult<Agent> {
    follow_until_converged(progress::Wait::simple(), client, name, timeout).await
}

async fn follow_until_converged(
    wait: progress::Wait,
    client: &Client,
    name: &str,
    timeout: Duration,
) -> CommandResult<Agent> {
    let waited = wait
        .until(client, name, tokio::time::timeout(timeout, client.converge(name)))
        .await;
    let Ok(converged) = waited else {
        return Err(CommandError::Message(match client.get(name).await.ok() {
            Some(agent) if agent.spec.is_stopped() => {
                format!("timed out waiting for agent {name:?} to stop; agentd keeps stopping it")
            }
            agent => wait_timeout_message(name, agent.as_ref().and_then(|agent| agent.status.ready_condition())),
        }));
    };
    converged.map_err(CommandError::from)
}

/// Stops one agent, or every one not stopped yet, and waits until they are stopped.
async fn stop(client: &Client, target: StopTarget) -> CommandResult<()> {
    // One named agent fails as any command on it does, with agentd's own error.
    if let Some(resource) = &target.resource {
        let name = agent_reference(resource, target.name.clone())?;
        if target.now {
            client.stop_now(&name).await?;
        } else {
            client.set_run_state(&name, RunState::Stopped).await?;
        }
        let converged = wait_until_converged(client, &name, target.timeout).await?;
        return stopped_or_restarted(&name, &converged).map_err(CommandError::Message);
    }
    let names = {
        let Some(running) = agents_to_stop(client, &target).await? else {
            return Ok(());
        };
        running
    };
    // Every stop is recorded before any is waited for, so agentd stops them side by side, and
    // one that cannot be recorded leaves the others to stop.
    let mut failed = 0;
    let mut recorded = Vec::new();
    for name in names {
        let stopping = if target.now {
            client.stop_now(&name).await
        } else {
            client.set_run_state(&name, RunState::Stopped).await
        };
        match stopping {
            Ok(_) => recorded.push(name),
            Err(error) => {
                failed += 1;
                eprintln!("agent/{name} could not be stopped: {error}");
            }
        }
    }
    if failed == 0
        && let [name] = recorded.as_slice()
    {
        let converged = wait_until_converged(client, name, target.timeout).await?;
        return stopped_or_restarted(name, &converged).map_err(CommandError::Message);
    }
    let deadline = tokio::time::Instant::now() + target.timeout;
    for name in &recorded {
        let outcome = match wait_saying_why(client, name, deadline).await {
            Some(Ok(converged)) => stopped_or_restarted(name, &converged),
            Some(Err(error)) => Err(format!("agent/{name} did not stop: {error}")),
            None => Err(format!("agent/{name} did not stop in time; agentd keeps stopping it")),
        };
        if let Err(message) = outcome {
            failed += 1;
            eprintln!("{message}");
        }
    }
    let total = recorded.len() + failed;
    if failed > 0 {
        return Err(CommandError::Message(format!(
            "{failed} of {total} agents did not stop; `agentctl get agents` shows them"
        )));
    }
    println!("Every agent is stopped. `agentctl start agent/NAME` starts one again.");
    Ok(())
}

/// How often a wait for several agents says what the one it waits for is doing.
const STOP_PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

/// Waits until `name` converges or `deadline` passes, saying on standard error every few seconds
/// what it is doing meanwhile; `None` when the deadline passed.
async fn wait_saying_why(client: &Client, name: &str, deadline: tokio::time::Instant) -> Option<Result<Agent, Error>> {
    let converged = tokio::time::timeout_at(deadline, client.converge(name));
    tokio::pin!(converged);
    let mut progress = tokio::time::interval_at(
        tokio::time::Instant::now() + STOP_PROGRESS_INTERVAL,
        STOP_PROGRESS_INTERVAL,
    );
    loop {
        tokio::select! {
            outcome = &mut converged => return outcome.ok(),
            _ = progress.tick() => {
                let doing = match client.get(name).await {
                    Ok(agent) if agent.status.ready_condition().is_some_and(|ready| ready.reason == agent::Condition::REASON_STOPPING) => "stopping",
                    Ok(_) => "waiting for agentd to finish setting it up",
                    Err(_) => "waiting for agentd",
                };
                eprintln!("agent/{name}: {doing}");
            }
        }
    }
}

/// Says that `name` stopped, or fails when it was started again before its stop finished.
fn stopped_or_restarted(name: &str, converged: &Agent) -> Result<(), String> {
    if converged.spec.is_stopped() {
        println!("agent/{name} stopped");
        return Ok(());
    }
    Err(format!("agent/{name} was started again before it stopped"))
}

/// Every agent not stopped yet, including one still stopping, so a kill can follow a graceful
/// stop under way; after asking at a terminal unless `--all` was passed. `None` when there is
/// none or the person declined.
async fn agents_to_stop(client: &Client, target: &StopTarget) -> CommandResult<Option<Vec<String>>> {
    let agents: Vec<Agent> = client
        .list_agents()
        .await?
        .into_iter()
        .filter(|agent| {
            agent.metadata.deletion_timestamp.is_none() && !(agent.spec.is_stopped() && agent.status.is_stopped())
        })
        .collect();
    if agents.is_empty() {
        println!("No agent is running.");
        return Ok(None);
    }
    let names: Vec<String> = agents.iter().map(|agent| agent.metadata.name.clone()).collect();
    if target.all {
        return Ok(Some(names));
    }
    if !prompt::interactive() {
        return Err(CommandError::Message(
            "name the agent to stop, or pass --all to stop every agent".into(),
        ));
    }
    let listed: Vec<String> = agents
        .iter()
        .map(|agent| {
            if agent.spec.is_stopped() {
                format!("{} (stopping)", agent.metadata.name)
            } else {
                agent.metadata.name.clone()
            }
        })
        .collect();
    let (running_line, question) = match agents.len() {
        1 => (format!("1 agent is not stopped: {}.", listed.join(", ")), "Stop it"),
        count => (
            format!("{count} agents are not stopped: {}.", listed.join(", ")),
            "Stop them all",
        ),
    };
    let question = if target.now {
        format!("{question} at once, without letting them shut down?")
    } else {
        format!("{question}?")
    };
    Ok(prompt::confirm_lines(&[&running_line, &question])?.then_some(names))
}

/// Stops or starts an Agent and waits until it is stopped or Ready.
async fn set_run_state(client: &Client, target: RunStateTarget, state: RunState) -> CommandResult<()> {
    let name = agent_reference(&target.resource, target.name)?;
    client.set_run_state(&name, state).await?;
    let converged = wait_until_converged(client, &name, target.timeout).await?;
    match (state, converged.spec.run_state()) {
        (RunState::Stopped, RunState::Stopped) => println!("agent/{name} stopped"),
        (RunState::Running, RunState::Running) => println!("agent/{name} started"),
        (_, current) => {
            return Err(CommandError::Message(format!(
                "Agent {name:?} was set to {current:?} again before it finished"
            )));
        }
    }
    Ok(())
}

fn wait_timeout_message(name: &str, ready: Option<&agent::Condition>) -> String {
    let Some(ready) = ready else {
        return format!("timed out waiting for agent {name:?} to become Ready; no Ready condition was reported");
    };
    format!(
        "timed out waiting for agent {name:?} to become Ready: {}",
        ready.summary()
    )
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 60 * 60),
        _ => return Err("duration must end in s, m, or h".into()),
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "duration must contain a positive whole number".to_string())?;
    if number == 0 {
        return Err("duration must be greater than zero".into());
    }
    Ok(Duration::from_secs(number.saturating_mul(multiplier)))
}

fn parse_harness(value: &str) -> Result<agent::Harness, String> {
    value.parse().map_err(|error: Error| error.to_string())
}

fn parse_model(value: &str) -> Result<agent::Model, String> {
    value.parse().map_err(|error: Error| error.to_string())
}

fn parse_effort(value: &str) -> Result<agent::Effort, String> {
    value.parse().map_err(|error: Error| error.to_string())
}

fn parse_variant_name(value: &str) -> Result<AgentVariantName, String> {
    value.parse().map_err(|error: Error| error.to_string())
}

fn print_agents(agents: &[Agent]) {
    let rows = agents
        .iter()
        .map(|agent| {
            let ready = agent.status.ready_condition();
            let ready_value = ready.map_or("Unknown", |condition| condition_status(condition.status));
            let status = if agent.metadata.deletion_timestamp.is_some() {
                "Terminating"
            } else {
                ready.map_or("Pending", |condition| condition.reason.as_str())
            };
            let harnesses = format_harnesses(&agent.spec);
            let provider = agent
                .status
                .sandbox
                .as_ref()
                .map_or("-", |assignment| assignment.provider().as_str());
            vec![
                agent.metadata.name.clone(),
                ready_value.into(),
                status.into(),
                harnesses,
                provider.into(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["NAME", "READY", "STATUS", "HARNESSES", "PROVIDER"], &rows);
}

fn print_agent_description(agent: &Agent) {
    for line in format::describe_agent_lines(agent) {
        println!("{line}");
    }
}

fn print_sessions(sessions: &[Session], show_agent: bool) {
    let rows = sessions
        .iter()
        .map(|session| {
            let mut row = Vec::new();
            if show_agent {
                row.push(session.agent.clone());
            }
            row.extend([
                session.name.as_str().to_owned(),
                session.harness.as_str().into(),
                session.model_selection.model_str().unwrap_or("-").into(),
                session.model_selection.effort_str().unwrap_or("-").into(),
                session_state(session.status.state).into(),
                format_age(session.created_at),
            ]);
            row
        })
        .collect::<Vec<_>>();
    let headers = if show_agent {
        vec!["AGENT", "NAME", "HARNESS", "MODEL", "EFFORT", "STATE", "AGE"]
    } else {
        vec!["NAME", "HARNESS", "MODEL", "EFFORT", "STATE", "AGE"]
    };
    print_table(&headers, &rows);
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        eprintln!("No resources found.");
        return;
    }
    for line in format::table_lines(headers, rows) {
        println!("{line}");
    }
}

async fn ensure_daemon(home: &ControlPlaneHome, client: &Client) -> Result<(), Error> {
    if let Ok(daemon) = client.health().await {
        return daemon.require_compatible();
    }
    let mut daemon = spawn_daemon(home)?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(daemon) = client.health().await {
            return daemon.require_compatible();
        }
        if let Some(status) = daemon.try_wait()? {
            return Err(Error::Daemon(format!(
                "automatic startup exited with {status}; {}",
                daemon_startup_diagnostics(home)
            )));
        }
    }
    Err(Error::Daemon(format!(
        "automatic startup did not become ready within 10 seconds; {}",
        daemon_startup_diagnostics(home)
    )))
}

fn daemon_startup_diagnostics(home: &ControlPlaneHome) -> String {
    let log = home.daemon_log_path();
    let marker = home.pending_session_relaunch_path();
    if marker.exists() {
        format!(
            "see {}; pending post-upgrade session relaunch: {}",
            log.display(),
            marker.display()
        )
    } else {
        format!("see {}", log.display())
    }
}

fn spawn_daemon(home: &ControlPlaneHome) -> Result<Child, Error> {
    home.prepare()?;
    let log = home.open_daemon_log()?;
    let executable = daemon_executable(&std::env::current_exe()?);
    let mut command = ProcessCommand::new(executable);
    command
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    agent::local::process::configure_detached(&mut command);
    agent::local::process::configure_logging(&mut command);
    command.spawn().map_err(Error::from)
}

fn daemon_executable(agentctl: &Path) -> PathBuf {
    agentctl.with_file_name(format!("agentd{}", std::env::consts::EXE_SUFFIX))
}

fn apply_manifest_path(filename: Option<PathBuf>, variant: Option<AgentVariantName>) -> Result<PathBuf, Error> {
    match (filename, variant) {
        (Some(filename), None) => Ok(filename),
        (None, Some(variant)) => Ok(PathBuf::from(variant.filename())),
        (None, None) => Ok(PathBuf::from(manifest::MANIFEST_FILE)),
        (Some(_), Some(_)) => Err(Error::Invalid("--filename and --variant are mutually exclusive".into())),
    }
}

fn read_apply_request(filename: PathBuf, env_file: Option<PathBuf>) -> Result<ApplyRequest, Error> {
    read_apply_request_in(std::env::current_dir, filename, env_file)
}

/// Reads an apply request, resolving relative paths in the working directory `current_dir` returns.
fn read_apply_request_in(
    current_dir: impl Fn() -> std::io::Result<PathBuf>,
    filename: PathBuf,
    env_file: Option<PathBuf>,
) -> Result<ApplyRequest, Error> {
    let filename = absolute(&current_dir, filename)?;
    let env_file = env_file.map(|path| absolute(&current_dir, path)).transpose()?;
    let agent = manifest::resolve(&filename)?.agent;
    let source_directory = filename
        .parent()
        .ok_or_else(|| Error::Invalid("manifest path has no parent directory".into()))?
        .to_path_buf();
    Ok(ApplyRequest {
        source_directory,
        manifest_path: Some(filename),
        env_file,
        create_only: false,
        agent,
    })
}

/// Says whether a harness login is stored, with exit code 1 when it is not.
async fn harness_status(client: &Client, harness: agent::Harness) -> CommandResult<ExitCode> {
    let status = client.auth_status().await?;
    let (signed_in, label, command) = match harness {
        agent::Harness::ClaudeCode => (status.claude_code, "Claude Code", "claude"),
        agent::Harness::Codex => (status.codex, "Codex", "codex"),
        agent::Harness::Pi => {
            println!("Pi: configured by each agent; agentctl stores no login for it");
            return Ok(ExitCode::SUCCESS);
        }
    };
    if signed_in {
        println!("{label}: signed in; agents can use it");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{label}: not signed in; run `agentctl {command} login`");
        Ok(ExitCode::FAILURE)
    }
}

/// A command that would change what `agentctl status` shows, and what it does.
type NextStep = (String, String);

/// Shows agentd, the stored logins and the current directory's agent at a glance, followed by
/// the commands that would change what it shows.
async fn status(home: &ControlPlaneHome, client: &Client) -> CommandResult<()> {
    let daemon = client.health().await?;
    let auth = client.auth_status().await?;
    let mut rows = vec![(
        "agentd",
        format!(
            "running, version {}",
            daemon
                .build_version
                .as_deref()
                .unwrap_or("unknown")
                .trim_start_matches('v')
        ),
    )];
    let mut next: Vec<NextStep> = Vec::new();
    for (label, signed_in) in [("Claude Code", auth.claude_code), ("Codex", auth.codex)] {
        rows.push((label, if signed_in { "signed in" } else { "not signed in" }.to_owned()));
    }
    // Agents need one harness; the other is a choice, not a step.
    if !auth.any_harness() {
        next.push((
            "agentctl claude login".to_owned(),
            "Sign in to Claude Code, or to Codex with `agentctl codex login`".to_owned(),
        ));
    }
    for (index, line) in github_lines(&auth.github, &mut next).into_iter().enumerate() {
        rows.push((if index == 0 { "GitHub" } else { "" }, line));
    }
    let instructions = home.instructions_path();
    rows.push((
        "Instructions",
        match std::fs::read_to_string(&instructions) {
            Ok(text) if !text.trim().is_empty() => {
                let lines = text.lines().count();
                format!(
                    "{lines} line{} in {}",
                    if lines == 1 { "" } else { "s" },
                    format::home_relative(&instructions)
                )
            }
            _ => "none".to_owned(),
        },
    ));
    let mut here = Vec::new();
    rows.push(("This directory", directory_line(home, client, &mut here).await?));
    let agents = client.list_agents().await?;
    let sessions = client.list_sessions(None).await?;
    let (line, waiting) = machine_line(&agents, &sessions);
    rows.push(("Globally", line));
    if waiting > 0 {
        here.push((
            "agentctl tui".to_owned(),
            format!(
                "Answer the session{} that need you",
                if waiting == 1 { "" } else { "s" }
            ),
        ));
    }
    // `init` signs in as it goes, so without an agent here it is the one step; otherwise what
    // concerns this directory's agent comes first.
    if !here.iter().any(|(command, _)| command == "agentctl init") {
        here.append(&mut next);
    }
    next = here;
    for (label, value) in rows {
        println!("{label:<15} {value}");
    }
    next.dedup_by(|later, earlier| later.0 == earlier.0);
    if !next.is_empty() {
        let width = next.iter().map(|(command, _)| command.len()).max().unwrap_or(0);
        println!("\nNext steps");
        for (command, what) in next {
            println!("  {command:<width$}  {what}");
        }
    }
    Ok(())
}

/// Every agent agentd runs at a glance: how many run, and how many sessions wait for an
/// answer, with that count.
fn machine_line(agents: &[Agent], sessions: &[agent::sessions::Session]) -> (String, usize) {
    let agents: Vec<&Agent> = agents
        .iter()
        .filter(|agent| agent.metadata.deletion_timestamp.is_none())
        .collect();
    if agents.is_empty() {
        return ("no agents yet".to_owned(), 0);
    }
    let running = agents
        .iter()
        .filter(|agent| !agent.spec.is_stopped() && agent.status.is_ready())
        .count();
    let stopped = agents.iter().filter(|agent| agent.spec.is_stopped()).count();
    let other = agents.len() - running - stopped;
    let count_of =
        |state: agent::sessions::State| sessions.iter().filter(|session| session.status.state == state).count();
    let waiting = count_of(agent::sessions::State::WaitingForInput);
    let working = count_of(agent::sessions::State::Working);
    let mut parts = Vec::new();
    let states = [running, other, stopped];
    for (count, word) in [
        (running, "running"),
        (other, "starting or failing"),
        (stopped, "stopped"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {word}"));
        }
    }
    // One state for every agent reads as that state, "all" in front when there are several.
    let summary = match (agents.len(), states.iter().filter(|count| **count > 0).count()) {
        (1, _) => parts.join(", ").trim_start_matches("1 ").to_owned(),
        (_, 1) => format!("all {}", parts.join(", ").split_once(' ').map_or("", |(_, word)| word)),
        _ => parts.join(", "),
    };
    let count = agents.len();
    let mut line = vec![format!("{count} agent{}: {summary}", if count == 1 { "" } else { "s" })];
    if waiting > 0 {
        line.push(format!(
            "{waiting} session{} need{} you",
            if waiting == 1 { "" } else { "s" },
            if waiting == 1 { "s" } else { "" }
        ));
    }
    if working > 0 {
        line.push(format!("{working} working"));
    }
    let line = line.join("; ");
    (line, waiting)
}

/// Each stored GitHub token as the account, where agents use it and until when.
fn github_lines(tokens: &[agent::github::GithubToken], next: &mut Vec<NextStep>) -> Vec<String> {
    if tokens.is_empty() {
        next.push((
            "agentctl github login".to_owned(),
            "Let agents work on GitHub".to_owned(),
        ));
        return vec!["no account".to_owned()];
    }
    let now = time::OffsetDateTime::now_utc();
    tokens
        .iter()
        .map(|token| {
            let place = match &token.owner {
                Some(owner) => format!("in {owner}"),
                None if tokens.len() == 1 => "in every organization".to_owned(),
                None => "in every other organization".to_owned(),
            };
            let expiry = match token.expires_at {
                Some(expires_at) if token.expired(now) => {
                    next.push((
                        "agentctl github login".to_owned(),
                        format!("Replace the expired token for {}", token.account.login),
                    ));
                    format!("expired {}", expires_at.date())
                }
                Some(expires_at) => format!("until {}", expires_at.date()),
                None if token.kind.single_owner() => "expiry unknown".to_owned(),
                None => "no expiry".to_owned(),
            };
            format!("{} {place}, {expiry}", token.account.login)
        })
        .collect()
}

/// The current directory's agent: its state, sessions and whether its tools changed.
async fn directory_line(home: &ControlPlaneHome, client: &Client, next: &mut Vec<NextStep>) -> CommandResult<String> {
    let directory = std::env::current_dir().map_err(Error::from)?;
    let agent = match client.resolve_agent_variant(directory, None).await {
        Ok(agent) => agent,
        Err(Error::Rpc(error)) if error.is_not_found() => {
            let directory = std::env::current_dir().map_err(Error::from)?;
            let what = if directory.join(manifest::MANIFEST_FILE).is_file() {
                "Create the agent from agent.yaml"
            } else if tui::working_tree_manifests(&directory).is_empty() {
                "Set up an agent here; it signs you in as needed"
            } else {
                "Create one of the agents set up here, or a new one"
            };
            next.push(("agentctl init".to_owned(), what.to_owned()));
            return Ok("no agent".to_owned());
        }
        Err(Error::Rpc(error)) if error.is_invalid_params() => {
            next.push((
                "agentctl tui".to_owned(),
                "Choose one of this directory's agents".to_owned(),
            ));
            return Ok("several agents".to_owned());
        }
        Err(error) => return Err(inference_error(error)),
    };
    let name = agent.metadata.name.clone();
    if let Some(problem) = format::blocking_problem(&agent) {
        return Ok(format!("{name} cannot start: {problem}"));
    }
    let state = match agent.status.ready_condition() {
        Some(condition) if condition.status == agent::ConditionStatus::True => "running".to_owned(),
        Some(condition) => lowercase_words(&condition.reason),
        None => "starting".to_owned(),
    };
    if agent.spec.is_stopped() {
        next.push((format!("agentctl start agent/{name}"), format!("Start {name} again")));
    }
    let sessions = client.list_sessions(Some(&name)).await?.len();
    let outdated = agent
        .status
        .provenance
        .as_ref()
        .and_then(|provenance| provenance.manifest_path.as_deref())
        .and_then(Path::parent)
        .and_then(|folder| tools::outdated(home, &name, folder))
        == Some(true);
    if outdated {
        next.push((
            "agentctl tools".to_owned(),
            format!("Recreate {name} with its changed tools"),
        ));
    }
    Ok(format!(
        "{name}: {state}, {sessions} session{}{}",
        if sessions == 1 { "" } else { "s" },
        if outdated { ", tools changed" } else { "" }
    ))
}

/// A condition reason such as `ImageBuildFailed` as words: `image build failed`.
fn lowercase_words(reason: &str) -> String {
    let mut words = String::new();
    for (index, character) in reason.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            words.push(' ');
        }
        words.extend(character.to_lowercase());
    }
    words
}

/// Opens the host-wide instructions in an editor, creating the file first when it is missing.
fn edit_instructions(home: &ControlPlaneHome) -> CommandResult<()> {
    let path = home.instructions_path();
    if !path.exists() {
        std::fs::write(&path, "").map_err(Error::from)?;
    }
    prompt::edit_files(&[&path])?;
    println!(
        "Agents receive {} ahead of their own instructions on their next pass.",
        path.display()
    );
    Ok(())
}

/// Signs in to a harness on the host, or stores a credential read from standard input.
async fn harness_login(
    home: &ControlPlaneHome,
    client: &Client,
    harness: agent::Harness,
    from_stdin: bool,
) -> CommandResult<()> {
    let credential = match (from_stdin, harness) {
        (true, agent::Harness::ClaudeCode) => read_token_from_stdin()?,
        (true, agent::Harness::Codex) => read_stdin_to_end()?,
        (true, agent::Harness::Pi) => {
            return Err(
                Error::Invalid("Pi is configured by each agent; agentctl stores no login for it".into()).into(),
            );
        }
        (false, harness) => agent::harness::acquire_host_credential(harness, home.path())?,
    };
    let imported = client.auth_login(harness, credential.to_string(), from_stdin).await?;
    println!("{} authentication stored", imported.provider);
    Ok(())
}

/// Attaches to a new Session with the first free name, `s1`, `s2` and so on.
async fn attach_new_session(home: &ControlPlaneHome, client: &Client, agent: &str) -> CommandResult<()> {
    let name = new_session_name(client, agent).await?;
    eprintln!("Attaching to session/{name}; detach with Ctrl-b d, and run `agentctl attach` here to return.");
    attach(
        home,
        client,
        Some("session"),
        Some(name),
        Some(agent.to_owned()),
        None,
        SessionSelection::default(),
    )
    .await
}

/// The first free session name of `agent`: `s1`, `s2` and so on.
async fn new_session_name(client: &Client, agent: &str) -> CommandResult<String> {
    let taken: Vec<String> = client
        .list_sessions(Some(agent))
        .await?
        .into_iter()
        .map(|session| session.name.to_string())
        .collect();
    // One of the first `taken.len() + 1` names is always free.
    Ok((1..=taken.len() + 1)
        .map(|number| format!("s{number}"))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| "s1".into()))
}

/// The session `agentctl attach` opens without a name: the agent's only one, a new one when it
/// has none, or the one chosen at a terminal.
async fn choose_session(client: &Client, agent: &str) -> CommandResult<String> {
    let sessions: Vec<String> = client
        .list_sessions(Some(agent))
        .await?
        .into_iter()
        .filter(|session| !session.is_archived())
        .map(|session| session.name.to_string())
        .collect();
    match sessions.as_slice() {
        [] => {
            let name = new_session_name(client, agent).await?;
            eprintln!("Starting session/{name}; detach with Ctrl-b d, and run `agentctl attach` here to return.");
            Ok(name)
        }
        [only] => Ok(only.clone()),
        _ if !prompt::interactive() => Err(CommandError::Message(format!(
            "{agent} has several sessions; name one: agentctl attach session/NAME ({})",
            sessions.join(", ")
        ))),
        _ => {
            let mut options = sessions.clone();
            options.push("A new session".to_owned());
            let chosen = prompt::choose(&format!("Which session of {agent}?"), &options, Some(0))?.unwrap_or(0);
            match sessions.get(chosen) {
                Some(name) => Ok(name.clone()),
                None => new_session_name(client, agent).await,
            }
        }
    }
}

/// Offers to start `agent` when it is stopped, at a terminal; elsewhere, or when declined, says
/// how to start it.
async fn offer_start_if_stopped(client: &Client, agent: &str) -> CommandResult<()> {
    if !client.get(agent).await?.spec.is_stopped() {
        return Ok(());
    }
    if prompt::interactive() && prompt::confirm(&format!("The agent {agent} is stopped. Start it?"))? {
        client.set_run_state(agent, RunState::Running).await?;
        wait_until_converged_simply(client, agent, Duration::from_mins(10)).await?;
        return Ok(());
    }
    Err(Error::Stopped(agent.to_owned()).into())
}

fn read_token_from_stdin() -> Result<zeroize::Zeroizing<String>, Error> {
    let mut line = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| Error::Invalid(format!("could not read the token from standard input: {error}")))?;
    let token = zeroize::Zeroizing::new(line.trim().to_owned());
    if token.is_empty() {
        return Err(Error::Invalid("no token was provided on standard input".into()));
    }
    Ok(token)
}

fn read_stdin_to_end() -> Result<zeroize::Zeroizing<String>, Error> {
    use std::io::Read as _;

    let mut text = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| Error::Invalid(format!("could not read the credential from standard input: {error}")))?;
    if text.trim().is_empty() {
        return Err(Error::Invalid("no credential was provided on standard input".into()));
    }
    Ok(text)
}

fn absolute(current_dir: impl Fn() -> std::io::Result<PathBuf>, path: PathBuf) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn kill_is_stop_now() {
        let stop = |arguments: &[&str]| {
            let parsed = Arguments::try_parse_from(arguments).expect("arguments");
            match parsed.command.expect("a command") {
                Command::Kill { target } => StopTarget::from(target),
                Command::Stop { target } => target,
                _ => panic!("expected stop or kill"),
            }
        };
        for (kill, stop_now) in [
            (&["agentctl", "kill"][..], &["agentctl", "stop", "--now"][..]),
            (&["agentctl", "kill", "--all"], &["agentctl", "stop", "--all", "--now"]),
            (
                &["agentctl", "kill", "agent/worker"],
                &["agentctl", "stop", "agent/worker", "--now"],
            ),
        ] {
            let (killed, stopped) = (stop(kill), stop(stop_now));
            assert_eq!(
                (killed.resource, killed.name, killed.all, killed.now, killed.timeout),
                (
                    stopped.resource,
                    stopped.name,
                    stopped.all,
                    stopped.now,
                    stopped.timeout
                ),
                "{kill:?}"
            );
        }
        assert!(
            Arguments::try_parse_from(["agentctl", "kill", "--now"]).is_err(),
            "kill is always at once"
        );
        assert!(Arguments::try_parse_from(["agentctl", "kill", "agent/worker", "--all"]).is_err());
    }

    struct StalledConnector {
        healthy: bool,
    }

    struct PreviewOneConnector;

    impl agent::control_api::Connector for PreviewOneConnector {
        fn connect(&self) -> sandbox::LocalFuture<'_, Result<Box<dyn agent::control_api::Connection>, Error>> {
            Box::pin(async {
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                let (client, server) = tokio::io::duplex(4096);
                tokio::task::spawn_local(async move {
                    let mut server = tokio::io::BufReader::new(server);
                    let mut request = String::new();
                    server.read_line(&mut request).await.expect("request");
                    let request: serde_json::Value = serde_json::from_str(&request).expect("RPC");
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": {"protocolVersion": "v1"}
                    });
                    server
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .expect("response");
                });
                Ok(Box::new(client) as Box<dyn agent::control_api::Connection>)
            })
        }
    }

    #[tokio::test(flavor = "local")]
    async fn incompatible_daemon_is_reported_without_starting_another() {
        let directory = tempfile::TempDir::new().expect("temporary home");
        let home = ControlPlaneHome::resolve(Some(directory.path())).expect("home");
        let client = Client::new(std::rc::Rc::new(PreviewOneConnector));
        let error = ensure_daemon(&home, &client)
            .await
            .expect_err("preview daemon is incompatible");
        assert!(error.to_string().contains("protocol Some(\"v1\")"));
        assert!(!home.daemon_log_path().exists(), "no second daemon was spawned");
    }

    #[test]
    fn startup_diagnostics_identify_a_pending_session_relaunch() {
        let directory = tempfile::TempDir::new().expect("temporary home");
        let home = ControlPlaneHome::resolve(Some(directory.path())).expect("home");
        std::fs::write(home.pending_session_relaunch_path(), "pending").expect("marker");

        let diagnostic = daemon_startup_diagnostics(&home);

        assert!(diagnostic.contains(&home.daemon_log_path().display().to_string()));
        assert!(diagnostic.contains(&home.pending_session_relaunch_path().display().to_string()));
    }

    impl agent::control_api::Connector for StalledConnector {
        fn connect(&self) -> sandbox::LocalFuture<'_, Result<Box<dyn agent::control_api::Connection>, Error>> {
            Box::pin(async move {
                use tokio::io::AsyncBufReadExt as _;
                if !self.healthy {
                    return std::future::pending().await;
                }
                let (client, server) = tokio::io::duplex(4096);
                tokio::task::spawn_local(async move {
                    let mut server = tokio::io::BufReader::new(server);
                    let mut line = String::new();
                    server.read_line(&mut line).await.expect("request");
                    let request: serde_json::Value = serde_json::from_str(&line).expect("RPC");
                    if request["method"] == "control.v1.health" {
                        let response = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": {
                                "protocolVersion": agent::control_api::PROTOCOL_VERSION,
                                "buildVersion": agent::build_version()
                            }
                        });
                        server
                            .write_all(format!("{response}\n").as_bytes())
                            .await
                            .expect("health response");
                    } else {
                        std::future::pending::<()>().await;
                        drop(server);
                    }
                });
                Ok(Box::new(client) as Box<dyn agent::control_api::Connection>)
            })
        }
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn create_stops_waiting_at_its_deadline() {
        for (owner, healthy) in [(Some("worker"), false), (Some("worker"), true), (None, true)] {
            let client = Client::new(std::rc::Rc::new(StalledConnector { healthy }));
            let directory = tempfile::TempDir::new().expect("temporary home");
            let home = ControlPlaneHome::resolve(Some(directory.path())).expect("home");
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                create_session(
                    &home,
                    &client,
                    SessionTarget {
                        resource: "session/s1".into(),
                        name: None,
                        agent: owner.map(str::to_owned),
                        variant: None,
                    },
                    SessionSelection::default(),
                    PromptInput {
                        prompt: Some("go".into()),
                        file: None,
                    },
                    Duration::from_secs(1),
                ),
            )
            .await
            .expect("the command's own deadline must include agent inference");
            assert!(matches!(result, Err(CommandError::Message(message)) if message.contains("timed out")));
        }
    }

    struct DelayedHealthConnector {
        remaining: std::rc::Rc<std::cell::Cell<Option<Duration>>>,
    }

    impl agent::control_api::Connector for DelayedHealthConnector {
        fn connect(&self) -> sandbox::LocalFuture<'_, Result<Box<dyn agent::control_api::Connection>, Error>> {
            Box::pin(async move {
                use tokio::io::AsyncBufReadExt as _;
                let (client, server) = tokio::io::duplex(4096);
                let remaining = self.remaining.clone();
                tokio::task::spawn_local(async move {
                    let mut server = tokio::io::BufReader::new(server);
                    let mut line = String::new();
                    server.read_line(&mut line).await.expect("request");
                    let request: serde_json::Value = serde_json::from_str(&line).expect("RPC");
                    if request["method"] == "control.v1.health" {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                    } else {
                        assert_eq!(request["method"], "sessions.v1.prompt");
                        remaining.set(Some(
                            serde_json::from_value(request["params"]["timeout"].clone()).expect("timeout"),
                        ));
                    }
                    let result = if request["method"] == "control.v1.health" {
                        serde_json::json!({
                            "protocolVersion": agent::control_api::PROTOCOL_VERSION,
                            "buildVersion": agent::build_version()
                        })
                    } else {
                        serde_json::json!({})
                    };
                    let response = serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result});
                    server
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .expect("response");
                });
                Ok(Box::new(client) as Box<dyn agent::control_api::Connection>)
            })
        }
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn prompt_setup_does_not_consume_the_completion_timeout() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let client = Client::new(std::rc::Rc::new(DelayedHealthConnector {
            remaining: remaining.clone(),
        }));
        let directory = tempfile::TempDir::new().expect("home");
        let home = ControlPlaneHome::resolve(Some(directory.path())).expect("home");
        prompt_session(
            &home,
            &client,
            SessionTarget {
                resource: "session/s1".into(),
                name: None,
                agent: Some("worker".into()),
                variant: None,
            },
            PromptInput {
                prompt: Some("go".into()),
                file: None,
            },
            CompletionOptions {
                wait: true,
                timeout: Duration::from_secs(1),
            },
        )
        .await
        .expect("prompt");
        assert_eq!(remaining.get(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn session_creation_commands_accept_optional_model_and_effort() {
        for verb in ["create", "attach"] {
            let arguments = Arguments::try_parse_from([
                "agentctl",
                verb,
                "session/s1",
                "--model",
                "claude-fable-5",
                "--effort",
                "xhigh",
            ])
            .expect("model and effort parse");
            let (Command::Create { selection, .. } | Command::Attach { selection, .. }) =
                arguments.command.expect("a command")
            else {
                panic!("expected a session creation command");
            };
            let request = selection.request(None);
            assert_eq!(request.model_selection.model_str(), Some("claude-fable-5"));
            assert_eq!(request.model_selection.effort_str(), Some("xhigh"));
            assert_eq!(request.harness, None);

            let omitted = Arguments::try_parse_from(["agentctl", verb, "session/s1"]).expect("omitted selections");
            let (Command::Create { selection, .. } | Command::Attach { selection, .. }) =
                omitted.command.expect("a command")
            else {
                panic!("expected a session creation command");
            };
            assert_eq!(selection.request(None), SessionRequest::default());

            let Err(error) = Arguments::try_parse_from(["agentctl", verb, "session/s1", "--model", ""]) else {
                panic!("an empty model is rejected before reaching the daemon");
            };
            assert!(error.to_string().contains("model must be 1-128"), "{error}");
            assert!(Arguments::try_parse_from(["agentctl", verb, "session/s1", "--effort", "very high"]).is_err());
        }
    }

    #[test]
    fn resource_references_follow_kubectl_shapes_and_aliases() {
        assert_eq!(
            resource_reference("agents", None).expect("Agent collection"),
            (Resource::Agent, None)
        );
        assert_eq!(
            resource_reference("ag/worker", None).expect("Agent reference"),
            (Resource::Agent, Some("worker".into()))
        );
        assert_eq!(
            resource_reference("session", Some("s1".into())).expect("Session reference"),
            (Resource::Session, Some("s1".into()))
        );
        assert!(resource_reference("agent/worker", Some("other".into())).is_err());
        assert!(resource_reference("pods", None).is_err());
    }

    #[test]
    fn agentctl_without_a_command_parses_and_init_is_a_command() {
        let bare = Arguments::try_parse_from(["agentctl", "--home", "/tmp/agent"]).expect("no command");
        assert!(bare.command.is_none());
        let init = Arguments::try_parse_from(["agentctl", "init"]).expect("init");
        assert!(matches!(init.command, Some(Command::Init)));
        let login = Arguments::try_parse_from(["agentctl", "github", "login", "--owner", "Digdir", "--browser"])
            .expect("github login");
        assert!(matches!(
            login.command,
            Some(Command::Github {
                command: GithubCommand::Login {
                    owner: Some(owner),
                    from_stdin: false,
                    from_gh: false,
                    browser: true,
                }
            }) if owner == "digdir"
        ));
        for sources in [["--from-gh", "--from-stdin"], ["--browser", "--from-gh"]] {
            let mut arguments = vec!["agentctl", "github", "login"];
            arguments.extend(sources);
            Arguments::try_parse_from(arguments).err().expect("one token source");
        }
        Arguments::try_parse_from(["agentctl", "github", "login", "--owner", "not/an/owner"])
            .err()
            .expect("an invalid owner");
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "github", "list"])
                .expect("list")
                .command,
            Some(Command::Github {
                command: GithubCommand::List
            })
        ));
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "github", "logout"])
                .expect("logout")
                .command,
            Some(Command::Github {
                command: GithubCommand::Logout {
                    name: None,
                    owner: None,
                    everywhere: false,
                    yes: false
                }
            })
        ));
        Arguments::try_parse_from(["agentctl", "github", "logout", "--owner", "digdir", "--everywhere"])
            .err()
            .expect("an owner or everywhere");
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "github", "reset", "--yes"])
                .expect("reset")
                .command,
            Some(Command::Github {
                command: GithubCommand::Reset { yes: true }
            })
        ));
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "status"])
                .expect("status")
                .command,
            Some(Command::Status)
        ));
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "instructions", "edit"])
                .expect("instructions edit")
                .command,
            Some(Command::Instructions {
                command: InstructionsCommand::Edit
            })
        ));
        assert!(matches!(
            Arguments::try_parse_from(["agentctl", "claude", "status"])
                .expect("claude status")
                .command,
            Some(Command::Claude {
                command: ClaudeCommand::Status
            })
        ));
    }

    #[test]
    fn exec_accepts_kubectl_style_interactive_and_inferred_shapes() {
        let explicit = Arguments::try_parse_from(["agentctl", "exec", "-it", "agent/worker", "--", "bash", "-l"])
            .expect("interactive exec arguments");
        let Command::Exec {
            stdin,
            tty,
            resource,
            agent,
            command,
            ..
        } = explicit.command.expect("a command")
        else {
            panic!("expected exec command");
        };
        assert!(stdin);
        assert!(tty);
        assert_eq!(resource.as_deref(), Some("agent/worker"));
        assert!(agent.is_none());
        assert_eq!(command, ["bash", "-l"]);

        let inferred = Arguments::try_parse_from(["agentctl", "exec", "--", "pwd"]).expect("inferred exec arguments");
        let Command::Exec { resource, command, .. } = inferred.command.expect("a command") else {
            panic!("expected exec command");
        };
        assert!(resource.is_none());
        assert_eq!(command, ["pwd"]);
    }

    #[test]
    fn port_forward_accepts_kubectl_shapes_and_inference() {
        let explicit = Arguments::try_parse_from(["agentctl", "port-forward", "agent/worker", "9090:80", ":5432"])
            .expect("explicit port-forward arguments");
        let Command::PortForward { agent, arguments, .. } = explicit.command.expect("a command") else {
            panic!("expected port-forward command");
        };
        assert!(agent.is_none());
        assert_eq!(
            split_forward_arguments(&arguments),
            (Some("agent/worker".into()), &arguments[1..])
        );

        let inferred =
            Arguments::try_parse_from(["agentctl", "port-forward", "8080"]).expect("inferred port-forward arguments");
        let Command::PortForward { arguments, .. } = inferred.command.expect("a command") else {
            panic!("expected port-forward command");
        };
        assert_eq!(split_forward_arguments(&arguments), (None, arguments.as_slice()));

        let flagged = Arguments::try_parse_from(["agentctl", "port-forward", "--agent", "worker", "0.0.0.0:80:80"])
            .expect("flagged port-forward arguments");
        let Command::PortForward { agent, arguments, .. } = flagged.command.expect("a command") else {
            panic!("expected port-forward command");
        };
        assert_eq!(agent.as_deref(), Some("worker"));
        assert_eq!(split_forward_arguments(&arguments), (None, arguments.as_slice()));
    }

    #[test]
    fn apply_defaults_to_agent_yaml_and_accepts_only_variant_names() {
        let default = Arguments::try_parse_from(["agentctl", "apply"]).expect("default apply");
        let Command::Apply { filename, variant, .. } = default.command.expect("a command") else {
            panic!("expected apply command");
        };
        assert!(filename.is_none());
        assert!(variant.is_none());
        assert_eq!(
            apply_manifest_path(filename, variant).expect("default path"),
            Path::new("agent.yaml")
        );

        let nested =
            Arguments::try_parse_from(["agentctl", "apply", "--variant", "nested-build"]).expect("variant apply");
        let Command::Apply { filename, variant, .. } = nested.command.expect("a command") else {
            panic!("expected apply command");
        };
        assert_eq!(
            apply_manifest_path(filename, variant).expect("variant path"),
            Path::new("agent.nested-build.yaml")
        );
        assert!(Arguments::try_parse_from(["agentctl", "apply", "-f", "agent.yaml", "--variant", "nested"]).is_err());
        assert!(Arguments::try_parse_from(["agentctl", "apply", "--variant", "../nested"]).is_err());
        assert!(
            Arguments::try_parse_from([
                "agentctl",
                "exec",
                "--agent",
                "worker",
                "--variant",
                "nested",
                "--",
                "true"
            ])
            .is_err()
        );
    }

    #[test]
    fn wait_durations_are_bounded_and_explicit() {
        assert_eq!(parse_duration("30s").expect("seconds"), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").expect("minutes"), Duration::from_mins(10));
        assert_eq!(parse_duration("2h").expect("hours"), Duration::from_hours(2));
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("forever").is_err());
    }

    #[test]
    fn status_shows_github_tokens_as_account_place_and_expiry() {
        let token = |owner: Option<&str>, login: &str, kind: &str, expires_at| agent::github::GithubToken {
            owner: owner.map(str::to_owned),
            account: agent::github::GithubAccount {
                login: login.into(),
                name: login.into(),
                email: format!("{login}@example.com"),
            },
            kind: agent::github::TokenKind::of(kind),
            expires_at,
        };
        let mut next = Vec::new();
        assert_eq!(
            github_lines(&[token(None, "danielskovli-agent", "gho_x", None)], &mut next),
            ["danielskovli-agent in every organization, no expiry"]
        );
        assert!(next.is_empty());
        let lines = github_lines(
            &[
                token(
                    Some("digdir"),
                    "digdir-bot",
                    "github_pat_x",
                    Some(time::macros::datetime!(2020-01-02 0:00 UTC)),
                ),
                token(None, "danielskovli-agent", "ghp_x", None),
            ],
            &mut next,
        );
        assert_eq!(
            lines,
            [
                "digdir-bot in digdir, expired 2020-01-02",
                "danielskovli-agent in every other organization, no expiry"
            ]
        );
        assert_eq!(next.len(), 1, "the expired token is to be replaced: {next:?}");
        assert_eq!(github_lines(&[], &mut Vec::new()), ["no account"]);
    }

    #[test]
    fn condition_reasons_read_as_lowercase_words() {
        assert_eq!(lowercase_words("Stopped"), "stopped");
        assert_eq!(lowercase_words("ImageBuildFailed"), "image build failed");
    }

    #[test]
    fn wait_timeout_reports_the_last_ready_diagnostic() {
        let condition = agent::Condition {
            kind: "Ready".into(),
            status: agent::ConditionStatus::False,
            reason: "SecretMissing".into(),
            message: ".env does not define required variable \"GITHUB_TOKEN\"".into(),
            last_transition_time: None,
        };

        let message = wait_timeout_message("worker", Some(&condition));
        assert!(
            message.contains("\"worker\"") && message.contains("SecretMissing") && message.contains(&condition.message),
            "{message}"
        );
    }

    #[test]
    fn inference_errors_have_one_actionable_message() {
        let ambiguous = inference_error(Error::Rpc(agent::control_api::ResponseError {
            code: -32602,
            message: "multiple agents were applied from this directory; specify --agent or --variant".into(),
        }));
        assert_eq!(
            ambiguous.to_string(),
            "multiple agents were applied from this directory; specify --agent or --variant"
        );

        let missing = inference_error(Error::Rpc(agent::control_api::ResponseError {
            code: -32004,
            message: "Agent not found".into(),
        }));
        assert_eq!(
            missing.to_string(),
            "no agent here; `agentctl init` sets one up, or pass --agent NAME"
        );
    }

    #[test]
    fn apply_source_is_resolved_in_the_client_working_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manifest_path = directory.path().join("agent.yaml");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/minimal/agent.yaml"),
            &manifest_path,
        )
        .expect("copy example manifest");

        let request = read_apply_request_in(|| Ok(directory.path().to_path_buf()), PathBuf::from("agent.yaml"), None)
            .expect("read apply request");
        let actual_directory = std::fs::canonicalize(&request.source_directory).expect("canonical source directory");
        let expected_directory = std::fs::canonicalize(directory.path()).expect("canonical temporary directory");
        assert_eq!(actual_directory, expected_directory);
    }
}
