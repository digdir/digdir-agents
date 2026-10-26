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

Log in to the harnesses your agents use:

```sh
agentctl claude login
agentctl codex login
```

Update with `agentctl self update`. Agents and sessions carry over.

## Your first agent

Start from the [.NET example](../agents/examples/dotnet-dev/), an [agent definition](../GLOSSARY.md) whose manifest
and image explain each line. Let an agent make it yours: ask Claude Code or Codex to copy it and change it for the
tools and repositories your work needs. A coding agent working in an agent can apply what it writes as a nested agent
and try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
