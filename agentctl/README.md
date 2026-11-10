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

At a terminal it asks how to get the token. Signing in through the browser as a GitHub user made for agents, such as a
bot, is the default: agents reach what that user has been given, and this machine needs no `gh`. A fine-grained token
reaches only the repositories you select in GitHub's form. Either way, sign in to GitHub as that user, such as in a
private window, and agentctl shows whom the token acts as before keeping it. `--web` and `--fine-grained` skip the
question, and `--from-stdin` takes a token instead.

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
replaces it for every agent at once, and `agentctl secret list` shows the agents that use each secret. A value that is
not secret goes in the manifest, as `value` on an entry of `spec.environment`.

agentctl stops reading an agent's environment file in a later release. [Moving off the environment
file](docs/moving-off-the-environment-file.md) says where each of its values goes.

Give the coding agents on your machine, such as Claude Code, Codex and Pi, the skill for driving agents:

```sh
agentctl skills install --bundled agentctl --user
```

`agentctl skills install --help` shows how to install your own skills for yourself or a repository.

Update with `agentctl self update`. Agents and sessions carry over. Run the `skills install` command above again to
update the skill as well.

Then bring the agent definitions in a checkout up to date with the new version:

```sh
agentctl update ~/repos/my-service
```

It runs the [agent builder](bundled/definitions/agent-builder/) on the checkout, as `agentctl new` does. The builder
reads the release notes since the version each definition was written for, proposes each change with its reason, and
writes the ones you agree to for you to review and commit. That includes moving off the environment file, for which it
gives you the `agentctl github login` and `agentctl secret set` commands to run. Running it again goes back to the same
conversation, and `agentctl update --rm` deletes the builder when the command ends instead.

An agent can bring its own definition up to date in the same way, such as when the checkout lives inside it. Ask it
to, or to run `agentctl update --print-task` there, which gives it the same task as the builder. It proposes each
change, tries the result and opens a pull request.

## Your first agent

`agentctl new` makes an agent for a repository with you. Run it in the repository's checkout, or name the checkout:

```sh
agentctl new ~/repos/my-service
```

It runs the [agent builder](bundled/definitions/agent-builder/), a definition agentd carries, on the checkout, and you
talk with it about what the new agent is for. The builder studies the repository, writes the agent's definition into
the checkout for your team or into `~/agents` for you alone, tries it, and leaves it for you to review. It ends with
the `agentctl apply` command that runs the agent it made. The builder keeps running until you delete it, so leaving
before it is done and running `agentctl new` in the same checkout again goes back to the same conversation. With
`--rm`, agentd deletes it when the command ends, such as when you leave its session. It needs your Claude Code login
(`agentctl claude login`).

To write an agent by hand, start from the [.NET example](../agents/examples/dotnet-dev/), an
[agent definition](../GLOSSARY.md) whose manifest and image explain each line. Let an agent make it yours: ask
Claude Code or Codex to copy it and change it for the tools and repositories your work needs. A coding agent working
in an agent can apply what it writes as a nested agent and try it before you do.

## Documentation

- [docs/](docs/README.md): agentctl's own docs, such as what an agent image must provide
- [architecture.md](../docs/architecture.md): the principles and diagrams
- [GLOSSARY.md](../GLOSSARY.md): the terms used here
- [CHANGELOG.md](CHANGELOG.md): release notes
- [CONTRIBUTING.md](../CONTRIBUTING.md): development
