# agentctl

`agentctl` runs long-running Agents with durable Sessions in isolated [Sandboxes](../sandbox/), with mediated network
access and no real secrets inside the Sandbox.

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

Log in to the harnesses your Agents use:

```sh
agentctl claude login
agentctl codex login
```

Update with `agentctl self update`. Agents and Sessions carry over.

## Your first Agent

At a terminal, `attach`, `exec` and `ssh` offer to start the Agent here when it is stopped, and
`agentctl attach` without a Session opens the Agent's only Session, starts one named after its harness, such as
`claude-1`, when it has none, or asks which.

`agentctl status` shows whether agentd answers, which harness logins are stored, the current directory's Agent and a
count of every Agent, with the commands to run next. `agentctl status --json` prints the same as a JSON object whose
keys stay stable, for agents and scripts that check their own setup.

`agentctl guide` prints, from the installed version, what a coding agent needs to set up an Agent with you: how to
go about it (`agentctl guide setup`), an annotated manifest, the image, instructions, GitHub accounts and the
`status --json` object.

Start from the [.NET example](../agents/examples/dotnet-dev/), an [Agent definition](../GLOSSARY.md) whose manifest
and image explain each line. Let an agent make it yours: ask Claude Code or Codex to copy it and change it for the
tools and repositories your work needs. An agent working in an Agent can apply what it writes as a nested Agent and
try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an Agent image must provide
- [guide/](guide/): what `agentctl guide` prints, for coding agents that set up an Agent
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
