# Open an agent in a JetBrains IDE

Open an agent's project in Rider, IntelliJ IDEA, WebStorm or another JetBrains IDE through
[JetBrains Gateway](https://www.jetbrains.com/remote-development/gateway/). Gateway runs the IDE's backend in the
agent and shows it in JetBrains Client on your machine, so the IDE indexes, builds and runs tests where the code is.

`agentctl` doesn't open Gateway for you yet ([#370](https://github.com/digdir/digdir-agents/issues/370)), so this
guide connects by hand. [#386](https://github.com/digdir/digdir-agents/issues/386) proposes `agentctl connect`, one
command that opens an agent in any client, Gateway among them. The steps were tried with Gateway 2026.3 and Rider
2026.3 on Linux, and with Gateway 2026.2.2 and WebStorm 2026.2.3 on Windows 11. macOS, and Gateway through the
JetBrains Toolbox App, are untested.

You need an agent with `access` of `type: ssh`, and Gateway installed on your machine. The example is the agent from
[the tutorial](first-agent.md), `profile-backend-developer-nested-build`, started from the `nested-build` variant of
the definition the agent builder wrote for altinn-profile.

## Give the agent room for the IDE

The backend wants at least 4 CPUs, and Gateway warns that "The host doesn't meet the minimal requirements" with fewer.
Rider's backend took about 5 GiB of memory while it opened altinn-profile, and the agent has no swap, so an agent
short of memory loses the backend or its harness rather than slowing down. It also needs disk: about 8 GB for Rider's
backend in the agent's home. Choosing an IDE is your choice, not the
repository's, so add the room in a variant of your own beside the definition's manifests, such as
`agents/backend-developer/minimal/agent.mine.yaml`:

```yaml
apiVersion: agents.platform/v1alpha1
kind: AgentVariant
extends: agent.nested-build.yaml
metadata:
  name: profile-backend-developer-nested-build
  labels:
    client: jetbrains
spec:
  sandbox:
    resources:
      cpu: "4"
      memory: "8Gi"
```

`extends` names the manifest the variant changes, here the `nested-build` variant the agent was started from. Keep
`metadata.name` the same as that one's, so the variant changes that agent rather than creating another. The
`client: jetbrains` label marks the agent as one you open in Gateway, for tools that list agents by client, as
[Label agents](label-agents.md) describes. The definition's `.gitignore` lists `agent.*.yaml` apart from the
definition's own variants, so the variant stays yours.

Apply it:

```sh
agentctl apply --variant mine --wait
```

`agentd` restarts the agent with the new size, so an editor connected to it loses the connection until the agent is
ready again. The agent now has the label:

```sh
agentctl get agents -L client
```

```text
NAME                                    READY  STATUS  HARNESSES                    PROVIDER      CLIENT
profile-backend-developer-nested-build  True   Ready   claudeCode (default), codex  microsandbox  jetbrains
```

Apply with `--variant mine` from now on. Applying the variant it extends again takes the room and the label away, and
restarts the agent with less.

## Set up SSH once

```sh
agentctl ssh-config install
```

Gateway's SSH client reads `~/.ssh/config` and the file this includes, and reaches the agent through `agentctl` from
there.

## Find the alias and the project

```sh
agentctl ssh-info agent/profile-backend-developer-nested-build
```

Note `Alias` and `Project`. Among the lines it prints are:

```text
Alias:       agentctl-profile-backend-developer-nested-build
User:        agent
Directory:   /home/agent/code
Project:     /home/agent/code/altinn-profile
```

## Connect from Gateway

1. The first time Gateway starts, it asks you to accept its user agreement, and whether to send usage statistics.
2. Under "SSH Connection", choose "New Connection".
3. Enter `agent` as the username, the alias as the host and `22` as the port. Choose "Check Connection and Continue".
4. Gateway asks you to confirm the agent's host key, as `[agentctl-NAME]:0`. Choose OK at once: the question counts
   against Gateway's 30-second limit for setting up the connection, and if it runs out, the check fails with
   `Timeout expired: 30000 MILLISECONDS`. Check the connection again, and answer faster. Gateway remembers the key
   once the check succeeds. It asks because it doesn't read the `HostKeyAlias` that `agentctl` looks the key up by.
5. Choose the IDE, such as Rider for .NET, and the project. Rider asks for a solution file, such as
   `/home/agent/code/altinn-profile/Altinn.Profile.sln`. Other IDEs take the folder `ssh-info` names as `Project`.
6. Choose "Download IDE and Connect". On the first connect Gateway downloads JetBrains Client onto your machine,
   787 MB for 2026.3, and the backend into the agent, 2.7 GB for Rider 2026.3, and starts the backend.
7. JetBrains Client asks you to accept its user agreement the first time, and has 35 seconds to connect, as the next
   section explains.
8. The IDE asks whether to trust the project, such as "Trust and Open 'Altinn.Profile' Solution?". Trust it to let it
   build and run the project's scripts in the agent.
9. A paid IDE, such as Rider, asks you to activate a license: log in to your JetBrains account, or enter an activation
   code or a license server.

JetBrains Client then shows the project. Its terminal is a shell in the agent, where `git` and `gh` use the agent's
GitHub login as they do over `agentctl ssh`.

## Accept the user agreement the first time

On a machine where JetBrains Client has never run, it asks you to accept its user agreement, and Gateway closes the
client if it hasn't connected within 35 seconds, with the agreement still open. Accepting it in time isn't enough
either: Gateway closes the client before it saves the answer, and it asks again on the next connect. Instead, start
JetBrains Client once on its own and accept the agreement there. It is in the `bin` folder of the client Gateway
downloaded:

- On Windows: `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist\BUILD\bin`.
- On Linux: `~/.cache/JetBrains/JetBrainsClientDist/BUILD/JetBrainsClient-VERSION/bin/jetbrains_client.sh`.

Close it, and connect again from Gateway, with the host's "+" under "Recent SSH Projects". Gateway reuses the backend
it installed.

## Connect again

Gateway keeps the connection, so next time you choose it from Gateway's list. The agent keeps the backend in its
home, so connecting again takes about 15 seconds. Start a stopped agent first, with
`agentctl start agent/profile-backend-developer-nested-build`.

## Known limits

- **Java's trust store.** The agent's network replaces the certificates of the sites it reaches with ones from a CA
  that the JetBrains Runtime doesn't trust. Gateway's download and the Marketplace work, but code in the backend that
  uses Java's default trust store fails with `PKIX path building failed`, and so may a Gradle or Maven build that the
  IDE starts. [#396](https://github.com/digdir/digdir-agents/pull/396) proposes that `agentd` provides a trust store
  that fixes this.
- **Memory after you close the IDE.** The backend keeps running, and keeps its memory, after Gateway exits, and after
  JetBrains Client closes if you choose to keep it running.
- **Gateway 2026.3 on Windows.** On a Windows 11 machine where 2026.2.2 worked, Gateway 2026.3 stopped at start with
  `Internal error` and `Unable to establish loopback connection`, for its `.port` file in
  `%LOCALAPPDATA%\JetBrains\JetBrainsGateway2026.3`. Endpoint protection that filters the `AppData` tree can block the
  local sockets Gateway uses there.
- **Disk.** Rider's backend takes about 8 GB in the agent's home, and WebStorm's about 3 GB. JetBrains Client takes
  about 2.3 GB on your machine.
