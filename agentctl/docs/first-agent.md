# Your first agent, from an empty folder to your editor

In this tutorial you give one of your repositories an agent: a computer of its own where Claude Code works on the
repository, and that you open in your editor as if it were a remote machine. You start from an empty folder and end
with the repository open in VS Code, running inside the agent. It takes about half an hour, most of it waiting for the
first image build.

The example is a .NET repository called `myapp` on GitHub, owned by `OWNER`. Use one of your own and put its owner and
name wherever `OWNER` and `myapp` appear. The commands work the same in a POSIX shell and in PowerShell.

## Before you start

You need:

- `agentctl`, installed and logged in to Claude Code, as [the README](../README.md#install) describes.
- Docker Engine, or Docker Desktop, running on this machine. The agent's image is built here.
- A repository of yours on GitHub.
- A [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) with `Contents` and
  `Pull requests` read and write on that repository.
- VS Code with the [Remote - SSH](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-ssh)
  extension. To use a JetBrains IDE instead, finish the tutorial up to "Open the repository in VS Code", then follow
  [Open an agent in a JetBrains IDE](open-in-jetbrains.md).
- Room on this machine for what the example's manifest asks for under `resources`, such as its CPUs and memory.

## Clone the repository

Start in an empty folder and clone your repository:

```sh
mkdir ~/code
cd ~/code
git clone https://github.com/OWNER/myapp
cd myapp
```

This checkout is where the agent's definition will live. The agent itself never sees it: it clones the repository
again, inside its own computer.

## Add an agent definition

An agent definition is a folder with a manifest, `agent.yaml`, and what the manifest refers to, such as a
`Dockerfile`. Copy the [.NET example](../../agents/examples/dotnet-dev/) into your repository, as `agents/dev`:

```sh
git clone --depth 1 https://github.com/digdir/digdir-agents ~/code/digdir-agents
mkdir agents
cp -r ~/code/digdir-agents/agents/examples/dotnet-dev agents/dev
cd agents/dev
```

Open `agent.yaml`. Every line has a comment that says what it does. Change the agent's name, `dotnet-dev`, to your
repository's. A name takes lowercase letters, digits and `-`, so write `MyApp` as `myapp`:

```yaml
metadata:
  # The agent's name in agentctl's commands and the TUI. `agentctl apply --name` overrides it, so one manifest can
  # create several agents.
  name: myapp
```

Open `workspace.list`, which lists the repositories the agent clones when it starts, and add yours at the end, on a
line of its own:

```text
# Repositories to clone into /home/agent/code at boot, one OWNER/NAME per line, optionally followed by a directory
# name. A repository that is already there is left alone. Cloning needs GITHUB_TOKEN, even for a public repository.
#
# Altinn/dialogporten
OWNER/myapp
```

The agent clones it to `/home/agent/code/myapp`, spelled as the line spells it.

## Give the agent its settings

The manifest reads its values from `.env` beside it. Make one from the sample:

```sh
cp .env.sample .env
```

Fill in your name and email for Git, and your token as `GITHUB_TOKEN`. You can leave `AIVAR_API_KEY` empty. The agent
clones with the GitHub CLI, which needs the token even for a public repository, so without it the agent starts with
an empty `/home/agent/code`. The token stays on this machine: the agent sees only a placeholder, and `agentctl` puts
the real token into requests to GitHub. The definition's `.gitignore` keeps `.env` out of Git.

## Start the agent

```sh
agentctl apply --wait
```

`agentctl` builds the image with Docker, starts the agent and shows the progress until the agent is ready. The first
build downloads the base image and the .NET SDK, and takes several minutes. Later applies reuse the image and take
seconds. If `apply` stops with `Claude Code login "default" is not stored`, run `agentctl claude login` and apply
again.

Check the agent:

```sh
agentctl get agents
```

```text
NAME   READY  STATUS  HARNESSES                        PROVIDER
myapp  True   Ready   claudeCode (default), codex, pi  microsandbox
```

The agent clones `myapp` while it starts, so give it a minute if a step below can't find the repository.

## Work with Claude Code in the agent

From the definition's folder, start a session called `s1`:

```sh
agentctl attach session/s1
```

Claude Code starts in `~/code`, which is `/home/agent/code` inside the agent. Ask it to look at `myapp` and run its
tests. It can use the .NET SDK, Git and `gh`, and push to GitHub with your token. If your tests target an older .NET
than the image's SDK, Claude Code finds its runtime missing. Add that runtime to the `Dockerfile` and apply again.

Press `Ctrl-b` and then `d` to leave the session, as the bar at the bottom says. The session keeps running, and
`agentctl attach session/s1` takes you back to it.

## Open the repository in VS Code

Let your SSH client reach your agents. You do this once on each machine:

```sh
agentctl ssh-config install
```

It adds an `Include` line at the top of `~/.ssh/config` and says so. Ask the agent where its project is:

```sh
agentctl ssh-info agent/myapp
```

Among the lines it prints, which also name the key and configuration files `agentctl` keeps for the agent, are:

```text
Alias:       agentctl-myapp
User:        agent
Directory:   /home/agent/code
Project:     /home/agent/code/myapp
```

`ssh agentctl-myapp` now opens a shell in the agent. Open the project in VS Code over SSH:

```sh
code --remote ssh-remote+agentctl-myapp /home/agent/code/myapp
```

The first time, VS Code installs its server in the agent, which takes a minute. Then it shows your repository. Its
terminal is a shell in the agent, and what you change there is what Claude Code sees.

## Keep the definition

Commit the definition, so that your colleagues can start the same agent with their own `.env`:

```sh
git add .
git commit -m "Add an agent definition"
```

## Stop the agent

```sh
agentctl stop agent/myapp
```

The agent keeps its disk, and `agentctl get agents` shows it as `Stopped`. `agentctl start agent/myapp` starts it
again in a few seconds, and `agentctl attach session/s1` resumes the session with its conversation.

## What you did

You added an agent definition to a repository, started an agent from it, worked in it with Claude Code and opened it
in VS Code. The agent works on its own clone, so its work reaches you through GitHub, as pushes and pull requests.

Next:

- [Open an agent in an editor](open-in-an-editor.md), such as Zed, or from the TUI.
- [Open an agent in a JetBrains IDE](open-in-jetbrains.md), such as Rider.
- `agentctl tui` shows every agent and session, and opens them with a key.
- [Label agents](label-agents.md), and [change inherited lists in a variant](variant-lists.md) of your own.
