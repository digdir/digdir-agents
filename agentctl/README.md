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

Give agents a GitHub account with a login kept under a name, and set `accounts: [{type: github, login: NAME}]` in the
manifests that should use it:

```sh
agentctl github login altinn-studio
```

At a terminal it opens GitHub's form for a fine-grained token, filled in except for the repositories. Sign in there as
the account agents should act as, such as in a private window. `--from-stdin` takes a token instead, such as
`gh auth token`, which lets agents act as you everywhere you can reach.

Store a secret that agents use, such as an API key, under a name with the hosts its value may reach, and ask for it
by that name in the manifests that use it:

```sh
agentctl secret set aivar --hosts api.aivar.no
```

```yaml
secrets:
  - name: aivar
    environment: AIVAR_API_KEY
```

The value only reaches the secret's hosts, and `allowedHosts` on the entry narrows them further. Setting it again
replaces it for every agent at once, and `agentctl secret list` shows the agents that use each secret.

Give the coding agents on your machine, such as Claude Code, Codex and Pi, the skill for driving agents:

```sh
agentctl skills install --bundled agentctl --user
```

`agentctl skills install --help` shows how to install your own skills for yourself or a repository.

Update with `agentctl self update`. Agents and sessions carry over. Run the `skills install` command above again to
update the skill as well.

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
