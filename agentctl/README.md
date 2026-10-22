# Agent platform

This area explores an open agent platform built on a reusable Sandbox SDK. The platform is designed to run locally
or under later cloud orchestration without coupling Agent automation to one isolation backend, network implementation,
harness or model.

The main goals are:

- long-running Agents with multiple durable Sessions;
- strong isolation with mediated network access and no real secrets inside Sandboxes;
- backend-neutral Sandbox lifecycle, execution, storage, file-transfer and terminal APIs;
- harness-neutral Agent and Session concepts with harness-specific behavior kept in adapters;
- host APIs that work on Linux, macOS and Windows while initially materializing Linux Sandboxes; and
- a Sandbox layer reusable by CI runners and other isolated workloads that do not depend on Agent concepts.

## Install

Install the released `agentctl` and `agentd` on Linux or macOS, then log in to Claude Code:

```sh
curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
agentctl claude login
```

Windows additionally requires the `HypervisorPlatform` optional feature. Install from PowerShell, then open a new
PowerShell window so the updated user `PATH` takes effect:

```powershell
irm https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.ps1 | iex
```

Update an installation with `agentctl self update`.

Then, in any repository, run `agentctl init`. It offers the Claude Code or Codex login when neither is stored, and the
GitHub account login when none is, writes an agent directory when the checkout has no manifest, and creates the Agent.
Once the Agent is Ready it offers to open a Session. Before creating the agent it offers to open the written files in
`$VISUAL` or `$EDITOR`, since the image
(including the Dockerfile's contents) and mounts stay as the agent was created; changing them means deleting it and
creating it again, which starts its disk over. Resources, access, accounts, environment and secrets change with
`agentctl apply`.

`init` asks what the agent should be called, such as `dev` or `reviewer`, and writes `agents/<name>/` at the repository
root, naming the agent `<repository>-<name>` and laying the directory out like the Agents in this repository and in
altinn-studio:

| File | What it is |
| --- | --- |
| `agent.yaml` | The manifest, with every setting a person is likely to change later; those that do not apply yet are commented out |
| `agent.worktree.yaml` | A variant of a cloning `agent.yaml` that works on this checkout through a read-write mount instead |
| `tools.yaml` | The tools the image installs: containers (Podman with `docker` and `docker compose`), a browser, toolchains at the versions the repository declares (Node.js and Corepack, .NET, Go, Rust, Python, Java and Maven), Ubuntu packages and global npm packages. Every manifest in the folder builds the same image |
| `Dockerfile` | Starts from an [`agentctl-base`](../images/agentctl-base) image. Its generated block comes from `tools.yaml`, rewritten whenever an agent is created from the folder; steps after the block are yours. It ends with `agent-image-check`, and Docker on your machine builds it |
| `instructions.md` | The agent's own instructions, installed for every harness beside the checkout's `CLAUDE.md` or `AGENTS.md` |
| `home/` | Files copied into `/home/agent`, such as dotfiles |
| `.env.sample` | Example plain values and secrets for `.env`, which `.gitignore` keeps out of Git with `agent.mine.yaml` |

`init` asks what the agent should be able to do, which picks what its image adds to the `ubuntu-26.04` base image and
what the manifest gives it: writing code (nothing more), also building and running containers (Podman, the default),
also testing web apps in a browser (the base image's `browser` feature), or also a desktop that `agentctl vnc` opens
(its `desktop` feature, with `access: vnc`). A fresh clone is offered for repositories on GitHub, which the base images clone at boot. The
repository's own skills under `.claude/skills` are listed in the manifest, so every harness gets them. `init` checks
for Docker before its first question and waits for it to start, and without Git it works on the mounted folder. The proposed size fits this machine:
up to 4 CPUs, 8 GiB of memory and a disk of at most half the free space on the volume holding `~/.agent`, since an
agent's disk takes host space as the agent writes, up to its size. `agentctl tools` shows the current directory's agent's tools; `agentctl tools add postgresql-client`, `go@1.26` or
`npm:pnpm` and `agentctl tools remove` change them. An agent keeps the image it was created with, so after a change
`agentctl tools` offers to recreate the agent, and `agentctl status` says when an agent's tools changed since it was
created. Edit the files like any others. `agentctl` on its own asks
whether to open the current directory's Agent in `agentctl tui`, or to set one up when there is none, and prints the
help when you decline. `attach`, `exec` and `ssh` likewise offer `init` when they would use the current directory's
Agent and find none, and `agentctl`, `attach` and `exec` offer to start it when it is stopped. `agentctl attach` on its
own opens the agent's only session, starts one when it has none, and asks which when it has several. All of these ask
only when a person is at the terminal; scripts get the usual error.

`agentctl status` shows agentd, the stored logins, the current directory's agent and a count of every agent on this
machine at a glance, with the commands that would change them; `agentctl claude status` and `agentctl codex status`
exit with 1 when that login is missing.

### Stopping every Agent

To stop every Agent at once, as an emergency stop:

```sh
agentctl kill --all     # the same as agentctl stop --all --now
```

Their VMs end at once instead of shutting down, so nothing in them runs or reaches the network any more, and they stay
stopped until `agentctl start agent/NAME`, even when their manifests are applied again. Disks are kept, but what an
Agent wrote in its last seconds may be lost. `agentctl stop` without an Agent lists the ones not stopped yet and asks before
stopping them all; without `--now`, each gets up to 10 seconds to shut down. A kill also cuts short such a graceful
stop already under way, so `kill --all` from another terminal ends a `stop --all` that waits on a guest. It also ends a
setup under way, such as an image build or a boot, so an Agent applied a moment ago stops at once too.
`agentctl kill` is `stop --now`, for one Agent, `--all` of them, or after asking. In `agentctl tui`, `X` does the same as `kill --all` after asking.

## Development

Run `make help` at the repository root for the supported development commands. `make user-install` builds, packages and
installs `agentctl` and `agentd` for the current user. On Windows without Make, run `.\agentctl\make-user-install.ps1` for the
same build, package and installation flow.

Maintainers updating the Microsandbox pin should follow [MICROSANDBOX.md](../sandbox/MICROSANDBOX.md).

User-visible changes are recorded in [CHANGELOG.md](CHANGELOG.md), which covers the whole stack and provides the
release notes for each `agentctl/v*` release.

Upgrading from a released version migrates the Agent database, so Agents and Sessions carry over. State created by
unreleased development builds is not migrated.

## Architecture

```text
Host
├── agentctl             local CLI, transient execution and Session attachment
└── agentd               control plane, reconciliation, policy and SecretStore
    └── Agent            declarative durable resource
        └── Sandbox      isolated execution environment
            ├── Session  durable tmux-backed harness process
            └── Session  durable tmux-backed harness process
```

![What runs where: agentctl, agentd, one msb process per Sandbox and the mediated network](docs/architecture/runtime.svg)

[docs/architecture](docs/architecture/) has diagrams of the crates, the network and secret mediation, and the path from
`agentctl apply` to an attached Session.

The implementation has two deliberately separate layers.

### Sandbox layer

`sandbox` is the generic Rust SDK. A Node hosts Sandboxes, and Sandboxes host Executions. Providers pair a Sandbox
Backend with an Image Backend and advertise platform capabilities before selection. Network Backends are selected
independently and consume a negotiated packet, intercepted-flow or versioned-control endpoint.

`sandbox-microsandbox` implements the Sandbox, Image and Network contracts for Microsandbox. Network enforcement and
secret substitution happen on the trusted mediation path; the Sandbox Backend must not leave an unobserved egress
path. `sandbox-authorization` defines the context-aware authorization vocabulary without depending on the Agent
control plane or an enforcement implementation.

The Sandbox crates do not depend on Agent automation.

### Agent layer

`agentd` owns the durable desired state and all lifecycle effects. `agentctl` starts the adjacent daemon on demand and
communicates through the versioned local control API. Its resource-oriented commands follow `verb resource [name]`;
Session scope is explicit through `--agent` or inferred from the closest unique persisted Agent source directory: the
manifest's directory, a bind-mounted host directory, or, least specifically, the Git checkout the manifest is in, so an
Agent defined in `agents/<name>/` is found from anywhere in its repository.
Transient `exec` commands similarly converge the Agent first, then target its exact materialized Sandbox without
creating durable Session state or taking Sandbox lifecycle ownership away from `agentd`.

An Agent owns one retained Sandbox incarnation. The Agent controller is the sole owner of Sandbox selection,
materialization, setup, network mediation and release. A Session controller can only open the already-materialized
Sandbox and owns the in-Sandbox tmux and harness effects for that Session. Both use the same keyed reconciliation
scheduler, which serializes work per resource identity while allowing unrelated resources to progress concurrently.

Desired state is persisted before reconciliation. Wakeups provide low-latency progress, while startup and periodic
scans ensure dropped notifications or daemon restarts do not lose work. Provider assignment is sticky for an Agent
incarnation, and a reused Agent name never inherits resources from a deleted incarnation. The Sandbox is named after the
incarnation, while its guest hostname is the Agent name so shell prompts and logs identify the Agent.

A running Sandbox's guest can stall while its VM process keeps running. The Sandbox SDK reports the guest's heartbeat
without a round trip to the guest. While reconciliation works inside the guest, `agentd` inspects the heartbeat every 2
seconds and records when it last advanced on the host clock. After 15 seconds without progress the work ends, and the
Agent reports `SandboxResponsive=False` and `Ready=False` with reason `SandboxUnresponsive`. A stalled guest therefore
cannot hold its Agent's reconciliation, and a command waiting for the Agent to become Ready fails once the stall is
recorded. The next pass after the heartbeat advances makes the Agent Ready again. A stalled guest is not restarted
automatically; `agentctl stop` and then `agentctl start` restart it.

An Agent's `spec.runState` is `Running`, the default, or `Stopped`. `agentctl stop` and `agentctl start` set it as a
new generation; a manifest may set it, and `apply` of one that omits it keeps the current run state. For a stopped Agent
the reconciler stops the Sandbox VM and its Network, killing a VM that does not stop gracefully, or at once after
`agentctl stop --now`, and keeps its root
filesystem, Volumes, identity and Provider assignment. It reaches nothing in the guest and reports `Ready=False` and
`SandboxReady=False` with reason `Stopped`. Commands that need the Sandbox fail at once, and its Sessions go Idle, as
after inactivity. A start is an ordinary pass on the same root filesystem; it launches no harness, and the next attach
to a Session resumes its conversation.

Provisioning progress is observed as state, not as a stream. The Sandbox SDK folds progress events into a `Progress`
value, so an observer that joins late or falls behind sees what one that saw every event would. `agentd` keeps each
Agent's latest provisioning pass in memory, and records a routine resync of a Ready Agent only when it fails; the
durable outcome is the Agent's conditions, with their transition times, and its failure class. Clients follow one Agent
with `agents.v1.progress` and every Agent and Session with `resources.v1.watch`, long-polls that return when the daemon
drains. Commands that wait for an Agent, such as `apply --wait`, `wait`, `start` and `stop`, call `agents.v1.converge`
while they follow its progress. It wakes the Agent and returns once a pass for its generation recorded its desired run
state, Ready or stopped, waiting through transient failures.

`agentctl tui` builds on the same two calls: it follows `resources.v1.watch` for the fleet and `agents.v1.progress` for
one Agent's provisioning, and derives each Agent's state from its conditions and failure class.

Sessions have platform-assigned identities independent of tmux and harness-native conversation IDs. Each Session binds
immutably to one of its Agent's declared harness installations and to a model selection (model and effort level)
resolved at creation: the caller's explicit choice, else the installation's manifest `defaults`, else nothing, leaving
the harness's own defaults. Both values are provider-owned identifiers the platform validates but does not interpret.
The selection is recorded with the Session, shown by `agentctl get sessions`, and applied on every launch including
resume, so a later manifest change affects only new Sessions and a model change made inside the harness lasts until
the next relaunch. Detaching leaves a Session running. An inactive, unattached Session becomes Idle and is relaunched
on the next ensure or attach, resuming the harness conversation when its native state still exists. Repeated
unexpected harness exits use bounded backoff. Deleting a Session hides it at once; the Session controller stops its
harness before removing it, so an unreachable Sandbox delays the deletion rather than leaving a harness untracked.
Archiving a Session stops its harness once any turn in progress ends, but not for a turn waiting for approval or quiet
for a minute, and keeps it stopped until the Session is unarchived; it keeps its name and conversation, and the next
attach resumes it.

Tmux is the current Session runtime, not a security boundary or a permanent generic driver abstraction. A second
runtime must establish the common interface before one is introduced.

## Images, home and harnesses

See the [harness compatibility test plan](HARNESSES.md) when updating harness installations.

Agent images own installed tools and optional workspace initialization. Repository checkouts are persistent runtime
data beneath `/home/agent/code`; they are not declared, updated or deleted by the Agent controller. Sessions may clone
repositories they can access, and image init may make a simple best-effort checkout for convenience.
`spec.sandbox.mounts` can instead attach caller-owned host directories or temporary memory filesystems when the selected
Sandbox Provider supports them; these attachments are immutable for the Agent incarnation.

An agent folder's `tools.yaml` lists the tools its image installs: containers (Podman with `docker` and
`docker compose`), the base image's browser or desktop feature, toolchains at the versions it names (Node.js and
Corepack, .NET, Go, Rust, Python, Java and Maven), Ubuntu packages and global npm packages. Before an Agent is created
from the folder, `agentctl apply` and the TUI write them into the generated block of the folder's `Dockerfile`, between
its `# --- Generated from tools.yaml` and `# --- End of the generated block` lines, and leave the rest of the file as it
is. Every manifest in the folder builds the same image, and an Agent keeps the image it was created with. A folder
without `tools.yaml` keeps its Dockerfile by hand.

