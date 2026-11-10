# Start an agent from a repository's definition

Start an agent for a repository that already has an agent definition, such as
[Altinn Studio](https://github.com/Altinn/altinn-studio)'s in `agents/`. You clone the repository, choose a
definition, give it its settings and apply it. For a repository without a definition, follow
[the tutorial](first-agent.md).

You need `agentctl`, installed and logged in to Claude Code, as [the README](../README.md#install) describes. You need
Docker only for a definition whose image has `type: build`, which builds it on your machine. One with
`type: reference` pulls a published image.

## Clone the repository

```sh
git clone https://github.com/Altinn/altinn-studio
cd altinn-studio
```

## Choose a definition

List the definitions in the repository:

```sh
agentctl get definitions
```

```text
DEFINITION                                NEEDS FOLDERS FOR                    NAME
./agents/desktop                          -                                    altinn-desktop
./agents/desktop/agent.nested-build.yaml  -                                    altinn-desktop-nested-build
./agents/desktop/agent.nested.yaml        -                                    altinn-desktop-nested
./agents/desktop/agent.worktree.yaml      -                                    altinn-desktop-worktree
./agents/minimal                          -                                    altinn-minimal
./agents/minimal/agent.nested-build.yaml  -                                    altinn-minimal-nested-build
./agents/minimal/agent.nested.yaml        -                                    altinn-minimal-nested
./agents/minimal/agent.worktree.yaml      -                                    altinn-minimal-worktree
bundled/agent-builder                     /home/agent/code,/home/agent/agents  agent-builder
```

Each folder with an `agent.yaml` is a definition, and the files beside it are its variants. `NAME` is the agent each
one creates. The last row is a definition `agentctl` carries itself, the agent builder, which writes a definition for
a repository that has none. The repository's own docs say what each of its definitions is for, here
`agents/README.md`: `minimal` suits code and review work, and `desktop` adds containers, a browser and a desktop. This
guide starts `minimal`.

## Give it its settings

A definition gets its values from your machine in one of two ways:

- **Logins and stored secrets**, which the manifest asks for by name under `accounts` and `secrets`, as the agent
  builder writes them. Store each once, such as with `agentctl github login default`, as
  [Give an agent GitHub access](give-an-agent-github-access.md) describes. `apply` names any that are missing.
- **An environment file, `.env`,** beside the manifest, as Altinn Studio's definitions use today. `agentctl` stops
  reading it in a later release, as [Moving off the environment file](moving-off-the-environment-file.md) explains.

Altinn Studio's `.env.sample` names the values its manifest reads. Copy it to `.env` beside the manifest and fill it
in:

```sh
cd agents/minimal
cp .env.sample .env
```

The manifest says which secrets are `optional`, and those can stay empty. `GITHUB_TOKEN` is required here. If a
required value is missing, `apply` names it and leaves the agent `Failed` until you add the value and apply again:

```text
agentctl: .env does not define required variable "GITHUB_TOKEN"
```

To share one file between several definitions, keep it outside the checkout and name it on every apply, such as
`agentctl apply --env-file ~/altinn.env --wait`.

## Start the agent

From the definition's folder:

```sh
agentctl apply --wait
```

The first apply pulls the image, which takes a minute or more. Check the agent:

```sh
agentctl get agents
```

```text
NAME            READY  STATUS  HARNESSES                        PROVIDER
altinn-minimal  True   Ready   claudeCode (default), codex, pi  microsandbox
```

The image lists the repositories the agent clones, so the agent has its own clone of Altinn Studio a little after it
is ready. `ssh-info` names it as the project:

```sh
agentctl ssh-info agent/altinn-minimal
```

```text
Alias:       agentctl-altinn-minimal
User:        agent
Directory:   /home/agent/code
Project:     /home/agent/code/altinn-studio
```

## Work in it

- `agentctl attach session/s1` from the definition's folder starts Claude Code in the agent.
- [Open an agent in an editor](open-in-an-editor.md), such as VS Code or Zed.
- [Open an agent in a JetBrains IDE](open-in-jetbrains.md), which adds the memory it needs in a variant of your own.

## Use a variant

`agentctl apply --variant nested --wait` applies `agent.nested.yaml` instead. A variant with a name of its own, as
all of Altinn Studio's have, creates another agent beside `altinn-minimal`. Variants of your own, such as
`agent.mine.yaml`, stay out of Git when the definition's `.gitignore` lists `agent.*.yaml`, as Altinn Studio's does.

## Remove it

```sh
agentctl delete agent/altinn-minimal
```

This deletes the agent and its disk, with everything in its home that you haven't pushed.
