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

Update with `agentctl self update`. Agents and Sessions carry over. Stop every Agent at once with `agentctl kill --all`.

## Your first Agent

Start from the [.NET example](../agents/examples/dotnet-dev/), whose manifest and image explain each line. Copy it and
change what your work needs.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an Agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
