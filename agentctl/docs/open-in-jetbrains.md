# Open an agent in a JetBrains IDE

Open an agent's project in Rider, IntelliJ IDEA, WebStorm or another JetBrains IDE through
[JetBrains Gateway](https://www.jetbrains.com/remote-development/gateway/). Gateway runs the IDE's backend in the
agent and shows it in JetBrains Client on your machine, so the IDE indexes, builds and runs tests where the code is.

`agentctl` doesn't open Gateway for you yet ([#370](https://github.com/digdir/digdir-agents/issues/370)), so this
guide connects by hand. [#386](https://github.com/digdir/digdir-agents/issues/386) proposes `agentctl connect`, one
command that opens an agent in any client, Gateway among them. The steps were tried with Gateway 2026.3 and Rider
2026.3 on Linux, with Gateway 2026.3 and a licensed Rider 2026.2.4.1 on Windows 11, opened with a link, and with
Gateway 2026.2.2 and WebStorm 2026.2.3 on Windows 11. macOS, and Gateway through the JetBrains Toolbox App, are
untested.

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

## Let Gateway find the host key

Gateway reads the alias's `ProxyCommand`, key and `UserKnownHostsFile` from the included config, but not its
`HostKeyAlias`, so it doesn't find the key `agentctl` recorded for the agent. It then asks you to confirm the key, as
`[agentctl-NAME]:0`, and the question counts against its 30-second limit for setting up the connection. Add the
agent's key under that name to the file `ssh-info` names as `Known hosts`, and Gateway connects without asking. The
key is on the line that starts with the agent's ID:

```sh
agentctl ssh-info agent/profile-backend-developer-nested-build
grep "^agent-AGENT-ID " KNOWN-HOSTS-FILE | sed "s/^[^ ]*/[agentctl-profile-backend-developer-nested-build]:0/" >> KNOWN-HOSTS-FILE
```

Put the agent's ID, which `ssh-info` prints as `Agent ID`, and the `Known hosts` path in place of `AGENT-ID` and
`KNOWN-HOSTS-FILE`. `agentd` leaves the line alone when it rewrites the file. An agent that is created again gets a new
key, so replace the line then. [#370](https://github.com/digdir/digdir-agents/issues/370) proposes that `agentctl`
writes it.

## Connect from Gateway

1. The first time Gateway starts, it asks you to accept its user agreement, and whether to send usage statistics.
2. Under "SSH Connection", choose "New Connection".
3. Enter `agent` as the username, the alias as the host and `22` as the port. Choose "Check Connection and Continue".
4. Without the line above, Gateway asks you to confirm the agent's host key. Choose OK at once: if the 30 seconds run
   out, the check fails with `Timeout expired: 30000 MILLISECONDS`. Check the connection again, and answer faster.
   Gateway remembers the key once the check succeeds.
5. Choose the IDE, such as Rider for .NET, and the project. Rider asks for a solution file, such as
   `/home/agent/code/altinn-profile/Altinn.Profile.sln`. Other IDEs take the folder `ssh-info` names as `Project`.
6. Choose "Download IDE and Connect". On the first connect Gateway downloads JetBrains Client onto your machine,
   787 MB for 2026.3, and the backend into the agent, 2.7 GB for Rider 2026.3, and starts the backend.
7. JetBrains Client asks you to accept its user agreement the first time, and has 35 seconds to connect, as the next
   section explains.
8. The IDE asks whether to trust the project, such as "Trust and Open 'Altinn.Profile' Solution?". Trust it to let it
   build and run the project's scripts in the agent.
9. A paid IDE, such as Rider, uses the license of that IDE on your machine. Where it has none, it asks you to activate
   one: log in to your JetBrains account, or enter an activation code or a license server.

JetBrains Client then shows the project. Its terminal is a shell in the agent, where `git` and `gh` use the agent's
GitHub login as they do over `agentctl ssh`. While the backend analyzes the solution for the first time, the agent's
CPUs are busy and the client may show "Host is unresponsive" for a minute.

## Open it with a link

A `jetbrains-gateway://` link opens the project in one step, as a client of your own can. Gateway's SSH links only
open a backend that is already installed in the agent, so install it first, at the version your license covers. The
[JetBrains releases API](https://data.services.jetbrains.com/products/releases?code=RD&type=release&latest=true)
gives the Linux download for a product code, such as `RD` for Rider. In the agent:

```sh
dist=~/.cache/JetBrains/RemoteDev/dist/JetBrains.Rider-2026.2.4.1
url=https://download.jetbrains.com/rider/JetBrains.Rider-2026.2.4.1.tar.gz
curl -4 -fsSL -o /tmp/rider.tar.gz "$url"
echo "$(curl -4 -fsSL "$url.sha256" | cut -d' ' -f1)  /tmp/rider.tar.gz" | sha256sum -c -
mkdir -p "$dist" && tar -xzf /tmp/rider.tar.gz -C "$dist" --strip-components=1 && rm /tmp/rider.tar.gz
```

`-4` works around [#452](https://github.com/digdir/digdir-agents/issues/452). Rider 2026.2.4.1 is a 2.3 GB download
and 6.7 GB unpacked, and took under 4 minutes. Then open the link on your machine, with the path escaped:

```text
jetbrains-gateway://connect#type=ssh&deploy=false&host=agentctl-profile-backend-developer-nested-build&port=22&user=agent&idePath=%2Fhome%2Fagent%2F.cache%2FJetBrains%2FRemoteDev%2Fdist%2FJetBrains.Rider-2026.2.4.1&projectPath=%2Fhome%2Fagent%2Fcode%2Faltinn-profile%2FAltinn.Profile.sln
```

The first time, Gateway shows "Connect to SSH" with the username, host and port filled in. Choose "Check Connection
and Continue". It then starts the backend, downloads the matching JetBrains Client, and opens the project, in about 2
minutes. A link with `deploy=true` fails with "No ssh specified in link; this type of links is not implemented yet".

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
- **Gateway stuck at start on Windows.** Gateway keeps a socket file, `.port`, in
  `%LOCALAPPDATA%\JetBrains\JetBrainsGateway2026.3`. One left from an earlier run that can't be reached, or removed,
  makes Gateway hang at start behind a hidden `Internal error` dialog, and a second Gateway report
  `Unable to establish loopback connection` or `still running and does not respond`. Start Gateway with a system
  folder of its own: an `idea.properties` file with `idea.system.path=C:/some/folder`, named in the environment
  variable `GATEWAY_PROPERTIES`. The same host's Rider and JetBrains Client then say "Cannot start internal HTTP
  server", and the project still works.
- **Disk.** Rider's backend takes about 8 GB in the agent's home, and WebStorm's about 3 GB. JetBrains Client takes
  about 2.3 GB on your machine.
