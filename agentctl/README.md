# agentctl

`agentctl` runs long-running agents with durable sessions in isolated [sandboxes](../sandbox/), with mediated network
access and no real secrets inside the sandbox.

It is a toolkit for building an agent's computer rather than a fixed workflow. The CLI, the TUI and tmux are one way
in. Any client that connects over SSH, such as a desktop coding app or an editor, works as well.

## Install

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.sh | sh
```

Windows, from PowerShell, with the `HypervisorPlatform` optional feature enabled:

```powershell
irm https://raw.githubusercontent.com/digdir/digdir-agents/main/agentctl/install.ps1 | iex
```

A first install then starts `agentctl setup`, which creates your first agent and signs you in to the harnesses it
uses. `AGENT_SETUP_ARGS` passes it flags, and `AGENT_SETUP=0` skips it. An upgrade has agents already, so it starts
setup only with `AGENT_SETUP=1`. `AGENT_INSTALL_MODE=standalone`, for images and CI jobs, never starts it.

Update with `agentctl self update`. Agents and sessions carry over.

## Your first agent

`agentctl setup` asks a few questions and leaves you with a running agent, opened in your editor if you like:

- **Start from a standard agent**, the choice to make if unsure: an agent for a kind of work, such as ASP.NET Core backends, on a
  published image, so no Docker is needed. `agentctl setup --list-archetypes` lists them. When the image cannot be
  pulled, such as before it is published, setup offers to build it on your machine from the Dockerfile agentctl
  carries, which needs Docker. `--build-image` always builds it.
- **Start from an agent definition that exists**, such as your team's: a manifest on disk, a folder holding one, or a
  GitHub address such as `https://github.com/Altinn/altinn-studio//agents/minimal`, or the address GitHub shows for a
  folder or a manifest, at any branch, tag or commit. A GitHub source is fetched into a temporary checkout that is
  removed once the agent's files are copied. When the manifest mounts its own repository, by a relative bind mount
  source such as `../..`, a local manifest shares the checkout it is in. Setup offers another, such as a worktree
  (`--checkout`). A fetched one has no checkout here, so setup asks whether to share one of yours.

The recommended answers usually fit, so setup fills them in and shows them before it creates anything: a name, a
size measured against your machine, the manifest's harnesses and its default one, the folders it shares, how the
agent reaches GitHub and who its commits are from, and which optional values stay empty. Choose Change something to
answer each question yourself, with those answers offered. Setup still asks for what has no default, such as a
required token. It then writes `~/agents/<name>/` with the manifest, the files it reads and a `.env`, and creates the
agent. The review also offers its `agent.yaml` in your editor, for anything the questions leave at its recommended
value, such as an exact size, the model or hosts the agent may not reach. Change the folder and run `agentctl apply` in
it later.

- **Shared folders** are folders on your machine that you and the agent both work on, so what it changes lands in
  them. Without one, the agent works on its own copies, which you reach over SSH or in an editor. Setup lists what the
  manifest shares and lets you keep it, share another checkout or folder (`--mount`), or leave one out (`--unshare`).
- **Sign-ins** run in a short-lived sandbox, so this machine needs no harness or `gh`. A harness login agentctl holds
  already, from `agentctl claude login` or an earlier setup, is used for every agent; setup asks whether to use it or
  sign in again (`--sign-in again`). A GitHub account is per agent, since each keeps its token in its own `.env`, so
  setup offers the account another agent in `~/agents` uses (`--github existing`), after checking its token still
  works.
- **Agents you set up before** are offered first when you run setup again: connect a running one to an editor, sign
  in to its harnesses, or replace it with a new definition, which setup asks about first. For a stopped agent or one
  that is not ready, setup names the `agentctl` command that helps.
- **Editors:** setup opens the agent in VS Code or Zed. Other apps that work over SSH, such as JetBrains Gateway and
  the Claude desktop app's Code tab, connect to the SSH host `agentctl-<name>` that setup names, in the folder it
  names. The Claude desktop app copies its own Claude sign-in into the agent, while sessions agentctl starts never let
  a real credential in.
- **Questions** take the arrow keys and Enter, and Esc goes back to the question before. Every question has a flag,
  and setup ends by printing the command that repeats it without questions, so a script runs the same setup without
  a terminal:

```sh
agentctl setup --non-interactive --yes --archetype dotnet-backend-developer --name api --github-token-stdin < token.txt
```

To write an agent of your own, start from the [.NET example](../agents/examples/dotnet-dev/), an [agent definition](../GLOSSARY.md) whose manifest
and image explain each line. Let an agent make it yours: ask Claude Code or Codex to copy it and change it for the
tools and repositories your work needs. A coding agent working in an agent can apply what it writes as a nested agent
and try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
