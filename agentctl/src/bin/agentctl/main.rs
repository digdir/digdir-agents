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
    upgrade::InstallPaths,
};
use clap::{Parser, Subcommand, ValueEnum};

mod builder;
mod consent;
mod environment_file;
mod format;
mod github;
mod launch;
mod new;
mod progress;
mod prompt;
mod sandbox_sign_in;
mod secret;
mod self_update;
mod skills;
mod tui;
mod update;
mod watch;

use format::{condition_status, format_age, format_harnesses, session_state};
use futures_util::StreamExt as _;
use sandbox::{execution::ExecutionEvent, terminal::TerminalAttachOutcome};
use tokio::io::AsyncWriteExt as _;
use tokio::runtime::LocalRuntime;

#[derive(Parser)]
#[command(
    name = "agentctl",
    about = "Run coding agents in sandboxes on this machine",
    after_help = "To stop every agent at once: agentctl kill --all",
    version = agent::build_version()
)]
struct Arguments {
    /// Agent control-plane home.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the terminal UI with every agent and session.
    Tui,
    /// Create or update an agent, or a volume, from a manifest.
    Apply {
        /// Agent or volume manifest path; defaults to ./agent.yaml.
        #[arg(short = 'f', long = "filename", conflicts_with_all = ["variant", "from"])]
        filename: Option<PathBuf>,
        /// Variant of the agent in the current directory.
        #[arg(long, value_parser = parse_variant_name, conflicts_with_all = ["filename", "from"])]
        variant: Option<AgentVariantName>,
        /// A definition by reference, such as `bundled/agent-builder`, or `agent-builder` for short,
        /// instead of a manifest on disk.
        #[arg(long, value_name = "REFERENCE")]
        from: Option<String>,
        /// A host folder for a bind mount, as `SOURCE:TARGET`, in place of the source the manifest
        /// names, or as `SOURCE` for the one bind mount it names no source for. Repeatable. An
        /// update that gives none keeps the agent's.
        #[arg(long = "mount", value_name = "SOURCE[:TARGET]", value_parser = parse_bind_source)]
        mounts: Vec<agent::control_plane::BindSource>,
        /// Override metadata.name so one manifest can create multiple agents.
        #[arg(long)]
        name: Option<String>,
        /// File supplying declared manifest environment and secret values; defaults to `.env`
        /// beside the manifest. Keep files containing secrets outside bind-mounted directories.
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// Let the agent read the `.env` files found in the folders it mounts or copies, which may hold
        /// real keys, without asking. At a terminal agentctl asks, and otherwise refuses them.
        #[arg(long)]
        allow_env_files: bool,
        /// Stay attached after applying and show provisioning progress until the agent is Ready.
        #[arg(long)]
        wait: bool,
        /// Maximum wait with `--wait`, written as seconds, minutes, or hours (for example `10m`).
        #[arg(long, default_value = "10m", value_parser = parse_duration, requires = "wait")]
        timeout: Duration,
    },
    /// Make an agent from a definition and attach to a session in it.
    ///
    /// An agent of the definition that already works on the same folders is refused, with the
    /// command that attaches to it. `--name` makes another one, and `apply` updates one.
    Run {
        /// A definition by reference, such as `bundled/agent-builder`, or `agent-builder` for short,
        /// or a definition folder or manifest on disk, written as a path, such as `./agents/reviewer`
        /// or `agent.yaml`.
        definition: String,
        /// The folder for the definition's one bind mount without a source, such as the checkout
        /// it works on.
        folder: Option<PathBuf>,
        /// A host folder for a bind mount, as `SOURCE:TARGET`, in place of the source the definition
        /// names, or as `SOURCE` for the one bind mount it names no source for. Repeatable.
        #[arg(long = "mount", value_name = "SOURCE[:TARGET]", value_parser = parse_bind_source)]
        mounts: Vec<agent::control_plane::BindSource>,
        /// Delete the agent when this command ends.
        #[arg(long, conflicts_with = "detach")]
        rm: bool,
        /// Start the agent without attaching to a session.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Name the agent instead of having it named after the definition and folder.
        #[arg(long)]
        name: Option<String>,
        /// Session to attach to, made if it does not exist.
        #[arg(long, default_value = "main")]
        session: String,
        /// File supplying declared manifest environment and secret values, for a definition on
        /// disk; defaults to `.env` beside its manifest.
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// Let the agent read the `.env` files found in the folders it mounts or copies, which may hold
        /// real keys, without asking. At a terminal agentctl asks, and otherwise refuses them.
        #[arg(long)]
        allow_env_files: bool,
        #[command(flatten)]
        selection: SessionSelection,
    },
    /// Create or attach to a named session in an agent sandbox.
    Attach {
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
    /// Start a stopped agent on its kept disk and wait until it is ready.
    Start {
        #[command(flatten)]
        target: RunStateTarget,
    },
    /// Stop an agent's sandbox VM, start it on its kept disk and wait until the agent is Ready.
    ///
    /// Use it when an agent is unresponsive, for example after running out of memory. A stopped
    /// agent is started. agentd finishes the restart when the command is interrupted, and
    /// attaching to a session afterwards resumes it, as after a stop and a start.
    Restart {
        #[command(flatten)]
        target: RunStateTarget,
        /// End the VM at once instead of letting it shut down. Whatever the agent wrote in its last
        /// seconds may be lost.
        #[arg(long)]
        now: bool,
    },
    /// Stop an agent, or every agent, at once, as `stop --now` does: an emergency stop.
    ///
    /// The VMs end without shutting down; disks are kept, but what an agent wrote in its last
    /// seconds may be lost. Without an agent, asks whether to stop every running agent.
    Kill {
        #[command(flatten)]
        target: KillTarget,
    },
    /// Make a new agent for a repository with the agent builder, which asks what it is for and writes
    /// its definition.
    New(new::NewArguments),
    /// Update the agent definitions in a checkout to this version of agentctl, with the agent
    /// builder.
    ///
    /// The builder finds what changed since each definition's version, proposes each change and
    /// writes it once you agree, for you to review and commit. With `--print-task`, an agent working
    /// in the checkout gets the same update to do itself. To update agentctl itself, run `agentctl
    /// self update`.
    Update(update::UpdateArguments),
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
    /// Manage the skills of the harnesses on this machine, outside any agent.
    Skills {
        #[command(subcommand)]
        command: skills::SkillsCommand,
    },
    /// Give agents a GitHub account, through logins kept under names their manifests ask for.
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
    /// Store secrets, such as API keys, under names that agents' manifests ask for.
    Secret {
        #[command(subcommand)]
        command: secret::SecretCommand,
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
        /// Include ephemeral agents and their sessions, which belong to the client that ran them,
        /// such as `agentctl run --rm`, and listings leave out otherwise.
        #[arg(long)]
        all: bool,
        /// Keep printing the resources each time they change, as one line each with `-o json`.
        #[arg(short = 'w', long)]
        watch: bool,
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
    /// Update agentctl and agentd themselves.
    ///
    /// To update the agent definitions in a checkout to this version, run `agentctl update`.
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
    Volume,
    Definition,
}

/// The agent `start` or `restart` acts on: `agent/NAME` or `agent NAME`, and how long to wait.
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

/// The agents `kill` acts on, as for `stop`; it always stops them at once.
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

/// The session a verb acts on: `session/NAME` or `session NAME`, plus its owning agent.
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

/// Selections fixed when a command creates a session. An existing session keeps
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
    /// Mint a long-lived Claude token and store it for agents under a name: with Claude Code on
    /// this host, or else in a short-lived agent, signing in with this host's browser.
    ///
    /// Agents whose manifest has `login: NAME` on their Claude Code harness use it, so each
    /// subscription you sign in to can serve its own agents. Inside an agent with Claude Code,
    /// store that agent's mediated placeholder instead, so a nested `agentd` chains through the
    /// outer mediation without ever holding a real credential.
    Login {
        /// The name manifests ask for the login by.
        #[arg(default_value_t)]
        name: agent::harness::LoginName,
        /// Read an existing credential from standard input instead.
        #[arg(long)]
        from_stdin: bool,
    },
    /// List the stored Claude logins and the agents that use each.
    List,
    /// Forget a stored Claude login. Agents using it move at once to another stored login their
    /// manifest allows, or lose Claude Code until it is stored again.
    Logout {
        /// The name of the login to forget.
        #[arg(default_value_t)]
        name: agent::harness::LoginName,
    },
}

#[derive(Subcommand)]
enum SshConfigCommand {
    /// Include the generated agent configuration from `~/.ssh/config`, once, at the top.
    Install,
}

#[derive(Subcommand)]
enum CodexCommand {
    /// Sign in with `ChatGPT` and store an agent-only grant under a name: with Codex on this host,
    /// or else in a short-lived agent, signing in with this host's browser.
    ///
    /// Agents whose manifest has `login: NAME` on their Codex harness use it, so each
    /// subscription you sign in to can serve its own agents. Inside an agent with Codex, store the
    /// placeholder credential file that agent's Codex has instead, so a nested `agentd` chains
    /// through the outer mediation without ever holding a real credential.
    Login {
        /// The name manifests ask for the login by.
        #[arg(default_value_t)]
        name: agent::harness::LoginName,
        /// Read a credential file from standard input instead; only an agent's placeholder file
        /// is accepted that way.
        #[arg(long)]
        from_stdin: bool,
    },
    /// List the stored Codex logins and the agents that use each.
    List,
    /// Forget a stored Codex login. Agents using it move at once to another stored login their
    /// manifest allows, or lose Codex until it is stored again.
    Logout {
        /// The name of the login to forget.
        #[arg(default_value_t)]
        name: agent::harness::LoginName,
    },
}

#[derive(Subcommand)]
enum GithubCommand {
    /// Keep a GitHub login under a name, separate from your own `gh` login.
    ///
    /// At a terminal it asks how to get the token: signing in through the browser as a GitHub user
    /// made for agents, creating a fine-grained token in GitHub's pre-filled form, or pasting one.
    /// It shows whom the token acts as and asks before keeping it. Agents whose manifest has
    /// `accounts: [{type: github, login: NAME}]` use it at once.
    Login {
        /// The name manifests ask for the login by, such as `altinn-studio`.
        name: String,
        /// Sign in through the browser, approving the GitHub CLI as the user agents act as. It runs
        /// in a short-lived agent, so this machine needs no `gh`.
        #[arg(long, group = "way")]
        web: bool,
        /// Create a fine-grained token in GitHub's form, filled in except for the repositories,
        /// and paste it.
        #[arg(long, group = "way")]
        fine_grained: bool,
        /// Read a token from standard input instead: fine-grained, classic or a GitHub CLI login's,
        /// such as `gh auth token | agentctl github login NAME --from-stdin`.
        #[arg(long, group = "way")]
        from_stdin: bool,
        /// Keep the token only if it acts as this GitHub user, without asking.
        #[arg(long, value_name = "LOGIN")]
        user: Option<String>,
    },
    /// List the kept logins, and the logins agents ask for that are not kept.
    List,
    /// Forget a kept login; agents asking for it lose GitHub access at once, and nothing else.
    ///
    /// Its token keeps working on GitHub until it expires or you revoke it there.
    Logout {
        /// The login's name, as `agentctl github list` shows it.
        name: String,
    },
}

fn main() -> ExitCode {
    if launch::as_browser(|name| std::env::var_os(name)) {
        return launch::open_argument();
    }
    match run() {
        Ok(code) => code,
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
        if !matches!(arguments.command, Command::Self_ { .. }) {
            self_update::resume_pending_before_command(&home)?;
        }
        if !matches!(
            arguments.command,
            Command::Create { .. }
                | Command::Prompt { .. }
                | Command::Self_ { .. }
                | Command::Skills { .. }
                | Command::SshConfig { .. }
        ) {
            ensure_daemon(&home, &client).await?;
        }
        Box::pin(execute(arguments.command, &home, &client)).await
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "keep command dispatch together; behavior lives in the handlers"
)]
async fn execute(command: Command, home: &ControlPlaneHome, client: &Client) -> CommandResult<ExitCode> {
    match command {
        Command::Self_ { command } => self_update::execute(command, home).await?,
        Command::Skills { command } => skills::execute(command)?,
        Command::Claude { command } => {
            let harness = agent::Harness::ClaudeCode;
            match command {
                ClaudeCommand::Login { name, from_stdin } => {
                    login(client, home, harness, name, from_stdin.then(read_token_from_stdin)).await?;
                }
                ClaudeCommand::List => print_logins(client, harness).await?,
                ClaudeCommand::Logout { name } => logout(client, harness, name).await?,
            }
        }
        Command::Codex { command } => {
            let harness = agent::Harness::Codex;
            match command {
                CodexCommand::Login { name, from_stdin } => {
                    login(client, home, harness, name, from_stdin.then(read_stdin_to_end)).await?;
                }
                CodexCommand::List => print_logins(client, harness).await?,
                CodexCommand::Logout { name } => logout(client, harness, name).await?,
            }
        }
        Command::Github { command } => match command {
            GithubCommand::Login {
                name,
                web,
                fine_grained,
                from_stdin,
                user,
            } => {
                let way = [
                    (web, github::Way::Web),
                    (fine_grained, github::Way::FineGrained),
                    (from_stdin, github::Way::Stdin),
                ]
                .into_iter()
                .find_map(|(given, way)| given.then_some(way));
                github::login(client, home, &name, way, user.as_deref()).await?;
            }
            GithubCommand::List => github::list(client).await?,
            GithubCommand::Logout { name } => github::logout(client, &name).await?,
        },
        Command::Secret { command } => secret::run(client, command).await?,
        Command::New(arguments) => new::new(arguments, home, client).await?,
        Command::Update(arguments) => update::update(arguments, home, client).await?,
        Command::Apply {
            filename,
            variant,
            from,
            mounts,
            name,
            env_file,
            allow_env_files,
            wait,
            timeout,
        } => {
            let directory = std::env::current_dir().map_err(Error::from)?;
            let filename = match from {
                Some(reference) => referenced_manifest(client, &reference).await?,
                None => apply_manifest_path(filename, variant, &directory)?,
            };
            if manifest_kind(&filename).as_deref() == Some(agent::volumes::KIND) {
                apply_volume(
                    client,
                    filename,
                    name,
                    env_file,
                    allow_env_files,
                    wait.then_some(timeout),
                )
                .await?;
                return Ok(ExitCode::SUCCESS);
            }
            let mut request = read_apply_request(filename, env_file)?;
            if let Some(name) = name {
                request.agent.metadata.name = name;
            }
            request.binds = absolute_binds(mounts)?;
            let reader = format!("agent/{}", request.agent.metadata.name);
            let ask = consent::Ask {
                reader: &reader,
                action: "Mount",
                allowed: allow_env_files,
            };
            let applied = consent::send(
                request,
                &ask,
                |request| &mut request.allow_exposed_files,
                |request| client.apply(request),
            )
            .await?;
            let name = applied.metadata.name;
            println!("agent/{name} applied");
            if wait {
                let converged = wait_until_converged(client, &name, timeout).await?;
                let outcome = if converged.spec.is_stopped() {
                    "stopped"
                } else {
                    "ready"
                };
                println!("agent/{name} {outcome}");
                if let Some(condition) = converged
                    .status
                    .conditions
                    .iter()
                    .find(|condition| condition.kind == agent::Condition::ENVIRONMENT_FILE)
                {
                    eprintln!("Warning: {}", condition.message);
                }
            }
        }
        Command::Get {
            resource,
            name,
            agent,
            variant,
            all_agents,
            archived,
            all,
            watch,
            output,
        } => {
            let scope = SessionScope {
                agent,
                variant,
                all_agents,
                archived,
                ephemeral: all,
            };
            get_resources(client, &resource, name, scope, watch, output).await?;
        }
        Command::Describe { resource, name, output } => describe(client, &resource, name, output).await?,
        Command::Delete {
            resource,
            name,
            agent,
            variant,
        } => {
            let (resource, name) = resource_reference(&resource, name)?;
            match resource {
                Resource::Agent => {
                    reject_session_scope(agent.as_deref(), variant.as_ref(), false)?;
                    let name = require_name(name, "Agent")?;
                    client.delete(&name).await?;
                    println!("agent/{name} deleted");
                }
                Resource::Volume => {
                    reject_session_scope(agent.as_deref(), variant.as_ref(), false)?;
                    let name = require_name(name, "Volume")?;
                    client.delete_volume(&name).await?;
                    // A volume goes once no agent uses it, so name the agents keeping it.
                    let volume = match client.get_volume(&name).await {
                        Ok(volume) => Some(volume),
                        Err(Error::Rpc(error)) if error.is_not_found() => None,
                        Err(error) => return Err(error.into()),
                    };
                    let users = volume.as_ref().map(|volume| volume.status.agents()).unwrap_or_default();
                    if users.is_empty() {
                        println!("volume/{name} deleted");
                    } else {
                        println!(
                            "volume/{name} is deleted once no agent uses it; used by {}",
                            users.join(", ")
                        );
                    }
                }
                Resource::Session => {
                    let name = SessionName::new(require_name(name, "Session")?)?;
                    let agent = resolve_agent_name(client, agent, variant).await?;
                    client.delete_session(&agent, name.clone()).await?;
                    println!("session/{agent}/{name} deleted");
                }
                Resource::Definition => {
                    return Err(Error::Invalid(
                        "a definition is a folder or one agentd carries, which agentctl does not delete".into(),
                    )
                    .into());
                }
            }
        }
        Command::Stop { target } => stop(client, target).await?,
        Command::Kill { target } => stop(client, target.into()).await?,
        Command::Start { target } => set_run_state(client, target, RunState::Running).await?,
        Command::Restart { target, now } => restart(client, target, now).await?,
        Command::Archive { target } => set_archived(client, target, true).await?,
        Command::Unarchive { target } => set_archived(client, target, false).await?,
        Command::Attach {
            resource,
            name,
            agent,
            variant,
            selection,
        } => attach(home, client, &resource, name, agent, variant, selection).await?,
        Command::Run {
            definition,
            folder,
            mounts,
            rm,
            detach,
            name,
            session,
            env_file,
            allow_env_files,
            selection,
        } => {
            let mut binds = mounts;
            binds.extend(folder.map(|source| agent::control_plane::BindSource { source, target: None }));
            let run = Run {
                definition,
                binds,
                lifetime: rm.then_some(agent::Lifetime::Ephemeral),
                name,
                env_file,
                allow_env_files,
            };
            let session = (!detach).then(|| SessionName::new(session)).transpose()?;
            run_agent(home, client, run, session, selection).await?;
        }
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
        } => return ssh(client, resource, agent, variant, &command).await,
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
        } => vnc_info(home, client, resource, agent, variant, output).await?,
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

/// The `get` flags that select sessions.
struct SessionScope {
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    all_agents: bool,
    archived: bool,
    /// Whether a listing includes ephemeral agents and their sessions.
    ephemeral: bool,
}

/// The sessions `get` shows, once the owning agent is resolved.
struct SessionFilter {
    /// The owning agent, or none for every agent.
    agent: Option<String>,
    name: Option<SessionName>,
    archived: bool,
}

impl SessionFilter {
    /// A named session is shown even when archived. A listing shows archived ones only when asked.
    fn matches(&self, session: &Session) -> bool {
        self.agent.as_ref().is_none_or(|agent| session.agent == *agent)
            && self
                .name
                .as_ref()
                .map_or_else(|| self.archived || !session.is_archived(), |name| session.name == *name)
    }
}

/// Prints the agent `name`, or every agent, which agentd lists without the ephemeral ones unless
/// `ephemeral`, as they belong to the client that asked for them.
async fn get_agents(
    client: &Client,
    name: Option<String>,
    ephemeral: bool,
    watch: bool,
    output: OutputFormat,
) -> CommandResult<()> {
    let agents = if let Some(name) = &name {
        vec![client.get(name).await?]
    } else {
        client.list_agents(ephemeral).await?
    };
    if watch {
        // A named agent is shown whatever its lifetime.
        return watch::follow(
            client,
            ephemeral || name.is_some(),
            |resources| {
                let mut agents = resources.agents;
                agents.retain(|agent| name.as_ref().is_none_or(|name| agent.metadata.name == *name));
                render_watched(output, &agents, agents_table)
            },
            &mut std::io::stdout(),
        )
        .await;
    }
    match output {
        OutputFormat::Json => print_json(&agents)?,
        OutputFormat::Table => print_table(&agents_table(&agents)),
    }
    Ok(())
}

async fn get_resources(
    client: &Client,
    resource: &str,
    name: Option<String>,
    scope: SessionScope,
    watch: bool,
    output: OutputFormat,
) -> CommandResult<()> {
    let (resource, name) = resource_reference(resource, name)?;
    // A watch first reads as `get` does, so that it fails the same way, such as on a misspelled name.
    match resource {
        Resource::Agent => {
            reject_session_scope(scope.agent.as_deref(), scope.variant.as_ref(), scope.all_agents)?;
            return get_agents(client, name, scope.ephemeral, watch, output).await;
        }
        Resource::Definition => {
            reject_session_scope(scope.agent.as_deref(), scope.variant.as_ref(), scope.all_agents)?;
            if name.is_some() || watch || scope.ephemeral || scope.archived {
                return Err(Error::Invalid(
                    "definitions are listed for the current directory, without a name, --watch, --all or --archived"
                        .into(),
                )
                .into());
            }
            let directory = std::env::current_dir().map_err(Error::from)?;
            let definitions = client
                .definitions(Some(directory.clone()), agent::definitions::Detail::Choosing)
                .await?;
            match output {
                OutputFormat::Json => print_json(&definitions)?,
                OutputFormat::Table => print_table(&format::definitions_table(&definitions, &directory)),
            }
        }
        Resource::Volume => {
            reject_session_scope(scope.agent.as_deref(), scope.variant.as_ref(), scope.all_agents)?;
            let volumes = if let Some(name) = &name {
                vec![client.get_volume(name).await?]
            } else {
                client.list_volumes().await?
            };
            if watch {
                return watch::follow(
                    client,
                    false,
                    |resources| {
                        let mut volumes = resources.volumes;
                        volumes.retain(|volume| name.as_ref().is_none_or(|name| volume.metadata.name == *name));
                        render_watched(output, &volumes, format::volumes_table)
                    },
                    &mut std::io::stdout(),
                )
                .await;
            }
            match output {
                OutputFormat::Json => print_json(&volumes)?,
                OutputFormat::Table => print_table(&format::volumes_table(&volumes)),
            }
        }
        Resource::Session => {
            let name = name.map(SessionName::new).transpose()?;
            if name.is_some() && scope.all_agents {
                return Err(
                    Error::Invalid("a named session requires --agent or current-directory inference".into()).into(),
                );
            }
            let agent = if scope.all_agents {
                None
            } else {
                Some(resolve_agent_name(client, scope.agent, scope.variant).await?)
            };
            let filter = SessionFilter {
                agent,
                name,
                archived: scope.archived,
            };
            let sessions = if let (Some(agent), Some(name)) = (&filter.agent, &filter.name) {
                vec![client.get_session(agent, name.clone()).await?]
            } else {
                let mut sessions = client.list_sessions(filter.agent.as_deref(), scope.ephemeral).await?;
                sessions.retain(|session| filter.matches(session));
                sessions
            };
            let table = |sessions: &[Session]| sessions_table(sessions, filter.agent.is_none());
            if watch {
                return watch::follow(
                    client,
                    scope.ephemeral || filter.agent.is_some(),
                    |resources| {
                        let mut sessions = resources.sessions;
                        sessions.retain(|session| filter.matches(session));
                        render_watched(output, &sessions, table)
                    },
                    &mut std::io::stdout(),
                )
                .await;
            }
            match output {
                OutputFormat::Json => print_json(&sessions)?,
                OutputFormat::Table => print_table(&table(&sessions)),
            }
        }
    }
    Ok(())
}

/// One state of a watch: a line in the shape `-o json` prints, or a table and a blank line.
fn render_watched<T: serde::Serialize>(
    output: OutputFormat,
    items: &[T],
    table: impl Fn(&[T]) -> Vec<String>,
) -> CommandResult<String> {
    Ok(match output {
        OutputFormat::Json => format!("{}\n", serde_json::to_string(items).map_err(Error::from)?),
        OutputFormat::Table => {
            let lines = table(items);
            if lines.is_empty() {
                "No resources found.\n\n".into()
            } else {
                format!("{}\n\n", lines.join("\n"))
            }
        }
    })
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
    resource: &str,
    name: Option<String>,
    agent: Option<String>,
    variant: Option<AgentVariantName>,
    selection: SessionSelection,
) -> CommandResult<()> {
    let (resource, name) = resource_reference(resource, name)?;
    if resource != Resource::Session {
        return Err(Error::Invalid("attach requires a session resource".into()).into());
    }
    let session = SessionName::new(require_name(name, "Session")?)?;
    let agent = resolve_agent_name(client, agent, variant).await?;
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
    let agent = resolve_execution_agent(client, resource, agent, variant).await?;
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

/// Splits a leading agent reference from the port mappings.
///
/// A first argument containing ':' or made only of digits is a port mapping;
/// anything else names the agent. Agent names cannot contain ':' and port
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

/// Opens the local OpenSSH client against the agent's generated alias.
///
/// The agent is converged first, so the server and the key material exist by
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
    ssh_config_arguments(&mut ssh, &access.config_file);
    ssh.arg(&access.alias).args(command);
    run_ssh_client(ssh)
}

fn ssh_client_executable() -> String {
    format!("ssh{}", std::env::consts::EXE_SUFFIX)
}

/// Adds `-F config` to an OpenSSH client command line, so that it reads `config` in place of the
/// user's configuration.
///
/// On Windows the path goes in double quotes, which Rust leaves out of an argument without a
/// space. Git for Windows' `ssh` expands `*`, `?`, `[...]` and `{...}` in an argument outside
/// quotes, as a shell would, and would then read another file.
fn ssh_config_arguments(ssh: &mut ProcessCommand, config: &Path) {
    ssh.arg("-F");
    #[cfg(windows)]
    if let Some(text) = config.to_str() {
        use std::os::windows::process::CommandExt as _;

        ssh.raw_arg(agent::ssh::shell_word(text, agent::ssh::CommandShell::Windows));
        return;
    }
    ssh.arg(config);
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
    show_warnings();
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

/// Shows warnings on standard error, such as why connecting waits, for a relay whose standard
/// output carries only the relayed stream.
fn show_warnings() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_level(false)
        .init();
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

/// Forwards the agent's desktop to a local port and holds it open.
///
/// The agent is converged first, so the platform-owned bridge from the guest
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
    // Refuses early, with the remedy, when the agent declares no VNC access.
    client.vnc_access(&agent).await?;
    let wait = progress::Wait::start();
    let target = wait
        .until(
            client,
            &agent,
            client.ensure_execution(&agent, WaitPolicy::UntilConverged),
        )
        .await?;
    // Read again now the agent is Ready: which ports its image offers is something a
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

fn sign_in() -> agent::harness::SignIn {
    launch::sign_in(|name| std::env::var_os(name), std::env::current_exe().ok())
}

/// Hands the address to whichever local application handles its scheme.
///
/// Best effort by design: there is no portable VNC viewer, the address is
/// already printed, and a missing handler must not fail the forward.
async fn open_locally(url: &str) {
    match launch::open_url(url).await {
        Ok(opener) => println!("Asked {opener} to open {url}."),
        Err(error) => eprintln!("{error}"),
    }
}

/// Relays one desktop connection over standard input and output.
///
/// This is the seam for a viewer that dials through a command rather than a
/// port, and for tooling that wants the RFB stream without a listening socket.
async fn vnc_proxy(home: &ControlPlaneHome, client: &Client, resource: String) -> CommandResult<ExitCode> {
    show_warnings();
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
    home: &ControlPlaneHome,
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
            for line in format::vnc_access_lines(&access, home.path()) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

/// Resolves a [`SessionTarget`] into the owning agent and session name.
async fn session_target(client: &Client, target: SessionTarget) -> CommandResult<(String, SessionName)> {
    let (resource, name) = resource_reference(&target.resource, target.name)?;
    if resource != Resource::Session {
        return Err(Error::Invalid("this command requires a session resource".into()).into());
    }
    let session = SessionName::new(require_name(name, "Session")?)?;
    let agent = resolve_agent_name(client, target.agent, target.variant).await?;
    Ok((agent, session))
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
    if resource == Resource::Volume {
        let volume = client.get_volume(&require_name(name, "Volume")?).await?;
        match output {
            OutputFormat::Json => print_json(&volume)?,
            OutputFormat::Table => {
                for line in format::describe_volume_lines(&volume) {
                    println!("{line}");
                }
            }
        }
        return Ok(());
    }
    if resource != Resource::Agent {
        return Err(Error::Invalid("describe supports agent and volume resources".into()).into());
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
    // A stopped agent is never Ready, also while its stop is still in progress.
    if client.get(&name).await?.spec.is_stopped()
        || wait_until_converged(client, &name, timeout).await?.spec.is_stopped()
    {
        return Err(Error::Stopped(name).into());
    }
    println!("agent/{name} condition met");
    Ok(())
}

/// Resolves `agent/NAME` or `agent NAME` for a verb that acts only on agents.
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
        "volume" | "volumes" | "vol" => Resource::Volume,
        "definition" | "definitions" | "def" => Resource::Definition,
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
            CommandError::Message("no agent here; `agentctl apply` creates one, or pass --agent NAME".into())
        }
        Error::Rpc(error) => CommandError::Message(error.message),
        error => error.into(),
    }
}

/// Follows an agent's convergence with live progress until it has its desired
/// run state, Ready or stopped, and returns it then; or until a terminal
/// error, the timeout, or Ctrl-C.
async fn wait_until_converged(client: &Client, name: &str, timeout: Duration) -> CommandResult<Agent> {
    converge_shown(&progress::Wait::start(), client, name, timeout).await
}

/// Waits until `name` converges, showing its progress as `wait` does.
async fn converge_shown(wait: &progress::Wait, client: &Client, name: &str, timeout: Duration) -> CommandResult<Agent> {
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
    let total = names.len();
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
                // Bounded by the deadline too, so a query agentd does not answer cannot outlast it.
                let Ok(current) = tokio::time::timeout_at(deadline, client.get(name)).await else {
                    return None;
                };
                let doing = match current {
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
    // An emergency stop stops what listings leave out too.
    let agents: Vec<Agent> = client
        .list_agents(true)
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

/// Stops or starts an agent and waits until it is stopped or Ready.
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

/// Restarts an agent and waits until it is Ready again.
async fn restart(client: &Client, target: RunStateTarget, now: bool) -> CommandResult<()> {
    let name = agent_reference(&target.resource, target.name)?;
    client.restart(&name, now).await?;
    let converged = wait_until_converged(client, &name, target.timeout).await?;
    if converged.spec.is_stopped() {
        return Err(CommandError::Message(format!(
            "agent/{name} was stopped again before it restarted"
        )));
    }
    println!("agent/{name} restarted");
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

/// Parses `--mount SOURCE[:TARGET]`. The target is what follows the last `:` before a `/`, so a
/// Windows drive, as in `C:\\src`, stays part of the source.
fn parse_bind_source(value: &str) -> Result<agent::control_plane::BindSource, String> {
    let split = value
        .rmatch_indices(":/")
        .map(|(index, _)| index)
        .find(|index| !(*index == 1 && value.as_bytes()[0].is_ascii_alphabetic()));
    let (source, target) = split.map_or((value, None), |index| {
        (&value[..index], Some(::sandbox::SandboxPath::new(&value[index + 1..])))
    });
    if source.is_empty() {
        return Err("the source folder is missing".into());
    }
    Ok(agent::control_plane::BindSource {
        source: PathBuf::from(source),
        target,
    })
}

/// `binds` with their sources made absolute against the current folder.
fn absolute_binds(
    binds: Vec<agent::control_plane::BindSource>,
) -> Result<Vec<agent::control_plane::BindSource>, Error> {
    binds
        .into_iter()
        .map(|bind| {
            Ok(agent::control_plane::BindSource {
                source: absolute(std::env::current_dir, bind.source)?,
                target: bind.target,
            })
        })
        .collect()
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

fn agents_table(agents: &[Agent]) -> Vec<String> {
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
    // Ephemeral agents are listed only when asked for, and then said to be.
    if agents.iter().any(|agent| agent.status.is_ephemeral()) {
        let rows = agents
            .iter()
            .zip(rows)
            .map(|(agent, mut row)| {
                let lifetime = if agent.status.is_ephemeral() { "ephemeral" } else { "-" };
                row.insert(1, lifetime.into());
                row
            })
            .collect::<Vec<_>>();
        return table_lines(&["NAME", "LIFETIME", "READY", "STATUS", "HARNESSES", "PROVIDER"], &rows);
    }
    table_lines(&["NAME", "READY", "STATUS", "HARNESSES", "PROVIDER"], &rows)
}

fn print_agent_description(agent: &Agent) {
    for line in format::describe_agent_lines(agent) {
        println!("{line}");
    }
}

fn sessions_table(sessions: &[Session], show_agent: bool) -> Vec<String> {
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
    table_lines(&headers, &rows)
}

/// The lines of a table, or none without rows.
fn table_lines(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    if rows.is_empty() {
        Vec::new()
    } else {
        format::table_lines(headers, rows)
    }
}

fn print_table(lines: &[String]) {
    if lines.is_empty() {
        eprintln!("No resources found.");
    }
    for line in lines {
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
            return Err(startup_failure(
                home,
                &format!("automatic startup exited with {status}"),
            ));
        }
    }
    Err(startup_failure(
        home,
        "automatic startup did not become ready within 10 seconds",
    ))
}

/// An update stops agentd and keeps it stopped until it starts the new one, so a command that
/// tries to start agentd meanwhile fails. The update need not be for this command's home, so the
/// usual diagnostics stay.
fn startup_failure(home: &ControlPlaneHome, failure: &str) -> Error {
    let diagnostics = daemon_startup_diagnostics(home);
    if InstallPaths::resolve()
        .and_then(|paths| paths.update_running())
        .unwrap_or(false)
    {
        return Error::Daemon(format!(
            "{failure}, likely because an agentctl update is running; rerun this command once it has finished, or {diagnostics}"
        ));
    }
    Error::Daemon(format!("{failure}; {diagnostics}"))
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

fn apply_manifest_path(
    filename: Option<PathBuf>,
    variant: Option<AgentVariantName>,
    directory: &Path,
) -> Result<PathBuf, Error> {
    match (filename, variant) {
        (Some(filename), None) => Ok(filename),
        (None, Some(variant)) => Ok(PathBuf::from(variant.filename())),
        (None, None) => default_manifest(directory),
        (Some(_), Some(_)) => Err(Error::Invalid("--filename and --variant are mutually exclusive".into())),
    }
}

/// The manifest `apply` uses without `--filename` in `directory`: its `agent.yaml`. Any other it
/// finds, as [`found_manifests`] does, it only names, so nothing is applied that was not named.
fn default_manifest(directory: &Path) -> Result<PathBuf, Error> {
    if directory.join(manifest::MANIFEST_FILE).is_file() {
        return Ok(PathBuf::from(manifest::MANIFEST_FILE));
    }
    let found = found_manifests(directory);
    let shown = |path: &PathBuf| path.strip_prefix(directory).unwrap_or(path).display().to_string();
    Err(Error::Invalid(match found.as_slice() {
        [] if agent::local::repository_root(directory).is_some() => {
            "no agent.yaml in this repository; name a manifest with --filename".into()
        }
        [] => "no agent.yaml here or in agents/<name>/; name a manifest with --filename".into(),
        [only] => format!("no agent.yaml here; apply {0} with `--filename {0}`", shown(only)),
        several => format!(
            "no agent.yaml here; name one of these with --filename: {}",
            several.iter().map(shown).collect::<Vec<_>>().join(", ")
        ),
    }))
}

/// The manifests [`agent::definitions::manifests`] finds for `directory`, variants left out.
fn found_manifests(directory: &Path) -> Vec<PathBuf> {
    agent::definitions::manifests(directory)
        .into_iter()
        .filter(|path| path.file_name() == Some(std::ffi::OsStr::new(manifest::MANIFEST_FILE)))
        .collect()
}

/// The `kind` a manifest declares, when it can be read.
fn manifest_kind(filename: &Path) -> Option<String> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&std::fs::read(filename).ok()?).ok()?;
    value.get("kind")?.as_str().map(str::to_owned)
}

/// Applies a volume manifest and, with `wait`, follows its newest revision until its copy is ready or failed.
async fn apply_volume(
    client: &Client,
    filename: PathBuf,
    name: Option<String>,
    env_file: Option<PathBuf>,
    allow_env_files: bool,
    wait: Option<Duration>,
) -> CommandResult<()> {
    if env_file.is_some() {
        return Err(Error::Invalid("--env-file applies only to agents".into()).into());
    }
    let mut request = read_volume_request(filename)?;
    if let Some(name) = name {
        request.volume.metadata.name = name;
    }
    let reader = format!("Every agent that mounts volume/{}", request.volume.metadata.name);
    let ask = consent::Ask {
        reader: &reader,
        action: "Copy",
        allowed: allow_env_files,
    };
    let applied = consent::send(
        request,
        &ask,
        |request| &mut request.allow_exposed_files,
        |request| client.apply_volume(request),
    )
    .await?;
    let name = applied.metadata.name;
    println!("volume/{name} applied");
    let Some(timeout) = wait else {
        return Ok(());
    };
    let mut renderer = progress::Renderer::stderr();
    let copied = tokio::time::timeout(timeout, async {
        let mut after = None;
        loop {
            // Each reply follows a change, such as the copy advancing.
            let resources = client.watch_resources(after, false).await?;
            after = Some(resources.revision);
            let status = resources
                .volumes
                .into_iter()
                .find(|volume| volume.metadata.name == name)
                .ok_or_else(|| Error::VolumeNotFound(name.clone()))?
                .status;
            // A copy that is done has become the latest revision, which keeps its progress.
            let progress = status.copy.as_ref().map_or_else(
                || status.latest().and_then(|revision| revision.progress.as_ref()),
                |copy| copy.progress.as_ref(),
            );
            if let Some(progress) = progress {
                renderer.render_copy(progress);
            }
            let Some(copy) = &status.copy else {
                return Ok(status.latest().map(|revision| revision.number));
            };
            if let Some(failure) = &copy.failure {
                return Err(Error::Invalid(format!(
                    "volume/{name} revision {} failed: {failure}",
                    copy.number
                )));
            }
        }
    })
    .await;
    renderer.finish();
    let copied = copied.map_err(|_| {
        Error::Invalid(format!(
            "timed out waiting for volume {name:?} to finish copying; agentd keeps copying it"
        ))
    })??;
    match copied {
        Some(number) => println!("volume/{name} ready (revision {number})"),
        None => println!("volume/{name} ready"),
    }
    Ok(())
}

/// Reads a volume manifest into the request that applies it, resolving its source against the manifest's
/// directory.
fn read_volume_request(filename: PathBuf) -> Result<agent::volumes::ApplyVolumeRequest, Error> {
    let filename = absolute(std::env::current_dir, filename)?;
    let bytes = std::fs::read(&filename)
        .map_err(|error| Error::Invalid(format!("{}: could not read manifest: {error}", filename.display())))?;
    let volume = agent::volumes::decode(&bytes)?;
    let source_directory = filename
        .parent()
        .ok_or_else(|| Error::Invalid("manifest path has no parent directory".into()))?
        .to_path_buf();
    Ok(agent::volumes::ApplyVolumeRequest {
        source_directory,
        manifest_path: Some(filename),
        volume,
        allow_exposed_files: std::collections::BTreeSet::new(),
    })
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
        binds: Vec::new(),
        create_only: false,
        allow_exposed_files: std::collections::BTreeSet::new(),
        agent,
    })
}

/// The manifest of agentd's copy of the definition `reference` names.
async fn referenced_manifest(client: &Client, reference: &str) -> Result<PathBuf, Error> {
    let reference = agent::DefinitionReference::parse(reference)?.to_string();
    let definitions = client.definitions(None, agent::definitions::Detail::Choosing).await?;
    let references = definitions
        .iter()
        .filter_map(|definition| Some((definition.reference.as_deref()?, &definition.path)));
    if let Some((_, path)) = references.clone().find(|(listed, _)| *listed == reference) {
        return Ok(path.clone());
    }
    let references = references.map(|(listed, _)| listed).collect::<Vec<_>>();
    Err(Error::Invalid(if references.is_empty() {
        format!("agentd has no definition {reference:?}, nor any other")
    } else {
        format!(
            "agentd has no definition {reference:?}; it has {}",
            references.join(", ")
        )
    }))
}

/// What `agentctl run` runs, as given on its command line.
struct Run {
    definition: String,
    binds: Vec<agent::control_plane::BindSource>,
    lifetime: Option<agent::Lifetime>,
    name: Option<String>,
    env_file: Option<PathBuf>,
    /// Whether `--allow-env-files` lets the agent read the `.env` files in its folders.
    allow_env_files: bool,
}

impl Run {
    /// The request for agentd: a definition written as a path is one on disk, and anything else
    /// is a reference agentd resolves.
    fn request(&self) -> Result<agent::control_plane::RunRequest, Error> {
        if !is_written_as_path(&self.definition)
            && self.definition.contains(['/', '\\'])
            && Path::new(&self.definition).exists()
        {
            return Err(Error::Invalid(format!(
                "{:?} is read as a definition reference; write ./{} for the folder here",
                self.definition, self.definition
            )));
        }
        let definition = if is_written_as_path(&self.definition) {
            // Canonical, as running the same definition again finds its agent by its manifest's path.
            let path = common::fs::canonicalize(&self.definition)
                .map_err(|error| Error::Invalid(format!("definition {}: {error}", self.definition)))?;
            let manifest = if path.is_dir() {
                path.join(manifest::MANIFEST_FILE)
            } else {
                path
            };
            let read = read_apply_request(manifest, self.env_file.clone())?;
            agent::control_plane::RunDefinition::Local {
                source_directory: read.source_directory,
                manifest_path: read
                    .manifest_path
                    .ok_or_else(|| Error::Invalid("the manifest has no path".into()))?,
                env_file: read.env_file,
                agent: Box::new(read.agent),
            }
        } else if self.env_file.is_some() {
            return Err(Error::Invalid(format!(
                "--env-file is for a definition on disk, and {:?} is a reference; write ./{} for a folder here",
                self.definition, self.definition
            )));
        } else {
            agent::control_plane::RunDefinition::Reference {
                reference: self.definition.clone(),
            }
        };
        Ok(agent::control_plane::RunRequest {
            definition,
            binds: absolute_binds(self.binds.clone())?,
            lifetime: self.lifetime,
            name: self.name.clone(),
            allow_exposed_files: std::collections::BTreeSet::new(),
        })
    }
}

/// Whether `definition` is written as a path rather than as a reference: starting at a root, `.`
/// or `..`, or a manifest file name. Others, such as `agents/reviewer`, are references, as
/// `npm install` and `docker run` read such names, since a reference can hold a `/` too.
fn is_written_as_path(definition: &str) -> bool {
    let path = Path::new(definition);
    // A root counts without a drive too, as in `/srv/agents/reviewer` on Windows.
    path.has_root()
        || matches!(
            path.components().next(),
            Some(std::path::Component::CurDir | std::path::Component::ParentDir)
        )
        || path
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml")
}

/// Runs an agent and, given a session, attaches to it. An ephemeral agent lives until this
/// returns, however it ends.
async fn run_agent(
    home: &ControlPlaneHome,
    client: &Client,
    run: Run,
    session: Option<SessionName>,
    selection: SessionSelection,
) -> CommandResult<()> {
    let ask = consent::Ask {
        reader: "The agent",
        action: "Mount",
        allowed: run.allow_env_files,
    };
    let (ran, _lease) = consent::send(
        run.request()?,
        &ask,
        |request| &mut request.allow_exposed_files,
        |request| client.run(request),
    )
    .await?;
    let name = ran.agent.metadata.name;
    println!("agent/{name} created");
    let Some(session) = session else {
        return Ok(());
    };
    let target = progress::Wait::start()
        .until(
            client,
            &name,
            client.ensure_session(&name, session, selection.request(None), WaitPolicy::UntilConverged),
        )
        .await?;
    agent::sessions::attach(home.path(), &target).await?;
    Ok(())
}

/// Stores a harness login: the credential on standard input when given, else, inside an agent with
/// the harness, that agent's mediated credential, else a sign-in on this host, in a short-lived
/// agent when this host does not have the harness.
async fn login(
    client: &Client,
    home: &ControlPlaneHome,
    harness: agent::Harness,
    name: agent::harness::LoginName,
    from_stdin: Option<Result<zeroize::Zeroizing<String>, Error>>,
) -> CommandResult<()> {
    let (credential, imported, source) = match from_stdin {
        Some(credential) => (credential?, true, ""),
        None => match agent::harness::agent_credential(harness, agent::harness::inside_agent(), |name| {
            std::env::var_os(name)
        })? {
            Some(credential) => (credential, true, " from this agent's mediated login"),
            None if agent::harness::host_can_sign_in(harness) => (
                agent::harness::acquire_host_credential(harness, home.path(), sign_in)?,
                false,
                "",
            ),
            None => return sandbox_login(client, home, harness, name).await,
        },
    };
    let stored = client
        .auth_login(harness, name, credential.to_string(), imported)
        .await?;
    println!("{} login {} stored{source}", stored.provider, stored.login);
    Ok(())
}

/// Signs in to `harness` in a short-lived agent and stores the login as `name`, for a machine
/// without the harness's own command.
async fn sandbox_login(
    client: &Client,
    home: &ControlPlaneHome,
    harness: agent::Harness,
    name: agent::harness::LoginName,
) -> CommandResult<()> {
    let display = harness.display_name();
    let Some(recipe) = agent::harness::sandbox_sign_in(harness) else {
        return Err(Error::Invalid(format!("{display} has no login to sign in to")).into());
    };
    if !prompt::interactive() {
        return Err(CommandError::Message(format!(
            "{display} is not installed here, and signing in without it needs a terminal and a browser"
        )));
    }
    let credential = sandbox_sign_in::sign_in(client, home, display, &recipe).await?;
    // A credential read off the sign-in's screen can be missed or come out garbled while the person
    // still sees it whole, so they can paste it instead.
    let read_off_screen = recipe.artifact.is_none();
    let paste = |what: String| {
        CommandError::Message(format!(
            "{what}; paste the one it printed into {} with --from-stdin",
            agent::harness::login_command(harness, &name)
        ))
    };
    let credential = match credential {
        Ok(credential) => credential,
        Err(failure) => {
            return Err(match *failure.error {
                Error::Invalid(reason) if read_off_screen => paste(reason),
                error => error.into(),
            });
        }
    };
    let stored = match client
        .auth_login(harness, name.clone(), credential.to_string(), false)
        .await
    {
        Ok(stored) => stored,
        Err(error) if read_off_screen => {
            return Err(paste(format!(
                "the {display} credential read from the screen was refused: {error}"
            )));
        }
        Err(error) => return Err(error.into()),
    };
    println!("{} login {} stored", stored.provider, stored.login);
    Ok(())
}

async fn logout(client: &Client, harness: agent::Harness, name: agent::harness::LoginName) -> CommandResult<()> {
    if client.auth_logout(harness, name.clone()).await? {
        println!("{} login {name} forgotten", harness.display_name());
        Ok(())
    } else {
        Err(CommandError::Message(format!(
            "no {} login is named {name}",
            harness.display_name()
        )))
    }
}

/// Prints `harness`'s stored logins, and those its agents ask for that are not stored, with the
/// agents that use each.
async fn print_logins(client: &Client, harness: agent::Harness) -> CommandResult<()> {
    let mut logins = std::collections::BTreeMap::<_, (bool, Vec<String>)>::new();
    for stored in client.auth_logins().await? {
        if stored.harness == harness {
            logins.entry(stored.login).or_default().0 = true;
        }
    }
    for agent in client.list_agents(true).await? {
        let Some(installation) = agent.spec.harness(harness) else {
            continue;
        };
        // The login the agent was given, or else the one it asks for, when it asks for just one.
        let login = agent
            .status
            .sandbox
            .as_ref()
            .and_then(|assignment| assignment.login(harness))
            .or_else(|| installation.login.single());
        if let Some(login) = login {
            logins
                .entry(login.clone())
                .or_default()
                .1
                .push(agent.metadata.name.clone());
        }
    }
    if logins.is_empty() {
        println!(
            "No {} logins; run {} to sign in",
            harness.display_name(),
            agent::harness::login_command(harness, &agent::harness::LoginName::default())
        );
        return Ok(());
    }
    let rows = logins
        .into_iter()
        .map(|(name, (stored, agents))| {
            vec![
                name.to_string(),
                if stored { "yes" } else { "no" }.into(),
                if agents.is_empty() {
                    "-".into()
                } else {
                    agents.join(",")
                },
            ]
        })
        .collect::<Vec<_>>();
    for line in table_lines(&["NAME", "STORED", "AGENTS"], &rows) {
        println!("{line}");
    }
    Ok(())
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
    fn a_mount_names_its_target_after_the_last_colon_before_a_slash() {
        let parsed = |value: &str| {
            super::parse_bind_source(value).map(|bind| {
                (
                    bind.source.display().to_string(),
                    bind.target.map(|target| target.as_str().to_owned()),
                )
            })
        };
        assert_eq!(parsed("../app"), Ok(("../app".into(), None)));
        assert_eq!(
            parsed("~/wt/a:b:/work/app"),
            Ok(("~/wt/a:b".into(), Some("/work/app".into())))
        );
        assert_eq!(parsed("C:/src/app"), Ok(("C:/src/app".into(), None)));
        assert_eq!(
            parsed("C:/src/app:/work/app"),
            Ok(("C:/src/app".into(), Some("/work/app".into())))
        );
        assert!(parsed(":/work/app").is_err());
    }

    #[test]
    fn run_reads_a_definition_written_as_a_path_as_one_on_disk_and_anything_else_as_a_reference() {
        for path in [
            ".",
            "..",
            "./agents/reviewer",
            "../reviewer",
            "/srv/agents/reviewer",
            "agent.yaml",
            "x/agent.yml",
        ] {
            assert!(super::is_written_as_path(path), "{path}");
        }
        for reference in [
            "agent-builder",
            "bundled/agent-builder",
            "agents/reviewer",
            "kihub.digdir.no/dotnet-dev",
        ] {
            assert!(!super::is_written_as_path(reference), "{reference}");
        }
    }

    #[test]
    fn help_leads_with_the_purpose_and_the_commands_people_use_first() {
        let help = <Arguments as clap::CommandFactory>::command().render_help().to_string();
        assert!(
            help.starts_with("Run coding agents in sandboxes on this machine"),
            "{help}"
        );
        let commands = help
            .lines()
            .skip_while(|line| line.trim() != "Commands:")
            .skip(1)
            .take_while(|line| !line.trim().is_empty())
            .filter_map(|line| line.split_whitespace().next())
            .collect::<Vec<_>>();
        assert_eq!(commands[..5], ["tui", "apply", "run", "attach", "stop"], "{help}");
        for hidden in ["ssh-proxy", "vnc-proxy"] {
            assert!(!commands.contains(&hidden), "{help}");
        }
        assert!(
            Arguments::try_parse_from(["agentctl", "ssh-proxy", "worker"]).is_ok(),
            "OpenSSH still runs the hidden command"
        );
    }

    #[test]
    fn secret_set_takes_a_name_and_hosts_and_never_a_value_argument() {
        let parse = |arguments: &[&str]| Arguments::try_parse_from(arguments).map(|arguments| arguments.command);
        assert!(matches!(
            parse(&["agentctl", "secret", "set", "aivar", "--hosts", "api.aivar.no,*.aivar.no", "--from-stdin"]),
            Ok(Command::Secret {
                command: secret::SecretCommand::Set { name, hosts: Some(hosts), from_stdin: true }
            }) if name == "aivar" && hosts == ["api.aivar.no", "*.aivar.no"]
        ));
        assert!(matches!(
            parse(&["agentctl", "secret", "set", "aivar"]),
            Ok(Command::Secret {
                command: secret::SecretCommand::Set {
                    hosts: None,
                    from_stdin: false,
                    ..
                }
            })
        ));
        for arguments in [
            &["agentctl", "secret", "set"][..],
            &["agentctl", "secret", "set", "aivar", "sk-FAKE_canary"],
            &["agentctl", "secret", "set", "aivar", "--value", "sk-FAKE_canary"],
            &["agentctl", "secret", "remove", "aivar", "sk-FAKE_canary"],
        ] {
            assert!(parse(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn github_takes_a_name_and_never_a_token_argument() {
        let parse = |arguments: &[&str]| Arguments::try_parse_from(arguments).map(|arguments| arguments.command);
        assert!(matches!(
            parse(&["agentctl", "github", "login", "altinn-studio", "--from-stdin"]),
            Ok(Command::Github {
                command: GithubCommand::Login { name, from_stdin: true, web: false, fine_grained: false, user: None }
            }) if name == "altinn-studio"
        ));
        assert!(matches!(
            parse(&["agentctl", "github", "login", "altinn-studio", "--web", "--user", "altinn-bot"]),
            Ok(Command::Github {
                command: GithubCommand::Login { web: true, user: Some(user), .. }
            }) if user == "altinn-bot"
        ));
        assert!(matches!(
            parse(&["agentctl", "github", "logout", "altinn-studio"]),
            Ok(Command::Github {
                command: GithubCommand::Logout { name }
            }) if name == "altinn-studio"
        ));
        for arguments in [
            &["agentctl", "github", "login"][..],
            &["agentctl", "github", "login", "digdir", "--token", "ghp_FAKE_canary"],
            &["agentctl", "github", "login", "digdir", "ghp_FAKE_canary"],
            &["agentctl", "github", "login", "digdir", "--token=ghp_FAKE_canary"],
            &["agentctl", "github", "list", "ghp_FAKE_canary"],
            &["agentctl", "github", "login", "digdir", "--web", "--fine-grained"],
            &["agentctl", "github", "login", "digdir", "--web", "--from-stdin"],
        ] {
            assert!(parse(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn kill_is_stop_now() {
        let stop = |arguments: &[&str]| {
            let parsed = Arguments::try_parse_from(arguments).expect("arguments");
            match parsed.command {
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
            let (Command::Create { selection, .. } | Command::Attach { selection, .. }) = arguments.command else {
                panic!("expected a session creation command");
            };
            let request = selection.request(None);
            assert_eq!(request.model_selection.model_str(), Some("claude-fable-5"));
            assert_eq!(request.model_selection.effort_str(), Some("xhigh"));
            assert_eq!(request.harness, None);

            let omitted = Arguments::try_parse_from(["agentctl", verb, "session/s1"]).expect("omitted selections");
            let (Command::Create { selection, .. } | Command::Attach { selection, .. }) = omitted.command else {
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

    fn session(agent: &str, name: &str, archived: bool) -> Session {
        let mut session = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "agentId": "00000000-0000-0000-0000-000000000002",
            "agent": agent,
            "name": name,
            "harness": "claudeCode",
            "createdAt": "2026-08-25T00:00:00Z",
        });
        if archived {
            session["archivedAt"] = "2026-08-26T00:00:00Z".into();
        }
        serde_json::from_value(session).expect("test session should deserialize")
    }

    #[test]
    fn session_filter_follows_the_get_flags() {
        let sessions = [
            session("worker", "s1", false),
            session("worker", "old", true),
            session("builder", "b1", false),
        ];
        let shown = |agent: Option<&str>, name: Option<&str>, archived: bool| {
            let filter = SessionFilter {
                agent: agent.map(str::to_owned),
                name: name.map(|name| SessionName::new(name.to_owned()).expect("name")),
                archived,
            };
            sessions
                .iter()
                .filter(|session| filter.matches(session))
                .map(|session| session.name.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(shown(Some("worker"), None, false), ["s1"]);
        assert_eq!(shown(Some("worker"), None, true), ["s1", "old"]);
        assert_eq!(shown(Some("worker"), Some("old"), false), ["old"]);
        assert_eq!(shown(Some("builder"), Some("s1"), false), Vec::<String>::new());
        assert_eq!(shown(None, None, false), ["s1", "b1"]);
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
        assert_eq!(
            resource_reference("vol/apps", None).expect("Volume reference"),
            (Resource::Volume, Some("apps".into()))
        );
        assert!(resource_reference("agent/worker", Some("other".into())).is_err());
        assert!(resource_reference("pods", None).is_err());
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
        } = explicit.command
        else {
            panic!("expected exec command");
        };
        assert!(stdin);
        assert!(tty);
        assert_eq!(resource.as_deref(), Some("agent/worker"));
        assert!(agent.is_none());
        assert_eq!(command, ["bash", "-l"]);

        let inferred = Arguments::try_parse_from(["agentctl", "exec", "--", "pwd"]).expect("inferred exec arguments");
        let Command::Exec { resource, command, .. } = inferred.command else {
            panic!("expected exec command");
        };
        assert!(resource.is_none());
        assert_eq!(command, ["pwd"]);
    }

    #[test]
    fn port_forward_accepts_kubectl_shapes_and_inference() {
        let explicit = Arguments::try_parse_from(["agentctl", "port-forward", "agent/worker", "9090:80", ":5432"])
            .expect("explicit port-forward arguments");
        let Command::PortForward { agent, arguments, .. } = explicit.command else {
            panic!("expected port-forward command");
        };
        assert!(agent.is_none());
        assert_eq!(
            split_forward_arguments(&arguments),
            (Some("agent/worker".into()), &arguments[1..])
        );

        let inferred =
            Arguments::try_parse_from(["agentctl", "port-forward", "8080"]).expect("inferred port-forward arguments");
        let Command::PortForward { arguments, .. } = inferred.command else {
            panic!("expected port-forward command");
        };
        assert_eq!(split_forward_arguments(&arguments), (None, arguments.as_slice()));

        let flagged = Arguments::try_parse_from(["agentctl", "port-forward", "--agent", "worker", "0.0.0.0:80:80"])
            .expect("flagged port-forward arguments");
        let Command::PortForward { agent, arguments, .. } = flagged.command else {
            panic!("expected port-forward command");
        };
        assert_eq!(agent.as_deref(), Some("worker"));
        assert_eq!(split_forward_arguments(&arguments), (None, arguments.as_slice()));
    }

    #[test]
    fn apply_without_a_filename_names_the_manifests_it_finds_but_applies_none() {
        let folder = tempfile::tempdir().expect("folder");
        let home = folder.path();
        let invalid = |result: Result<PathBuf, Error>| match result {
            Err(Error::Invalid(message)) => message,
            other => panic!("expected an invalid request, got {other:?}"),
        };
        // The message shows paths as the platform writes them, with `\` on Windows.
        let shown = |path: &str| Path::new(path).components().collect::<PathBuf>().display().to_string();
        assert!(invalid(default_manifest(home)).starts_with("no agent.yaml here or in agents/<name>/"));

        std::fs::create_dir_all(home.join("agents/notes")).expect("agent folder");
        std::fs::write(home.join("agents/notes/agent.yaml"), "").expect("manifest");
        std::fs::write(home.join("agents/notes/agent.gpu.yaml"), "").expect("variant");
        assert_eq!(found_manifests(home), [home.join("agents/notes/agent.yaml")]);
        assert_eq!(
            invalid(default_manifest(home)),
            format!(
                "no agent.yaml here; apply {0} with `--filename {0}`",
                shown("agents/notes/agent.yaml")
            )
        );

        std::fs::create_dir_all(home.join("agents/work")).expect("second agent folder");
        std::fs::write(home.join("agents/work/agent.yaml"), "").expect("second manifest");
        assert_eq!(
            invalid(default_manifest(home)),
            format!(
                "no agent.yaml here; name one of these with --filename: {}, {}",
                shown("agents/notes/agent.yaml"),
                shown("agents/work/agent.yaml")
            )
        );

        let repository = home.join("code/app");
        std::fs::create_dir_all(repository.join(".git")).expect("repository");
        std::fs::create_dir_all(repository.join("deploy/agent")).expect("agent folder");
        assert!(invalid(default_manifest(&repository)).starts_with("no agent.yaml in this repository"));
        std::fs::write(repository.join("deploy/agent/agent.yaml"), "").expect("manifest");
        std::fs::create_dir_all(repository.join("src")).expect("subfolder");
        assert_eq!(
            found_manifests(&repository.join("src")),
            [repository.join("deploy/agent/agent.yaml")],
            "anywhere in the repository"
        );
    }

    #[test]
    fn apply_defaults_to_agent_yaml_and_accepts_only_variant_names() {
        let default = Arguments::try_parse_from(["agentctl", "apply"]).expect("default apply");
        let Command::Apply { filename, variant, .. } = default.command else {
            panic!("expected apply command");
        };
        assert!(filename.is_none());
        assert!(variant.is_none());
        let here = tempfile::tempdir().expect("directory");
        std::fs::write(here.path().join("agent.yaml"), "").expect("manifest");
        assert_eq!(
            apply_manifest_path(filename, variant, here.path()).expect("default path"),
            Path::new("agent.yaml")
        );

        let nested =
            Arguments::try_parse_from(["agentctl", "apply", "--variant", "nested-build"]).expect("variant apply");
        let Command::Apply { filename, variant, .. } = nested.command else {
            panic!("expected apply command");
        };
        assert_eq!(
            apply_manifest_path(filename, variant, here.path()).expect("variant path"),
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
    fn errors_are_printed_without_type_prefixes() {
        let rpc = |code, message: &str| {
            CommandError::Agent(Error::Rpc(agent::control_api::ResponseError {
                code,
                message: message.into(),
                data: None,
            }))
        };
        assert_eq!(
            rpc(-32602, "spec.network.deny contains an invalid host pattern").to_string(),
            "spec.network.deny contains an invalid host pattern"
        );
        assert_eq!(
            rpc(-32603, "Agent sandbox setup failed: no space left").to_string(),
            "Agent sandbox setup failed: no space left",
            "an internal error from agentd is shown as agentd wrote it"
        );
        assert_eq!(
            CommandError::Agent(Error::Invalid("at least one port mapping is required".into())).to_string(),
            "at least one port mapping is required"
        );
    }

    #[test]
    fn inference_errors_have_one_actionable_message() {
        let ambiguous = inference_error(Error::Rpc(agent::control_api::ResponseError {
            code: -32602,
            message: "more than one agent is found here (first, second); pass --agent NAME or --variant VARIANT".into(),
            data: None,
        }));
        assert_eq!(
            ambiguous.to_string(),
            "more than one agent is found here (first, second); pass --agent NAME or --variant VARIANT"
        );

        let missing = inference_error(Error::Rpc(agent::control_api::ResponseError {
            code: -32004,
            message: "Agent not found".into(),
            data: None,
        }));
        assert_eq!(
            missing.to_string(),
            "no agent here; `agentctl apply` creates one, or pass --agent NAME"
        );
    }

    #[test]
    fn apply_source_is_resolved_in_the_client_working_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manifest_path = directory.path().join("agent.yaml");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent.yaml"),
            &manifest_path,
        )
        .expect("copy fixture manifest");

        let request = read_apply_request_in(|| Ok(directory.path().to_path_buf()), PathBuf::from("agent.yaml"), None)
            .expect("read apply request");
        let actual_directory = common::fs::canonicalize(&request.source_directory).expect("canonical source directory");
        let expected_directory = common::fs::canonicalize(directory.path()).expect("canonical temporary directory");
        assert_eq!(actual_directory, expected_directory);
    }
}
