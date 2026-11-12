# Your first agent, from a repository to your editor

In this tutorial you give one of your repositories an agent: a computer of its own where Claude Code works on the
repository, and that you open in your editor as if it were a remote machine. You let the agent builder write the
agent's definition with you, start the agent from it, and end with the repository open in VS Code, running inside the
agent. Plan on an hour, most of it the agent builder trying what it wrote and the first image build.

The example is [Altinn/altinn-profile](https://github.com/Altinn/altinn-profile), a .NET API with PostgreSQL and no
agent definition of its own. The output below is from a run on it. Use one of your repositories and put its owner and
name wherever `Altinn` and `altinn-profile` appear. The commands work the same in a POSIX shell and in PowerShell.

## Before you start

You need:

- `agentctl`, installed as [the README](../README.md#install) describes.
- Docker Engine, or Docker Desktop, running on this machine. Until your team's CI publishes the agent's image, it is
  built here.
- A repository on GitHub, and a GitHub account that can push to it.
- VS Code with the [Remote - SSH](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-ssh)
  extension. To use a JetBrains IDE or Zed instead, finish the tutorial up to "Open the repository in VS Code", then
  follow [Open an agent in a JetBrains IDE](open-in-jetbrains.md) or [Open an agent in an editor](open-in-an-editor.md).
- Room on this machine for two agents: the agent builder takes 4 CPUs and 8 GiB of memory while it runs, and the
  example's agent 2 CPUs and 4 GiB.

## Log in once

Agents run Claude Code with your subscription. Store your login:

```sh
agentctl claude login
```

It opens your browser to approve a long-lived token, and ends with `claude login default stored`.

Agents clone, commit and open pull requests as a GitHub account, through a login stored under a name. Manifests ask for
it by that name, and the agent builder writes manifests that ask for `default`:

```sh
agentctl github login default
```

It asks whose repositories the token will reach, opens GitHub's form for a fine-grained token with the rest filled
in, and reads the token you paste. Answer with the owner of your repository, such as `Altinn`, and choose the
repository under "Repository access". A fine-grained token reaches only repositories its owner owns, so a token owned
by your own account can read a public Altinn repository but never push to it. When the token checks out, `agentctl`
says who the agents act as:

```text
  Login:    default
  Acts as:  your-account (Your Name <12345678+your-account@users.noreply.github.com>)
  Kind:     fine-grained
  Expires:  2027-02-07
Agents asking for the login default now use GitHub as your-account.
```

The token stays on this machine. The agent sees only a placeholder, and `agentctl` puts the real token into requests
to GitHub. [Give an agent GitHub access](give-an-agent-github-access.md) covers organisations, other kinds of token
and logging out.

## Clone the repository

```sh
mkdir ~/repos
cd ~/repos
git clone https://github.com/Altinn/altinn-profile
cd altinn-profile
```

This checkout is where the agent's definition will live. The agent itself works on a clone of its own, inside its
computer.

## Let the agent builder write the definition

```sh
agentctl new
```

`agentctl new` starts the agent builder, an agent `agentctl` carries, with your checkout mounted, and connects you to
a Claude Code session in it:

```text
Starting the agent builder on /home/you/repos/altinn-profile.
  ✓ Pull OCI image (1.0s)
✓ Resolve sandbox image (1.1s)
  ✓ Create Microsandbox VM (3.7s)
✓ Start sandbox (3.8s)
```

The builder reads the repository and its CI, then asks what the agent is for. In the run on altinn-profile it asked
about:

1. **The role**, from what it found: backend developer, tester, reviewer or documentation writer.
2. **What "done" means**: build and test as CI does and open a pull request, or also run the API and its k6 and Bruno
   tests.
3. **Whose agent it is**: the team's, in `agents/` in the repository with an image CI publishes, or yours alone, in
   `~/agents`.
4. **Other repositories** the agent should have cloned beside this one.
5. **Services and secrets**: the GitHub login, and any keys for test services, with the hosts each may reach.
6. **The plan**: the tools and the versions it pins, the checks and what each needs, the size, and where the files go.

Answer for your repository. Then the builder writes the definition, builds its image and tries it in an agent of its
own, nested inside the builder. That takes a while: in the example, half an hour. It ends with a report of what it
verified, such as:

```text
- The image builds and passes `agent-image-check`.
- `dotnet build` has 0 warnings and 0 errors.
- `profile-db up` applied 8 migrations.
- The full `dotnet test` (Release, with coverage) passed: 941 passed, 0 failed.
- The API serves `/health` and `/swagger` with 200 OK.
```

and what it could not verify, and why. Read that part: it is what you check next.

You can leave the session with `Ctrl-b` and then `d` while the builder works. When you leave, `agentctl new` says
how to come back and how to remove the builder:

```text
The agent builder keeps running, and `agentctl new` in /home/you/repos/altinn-profile goes back to it.
Apply the agent it made with `agentctl apply -f` and the manifest it named. Run `agentctl delete agent-builder-altinn-profile` once you are done with it.
```

## Look at what it wrote

Nothing is committed. For a team's agent, the builder writes a folder under `agents/` and a workflow that publishes its
image:

```text
.github/workflows/agent-backend-developer.yml
agents/backend-developer/Dockerfile
agents/backend-developer/common/profile-db
agents/backend-developer/common/workspace.list
agents/backend-developer/instructions.md
agents/backend-developer/improving.md
agents/backend-developer/skills/pr-evidence/SKILL.md
agents/backend-developer/minimal/agent.yaml
agents/backend-developer/minimal/agent.nested.yaml
agents/backend-developer/minimal/agent.nested-build.yaml
agents/backend-developer/minimal/agent.worktree.yaml
agents/backend-developer/minimal/environment.md
agents/backend-developer/minimal/.gitignore
```

`minimal/agent.yaml` is the manifest. Its comments say what each part is for. Its image is the one CI publishes once
the definition is merged, so until then you start the `nested-build` variant beside it, which builds the image from
your checkout instead.

## Start the agent

From the stage's folder:

```sh
cd agents/backend-developer/minimal
agentctl apply --variant nested-build --wait
```

`agentctl` builds the image with Docker, starts the agent and shows the progress until it is ready:

```text
agent/profile-backend-developer-nested-build applied
  ✓ Build Docker image (2m 23s)
  ✓ Export Docker image (35.0s)
  ✓ Materialize image layers (4.8s)
  ✓ Assemble root disk (505ms)
  ✓ Import Microsandbox image (6.8s)
✓ Resolve sandbox image (3m 06s)
✓ Create sandbox (114ms)
  ✓ Materialize direct root image (16.6s)
  ✓ Create Microsandbox VM (6.1s)
✓ Start sandbox (22.8s)
  ✓ Prepare workspace and Podman (1.6s)
✓ Set up agent (2.5s)
✓ Configure SSH access (979ms)
agent/profile-backend-developer-nested-build ready
```

Later applies reuse the image and take seconds. Check the agent:

```sh
agentctl get agents
```

```text
NAME                                    READY  STATUS  HARNESSES                    PROVIDER
profile-backend-developer-nested-build  True   Ready   claudeCode (default), codex  microsandbox
```

The agent clones the repository with the GitHub login a few seconds after it is ready. Check that it did:

```sh
agentctl ssh agent/profile-backend-developer-nested-build -- git -C code/altinn-profile log -1 --oneline
```

```text
5b9fc37 Extend db col notification_name 200->254 (#1090)
```

## Work with Claude Code in the agent

Start a session called `s1`:

```sh
agentctl attach session/s1 --agent profile-backend-developer-nested-build
```

Claude Code starts in `~/code`, which is `/home/agent/code` inside the agent, with the instructions the builder wrote.
Ask it to build `altinn-profile` and run its tests. It commits as the GitHub login's account, and pushes and opens pull
requests where that account may.

Press `Ctrl-b` and then `d` to leave the session, as the bar at the bottom says. The session keeps running, and the
same `attach` command takes you back to it.

## Open the repository in VS Code

Let your SSH client reach your agents. You do this once on each machine:

```sh
agentctl ssh-config install
```

It adds an `Include` line at the top of `~/.ssh/config` and says so. Ask the agent where its project is:

```sh
agentctl ssh-info agent/profile-backend-developer-nested-build
```

Among the lines it prints, which also name the key and configuration files `agentctl` keeps for the agent, are:

```text
Alias:       agentctl-profile-backend-developer-nested-build
User:        agent
Directory:   /home/agent/code
Project:     /home/agent/code/altinn-profile
```

`ssh agentctl-profile-backend-developer-nested-build` now opens a shell in the agent. Open the project in VS Code:

```sh
code --remote ssh-remote+agentctl-profile-backend-developer-nested-build /home/agent/code/altinn-profile
```

The first time, VS Code asks two things. On Windows it asks for the platform of the remote host: choose Linux. Then it
asks whether you trust the authors of the files in the folder. Answer straight away: VS Code connects only once you
have, and gives up after about 17 seconds with "Connecting with SSH timed out". If it did, choose Retry. VS Code then
installs its server in the agent, about 700 MB of the agent's disk, and shows your repository on the branch the agent
cloned. Its terminal is a shell in the agent, and what you change there is what Claude Code sees.

## Propose the definition

The definition is your team's once it is merged. Commit it on a branch and open a pull request:

```sh
cd ~/repos/altinn-profile
git switch -c add-backend-developer-agent
git add agents .github/workflows/agent-backend-developer.yml
git commit -m "Add a backend developer agent"
```

[Onboard a repository](onboard-a-repository.md) says what to check before the pull request, and what your teammates
run once it is merged.

## Clean up

Delete the agent builder once you are done with it, with the command `agentctl new` printed:

```sh
agentctl delete agent-builder-altinn-profile
```

Stop the agent you started:

```sh
agentctl stop agent/profile-backend-developer-nested-build
```

The agent keeps its disk, and `agentctl get agents` shows it as `Stopped`. VS Code loses the connection and offers to
reconnect. `agentctl start agent/profile-backend-developer-nested-build` starts it again in a few seconds, and the
`attach` command resumes the session with its conversation.

## What you did

You let the agent builder write an agent definition for a repository, started an agent from it, worked in it with
Claude Code and opened it in VS Code. The agent works on its own clone, so its work reaches you through GitHub, as
pushes and pull requests.

Next:

- [Onboard a repository](onboard-a-repository.md), from the builder's draft to an agent your whole team starts.
- [Open an agent in an editor](open-in-an-editor.md), such as Zed, or from the TUI.
- [Open an agent in a JetBrains IDE](open-in-jetbrains.md), such as Rider.
- [Use an agent from T3 Code](use-an-agent-from-t3-code.md).
- [Start an agent from a repository's definition](start-from-a-definition.md), when the repository already has one.
- `agentctl tui` shows every agent and session, and opens them with a key.
