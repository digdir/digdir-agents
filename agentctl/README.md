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

### Building an Agent image

An Agent whose `image` specifies a Dockerfile needs a running Docker Engine on the host. Published OCI image
references can be pulled without Docker. Dockerfile builds use `agentd`'s `DOCKER_HOST` when set, otherwise
`DOCKER_CONTEXT` or the context selected with `docker context use`. Local Unix socket and named pipe contexts are
supported, including Docker Desktop and Colima's per-user sockets. Remote Docker contexts are not supported.
`DOCKER_CONFIG` selects the Docker configuration directory, which defaults to `~/.docker`.

`agentd` inherits environment variables when it starts; `apply --env-file` supplies the Agent's environment, not
the host Docker connection. Starting Docker or changing the saved local context lets background retries proceed
without restarting `agentd`.

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
Session scope is explicit through `--agent` or inferred from the closest unique persisted Agent source directory.
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

Each launch tells the harness where it runs: `AGENT_NAME`, `AGENT_SESSION_NAME`, `AGENT_HOST` and
`AGENT_SOURCE_DIRECTORY` (the Agent's directory on the host) are set beside `AGENT_SESSION_ID`, so instructions can ask
an Agent to name itself, for example in a shared task list. The harness, and so its model provider, sees these values;
`AGENT_SOURCE_DIRECTORY` is a host path that does not exist inside the Sandbox. `AGENT_HARNESS` is `claudeCode`, `codex` or
`pi`; the `desktop` helper sizes screenshots by it ([the screenshot budget](../images/agentctl-base/README.md#the-screenshot-budget)).
The tmux status bar shows the Session's name and how to detach.

Tmux is the current Session runtime, not a security boundary or a permanent generic driver abstraction. A second
runtime must establish the common interface before one is introduced.

## Images, home and harnesses

See the [harness compatibility test plan](HARNESSES.md) when updating harness installations.

Agent images own installed tools and optional workspace initialization. Repository checkouts are persistent runtime
data beneath `/home/agent/code`; they are not declared, updated or deleted by the Agent controller. Sessions may clone
repositories they can access, and image init may make a simple best-effort checkout for convenience.
`spec.sandbox.mounts` can instead attach caller-owned host directories or temporary memory filesystems when the selected
Sandbox Provider supports them; these attachments are immutable for the Agent incarnation.

`spec.sandbox.resources.nestedSandboxes` lets the Sandbox run Sandboxes of its own, such as nested Agents. With
`required`, the Agent fails on a host that cannot provide them; with `preferred`, it runs without them there, and its
`NestedSandboxes` condition says why. Microsandbox gives the Sandbox `/dev/kvm` for them, which needs a Linux x86_64
host with nested KVM enabled or a Mac with an Apple M3 or newer chip and macOS 15 or newer. Agent setup gives the device
to the `agent` group, so the image need not grant it. Turning nested Sandboxes on or off restarts a running Agent. To
run nested Agents, the image needs `agentctl` and `agentd`: copy them from the [agentctl
image](../images/agentctl/README.md), at a release, the newest `main` or one commit. Inside an Agent, `agentctl claude
login` and `agentctl codex login` store that Agent's mediated placeholders instead of signing in, so a nested `agentd`
never holds a real credential.

`spec.home` is a continuously applied overlay onto `/home/agent`. It converges files supplied by the builder but does
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
`spec.instructions` names one harness-neutral Agent instruction file. Every declared Harness Adapter installs that source
at its global instruction location: `~/.claude/CLAUDE.md` for Claude Code, `~/.codex/AGENTS.md` for Codex and
`~/.pi/agent/AGENTS.md` for Pi.
`agentd` writes a short Agent sandbox section ahead of it, from the manifest and what setup finds in the Sandbox: the
Agent's resources, the installed harnesses, the configured secrets and their hosts, network, access and whether nested
Agents can run. Without `spec.instructions` the image or `spec.home` may own those files, and the section goes above
what they hold; Codex does not read it when the image ships `~/.codex/AGENTS.override.md`. Every Agent also gets an
`agentctl` skill for driving Agents, nested ones included. A skill of its own in `spec.skills` by that name replaces it;
one in `spec.home` is overwritten.
Repository-local instruction files continue to be discovered by the harness itself.

Harness Adapters own authentication, version verification, managed configuration, hooks, native conversation IDs and
launch arguments. Claude Code Bash permission requests, including deletion safety prompts, are automatically approved
inside the Agent's Sandbox. Other tool permission prompts and user questions remain interactive.
The current adapters support Claude Code, Codex CLI and Pi. Harness-owned mutable state is seeded by
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
`PI_TELEMETRY=0` and `PI_SKIP_VERSION_CHECK=1` unless `spec.environment` sets them, and Pi trusts the mediation CA
through the `NODE_EXTRA_CA_CERTS` every Session in an Agent has. See the
[self-dev Agent](../agents/self-dev/README.md#pi) for a worked example.

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

Policy is evaluated for live Sandbox-originated operations and fails closed when the destination, authorization,
secret resolution or trusted mediation path is unavailable. Host-destined traffic is restricted to the registered
Platform API endpoint. This authorization is separate from authorization of users calling the host Agent API.
When an Agent image includes Podman, the platform makes the guest's mediated CA bundle available to containers and
build steps through standard trust paths. An OCI hook copies the bundle into the container root filesystem rather
than bind-mounting it, so package managers can still replace the bundle, and it adds the mediator CA as a trust
anchor so a regenerated bundle keeps trusting mediation. Docker and dockerd are not covered by this convenience
wiring.

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
