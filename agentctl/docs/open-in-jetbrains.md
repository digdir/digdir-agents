# Open an agent in a JetBrains IDE

Open an agent's project in Rider, IntelliJ IDEA, WebStorm or another JetBrains IDE through
[JetBrains Gateway](https://www.jetbrains.com/remote-development/gateway/). Gateway runs the IDE's backend in the
agent and shows it in JetBrains Client on your machine, so the IDE indexes, builds and runs tests where the code is.

`agentctl` doesn't open Gateway for you yet ([#370](https://github.com/digdir/digdir-agents/issues/370)), so this
guide connects by hand. The steps were tried on Windows 11 with Gateway 2026.2.2 and WebStorm 2026.2.3. macOS and
Linux hosts, and Gateway through the JetBrains Toolbox App, are untested.

You need an agent with `access` of `type: ssh`, as the [.NET example](../../agents/examples/dotnet-dev/) has, and
Gateway installed on your machine.

## Give the agent room for the IDE

The backend needs about 4 GiB of memory beyond what the agent uses without it, and the agent has no swap, so an agent
short of memory loses the backend or its harness rather than slowing down. Choosing an IDE is your choice, not the
repository's, so add the memory in a variant of your own beside the repository's `agent.yaml`, such as
`agent.mine.yaml`:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.yaml
metadata:
  name: myapp
  labels:
    client: jetbrains
spec:
  sandbox:
    resources:
      memory: "12Gi"
  access:
    - type: ssh
```

Keep `metadata.name` the same as the base's, so the variant changes that agent rather than creating another. The
`client: jetbrains` label marks the agent as one you open in Gateway, for tools that list agents by client, as
[Label agents](label-agents.md) describes. The `ssh` entry adds SSH access if the base lacks it and changes nothing if
it has it. Git ignores `agent.*.yaml` in the example's definition, so the variant stays yours.

Apply it:

```sh
agentctl apply --variant mine --wait
```

## Set up SSH once

```sh
agentctl ssh-config install
```

Gateway's SSH client reads `~/.ssh/config` and the file this includes, and reaches the agent through `agentctl` from
there.

## Find the alias and the project

```sh
agentctl ssh-info agent/myapp
```

Note `Alias`, such as `agentctl-myapp`, and `Project`, such as `/home/agent/code/myapp`.

## Connect from Gateway

1. In Gateway, start a new SSH connection.
2. Enter `agent` as the username, the alias as the host and `22` as the port. Choose "Check Connection and
   Continue".
3. Choose the IDE, such as Rider for .NET, and enter `Project` as the project's path.
4. Wait while Gateway downloads the backend into the agent, about 1 to 1.6 GB, and on the first connect also
   JetBrains Client onto your machine, about 700 MB. The first connect takes two to three minutes.

JetBrains Client then shows the project. Its terminal is a shell in the agent, where `git` and `gh` use the agent's
GitHub token as they do over `agentctl ssh`.

## Accept the user agreement the first time

On a machine where JetBrains Client has never run, it asks you to accept its user agreement, and Gateway closes the
client if it hasn't connected within about 35 seconds. If that happens, start JetBrains Client once on its own and
accept the agreement there. On Windows it is in `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist\BUILD\bin`. Then
connect again from Gateway.

## Connect again

Gateway keeps the connection, so next time you choose it from Gateway's list. The agent keeps the backend in its
home, so connecting again takes about 15 seconds. Start a stopped agent first with `agentctl start agent/myapp`.

## Known limits

- **Java's trust store.** The agent's network replaces the certificates of the sites it reaches with ones from a CA
  that the JetBrains Runtime doesn't trust. Gateway's download and the Marketplace work, but code in the backend that
  uses Java's default trust store fails with `PKIX path building failed`, and so may a Gradle or Maven build that the
  IDE starts. [#396](https://github.com/digdir/digdir-agents/pull/396) proposes that `agentd` provides a trust store
  that fixes this.
- **Memory after you close the IDE.** The backend keeps running, and keeps its memory, after Gateway exits, and after
  JetBrains Client closes if you choose to keep it running.
- **Disk.** The backend takes about 3 GB in the agent's home, and JetBrains Client about 1.8 GB on your machine.