`spec.home`, when given, is a continuously applied overlay onto `/home/agent`. It converges files supplied by the builder but does
not delete guest files that disappear from the source. Builders may use it to own harness configuration explicitly,
with the consequence that those files are reapplied on every Agent pass.

`spec.harnesses` declares the harness installations available to Sessions and selects the default used for new Sessions.
A declared `version` is verified against the image at setup; omit it when the image owns the version, so image bumps need no manifest change.
Set `optional: true` when an absent host login should omit that installation instead of blocking Agent creation, so a
manifest can offer a harness that not everyone has signed in to. The check runs on every convergence, so signing in on
the host installs the harness on the next pass; until then a Session on it is refused, naming the login. The default
installation cannot usefully be optional, since it is what a Session selecting no harness gets. `auth` may be omitted:
it follows from the harness, `mediated` for Claude Code and Codex and `none` for Pi, which has no platform login and so
cannot be optional.
Each installation may declare `defaults` with a `model` and an `effort` level for its new Sessions, in the harness's
own vocabulary. The published manifests select `model: fable` for Claude Code because a mediated token cannot list
Fable in the `/model` picker.
`agentctl instructions edit` opens the host-wide instructions file, `~/.agent/instructions.md`, which every agent on
the host receives ahead of its own instructions, within one reconciliation pass and without being recreated; emptying
it gives agents none. `spec.instructions` names one harness-neutral Agent instruction file. Every declared Harness Adapter installs that source
at its global instruction location: `~/.claude/CLAUDE.md` for Claude Code, `~/.codex/AGENTS.md` for Codex and
`~/.pi/agent/AGENTS.md` for Pi.
Repository-local instruction files continue to be discovered by the harness itself.

