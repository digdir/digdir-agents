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

To keep more than one subscription, such as one for work and one of your own, store each under a name with
`agentctl claude login work`, and set `login: work` on the harness in the manifests that should use it. With
`login: [work, private]` or `login: any`, each agent gets whichever of them the fewest other running agents use, and
keeps it. `agentctl claude list` shows the logins and the agents that use each.

Give the coding agents on your machine, such as Claude Code, Codex and Pi, the skill for driving agents:

```sh
agentctl skills install --bundled agentctl --user
```

`agentctl skills install --help` shows how to install your own skills for yourself or a repository.

Update with `agentctl self update`. Agents and sessions carry over. Run the `skills install` command above again to
update the skill as well.

## Your first agent

`agentctl new` makes an agent for a repository with you. Run it in the repository's checkout, or name the checkout:

```sh
agentctl new ~/repos/my-service
```

It starts the [agent builder](../agents/agent-builder/), an agent agentctl carries, on the checkout, and you talk with
it about what the new agent is for. The builder studies the repository, writes the agent's definition into the
checkout for your team or into `~/agent-definitions` for you alone, tries it, and leaves it for you to review. It ends
with the `agentctl apply` command that runs the agent it made. The builder keeps running until you
`agentctl delete agent-builder`, so leaving before it is done and running `agentctl new` again goes back to the same
conversation. It needs your Claude Code login (`agentctl claude login`).

To write an agent by hand, start from the [.NET example](../agents/examples/dotnet-dev/), an [agent definition](../GLOSSARY.md) whose manifest
and image explain each line. Let an agent make it yours: ask Claude Code or Codex to copy it and change it for the
tools and repositories your work needs. A coding agent working in an agent can apply what it writes as a nested agent
and try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