Harness Adapters own authentication, version verification, managed configuration, hooks, native conversation IDs and
launch arguments. The current adapters support Claude Code, Codex CLI and Pi. Harness-owned mutable state is seeded by
the image or the user and is not used as a trusted bootstrap marker.

### Pi

Pi (`@earendil-works/pi-coding-agent`, `type: pi`) is configured by the Agent; `agentctl` knows no provider, gateway or
model. The Agent's Dockerfile installs a pinned Pi, `fd-find` and ripgrep, its manifest declares the harness and the
provider key as a secret, and its home overlay or image provides Pi's own files under `~/.pi/agent/`, such as a
`models.json` that reads the key as `"apiKey": "$MY_GATEWAY_KEY"`. Under `~/.pi/agent/`, `agentctl` writes only
`AGENTS.md`, from `spec.instructions`.

```yaml
  harnesses:
    - type: pi
      defaults:
        model: gateway/some-model
  secrets:
    - environment: MY_GATEWAY_KEY
      allowedHosts: [gateway.example.com]
```

`--model`, `--effort` and `defaults` pass to `pi --model` and `pi --thinking` unchanged, and skills install in
`~/.agents/skills`. Pi starts with `--approve`, trusting the working directory's `.pi` resources. The launch sets
`NODE_EXTRA_CA_CERTS` to the mediation CA bundle, `PI_TELEMETRY=0` and `PI_SKIP_VERSION_CHECK=1` unless
`spec.environment` sets them. See the [self-dev Agent](../agents/self-dev/README.md#pi) for a worked example.

Harness types are fixed when an Agent is created, so adding Pi means deleting and re-applying the Agent. An `agentd`
built before Pi support cannot list Agents once one has `type: pi`; delete those before moving to an older build.

## SSH access

`spec.access: [{type: ssh}]` gives the Agent's user OpenSSH access to the Sandbox as the platform-owned user `agent`:
`agentctl ssh <agent> [-- command]` opens it, `agentctl ssh-config install` makes the alias `agentctl-<name>`
available to plain `ssh`, `sftp` and editors that read OpenSSH configuration, and
`agentctl ssh-info <agent> -o json` describes the connection for other tools. The server listens only inside the
Sandbox and is reached through `agentctl ssh-proxy`; the image must provide OpenSSH, systemd and a usable `agent`
account, while `agentd` installs the isolated server policy and unit. `agent` has passwordless `sudo`, so an SSH
login shares the Sandbox's one trust boundary with Sessions. SSH shells, remote commands and editor servers inherit
the same image, Agent and mediated trust environment as Sandbox Executions; terminal- and Session-specific variables
remain local to their process.

## Secrets and network policy

A secret is any protected host-owned value. Credentials are the subset used for authentication. Generic storage and
mediation therefore use the `SecretStore` concept, while harness login remains an authentication concern.

`spec.environment` explicitly selects non-secret values from the same `.env` file, with `name` as both the Sandbox
variable and default source name. An optional `source` selects a differently named entry. Only declared values are
copied, and they enter the Sandbox in plaintext, where image init and Sandbox Executions inherit them. Reapplying
after changing the file updates the Sandbox environment. Do not declare secrets here.

Manifest secret bindings name a guest environment variable and the hosts where its value may be substituted. The
matching real value is loaded from the manifest directory's `.env` file, or the file named by
`agentctl apply --env-file`, and retained only in the owner-protected host database. A bind mount whose source
contains any active Agent's secret file is refused at apply time, because the Sandbox would otherwise read the real
values from the mounted directory. The Sandbox sees an inert placeholder in the named environment variable. The Network Backend substitutes
the current real value only for an authorized request to an allowed host; rotation does not require copying new
material into the Sandbox. A custom placeholder is optional for clients that validate token shape.
Set `optional: true` when a missing or empty environment-file value should omit that secret binding instead of
rejecting the Agent apply. Required secrets remain the default.

`spec.network` may be omitted; it then defaults to `mode: mediated` and `allow: all`.

Policy is evaluated for live Sandbox-originated operations and fails closed when the destination, authorization,
secret resolution or trusted mediation path is unavailable. Host-destined traffic is restricted to the registered
Platform API endpoint. This authorization is separate from authorization of users calling the host Agent API.
When an Agent image includes Podman, the platform makes the guest's mediated CA bundle available to containers and
build steps through standard trust paths. An OCI hook copies the bundle into the container root filesystem rather
than bind-mounting it, so package managers can still replace the bundle, and it adds the mediator CA as a trust
anchor so a regenerated bundle keeps trusting mediation. Docker and dockerd are not covered by this convenience
wiring.

## GitHub accounts

agentd keeps GitHub tokens for Agents, one per owner, separate from any `gh` login on the host, so Agents can act as
their own account, such as a bot, while you keep using yours.

`agentctl github login` walks through creating a fine-grained token for the owner of the current directory's
repository, or `--owner`. It opens GitHub's token form with the name, owner, a 90-day lifetime and the permissions
filled in: contents, issues, pull requests, Actions and workflow files read and write, and commit statuses read. You
pick the repositories, the one thing the form cannot be given, and paste the token. agentctl then asks GitHub which
account it acts as, whether it can see the repository and when it expires, and shows that before anything is stored.
It warns when the account is also your own `gh` login, is not named like an automation account (`…-bot`, `…-agent`),
cannot see the repository, or expires within a week, and then asks for confirmation. Yes-or-no questions have no
default: they take `y` or `n`, never a bare Enter.

A fine-grained token is kept for its owner. A classic or OAuth token reaches every owner and is kept for every
organization without a token of its own: `--from-stdin` reads any token, `--from-gh` imports the host's own `gh`
login, and `--browser` runs the GitHub CLI's browser sign-in against a throwaway configuration. Because that token
reaches every repository the account can, `--browser` first asks you to type the account's login and refuses a
sign-in as any other. `agentctl github list` shows each token's owner, account, kind and expiry. `agentctl github logout` forgets one, chosen from a list
that ends with "everywhere", and `agentctl github reset` forgets them all after a warning. Agents lose GitHub access on
their next pass; the token stays valid on GitHub until revoked there.

An Agent opts in with `spec.accounts: [{type: github, owner: digdir}]`, and uses its owner's token, else the one for every organization without a token of its own;
without `owner`, the only stored token. Its Sandbox then sees an inert `GITHUB_TOKEN`, which the network mediator
replaces only in requests to `github.com`, `api.github.com`, `gist.github.com` and `uploads.github.com`; a manifest
cannot widen that list. An expired token stops the Agent's reconciliation with an error naming the login to run. The
token's account name and email, or its noreply address, become the Agent's Git identity unless the manifest selects
`GIT_USER_NAME` and `GIT_USER_EMAIL` itself. When the image has Git, setup also gives Git the token for GitHub over
HTTPS and rewrites GitHub's SSH URLs to HTTPS, since the Sandbox has no SSH key, unless the image configured either
itself. A manifest with the account cannot also bind its own `GITHUB_TOKEN`. Changing the account a token acts as
changes that identity, which restarts the Agent's running Sandbox on its next pass, as any change to a Sandbox's
environment does.

SQLite `secure_delete` and owner-only filesystem permissions provide local hygiene. They are not a cryptographic
erasure guarantee across WAL history, filesystem snapshots or backups.

## Current scope and direction

The current milestone provides persistent Agents and Sessions, real Microsandbox lifecycle, mediated harness and GitHub
authentication, image-owned workspace initialization, idle/resume behavior, local packaging and release-pinned runtime
downloads.

Important current limitations are:

- Codex uses a separate ChatGPT subscription login owned and refreshed by `agentd`;
- Sessions share one Sandbox user and tmux server and therefore one trust boundary;
- attachment is still a client-side Provider operation rather than a daemon-owned terminal capability;
- Session content, prompt steering and plugin APIs are not implemented; and
- global scheduling and Kubernetes orchestration are future work.

The next planned slices are:

1. expose harness-native Session content and prompt/steer/interrupt operations;
2. add an authorized Sandbox-facing Platform API for delegation and isolated host plugins; and
3. add global orchestration only after the local control-plane contracts are proven.

## References

- agentdp and nvt-agent: earlier agent-platform prototypes
- Microsandbox and smolvm: microVM and Sandbox implementations
- Herdr: harness multiplexing and native session-state exploration
