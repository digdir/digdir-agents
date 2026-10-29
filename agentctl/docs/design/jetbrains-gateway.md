# JetBrains Gateway on an agent

Status: proposal, from a spike against `agentctl` v0.1.0-preview.13, JetBrains Gateway 2026.2.2 and a WebStorm
2026.2.3 backend on Windows 11.

JetBrains Gateway runs a JetBrains IDE's backend on an SSH host and shows it in JetBrains Client on the user's machine.
An agent with `access: ssh` is such a host already: Gateway connects through `agentctl ssh-proxy` unchanged, and the
IDE's terminal, `git` and `gh` work on the agent's mediated secrets. What is missing is the wiring. Today a user takes
seven steps, two of them in Gateway's UI and one against a 35-second timer, before the IDE shows the project. This
note proposes how `agentctl` and the base images take those steps, in a way the proposal for the Claude app's Code
tab, `host-app-over-ssh.md`, can share.

## The flow we want

```sh
agentctl connect saman --client gateway
```

Gateway opens, and JetBrains Client shows the agent's project in the IDE its image provides, with no dialog. On an
image with the backend installed, the first connect from a host takes about a minute and a half, most of it JetBrains
Client's own download to the host. Later ones take about 15 seconds.

## What a user does today

With the agent applied and Ready, on a host with Gateway installed:

| Step | Where | Notes |
| --- | --- | --- |
| `agentctl ssh-config install` | Terminal | Once per host. |
| Find the alias and the project | Terminal | `agentctl ssh-info <agent>` prints `agentctl-<agent>` and `/home/agent/code`. The checkout is below it. |
| Add the SSH connection | Gateway | User `agent`, the alias as host, port 22, then "Check Connection and Continue". |
| Choose the IDE and the project | Gateway | Product, version and the project's path in the agent. The spike skipped this page with the links below. |
| Wait for the backend | Gateway | Gateway downloads it into the agent: 1.15 GB for WebStorm in 38 seconds, unpacked in 31, running 10 later. |
| Wait for JetBrains Client | Gateway | Once per host and IDE build: a 682 MB download to the host, about 70 seconds with unpacking. |
| Accept the user agreement | JetBrains Client | Once per host. Gateway kills the client if it has not connected within about 35 seconds, which is too short to read the agreement. Starting JetBrains Client once on its own, from `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist\<build>\bin`, avoids the timer. |

Gateway connects through the generated configuration by itself. Its SSH client reads `~/.ssh/config`, follows the
`Include`, and takes the agent's `ProxyCommand`, `IdentityFile` and `UserKnownHostsFile`. It then uploads a worker
over SFTP, starts the backend over SSH and forwards the backend's port over the same connection, all of which the
agent's [SSH server](../../src/sandbox/platform/linux/ssh.rs) allows.

## How Gateway can be driven

**Connections.** Gateway keeps its SSH connections in `options/sshConfigs.xml` in its configuration directory,
`%APPDATA%\JetBrains\JetBrainsGateway<version>` on Windows. An entry that uses the OpenSSH configuration needs only
the alias:

```xml
<sshConfig id="agentctl-saman" authType="OPEN_SSH" useOpenSSHConfig="true" host="agentctl-saman" port="22"
           username="agent" nameFormat="CUSTOM" customName="saman (agentctl)" />
```

Gateway accepts an `id` that is not a UUID, so `agentctl` can own the entries whose `id` starts with `agentctl-`, as it
owns the `Include` line and, in the Claude app, its `ssh_configs.json` entries. The configuration directory's name
carries Gateway's major version, so `agentctl` has to find the current one. The spike wrote the entry while Gateway
was closed.
Whether Gateway sees, or keeps, an entry written while it runs is untested.

**Links.** Gateway registers `jetbrains-gateway://` with the operating system, so opening a link starts Gateway or
hands it to the running one. That is how `agentctl` already opens VS Code without its launcher. Two forms connect to
an agent, and with a matching connection entry neither shows a dialog:

| Link | What Gateway does |
| --- | --- |
| `jetbrains-gateway://connect#type=ssh&ssh=<entry id>&deploy=true&productCode=WS&buildNumber=<build>&projectPath=<path>` | Downloads that build into `~/.cache/JetBrains/RemoteDev/dist` unless it is there already, starts it and opens the project. |
| `jetbrains-gateway://connect#type=ssh&host=<alias>&port=22&user=agent&deploy=false&idePath=<backend>&projectPath=<path>` | Starts the backend installed at `idePath`, or joins it if it runs already. Without an entry for that host, user and port, Gateway first shows its "Connect to SSH" dialog. |

`deploy=true` refuses a link without `ssh`. A backend that is still running, as it is after Gateway exits or when the
user keeps it on closing the client, is joined again in about 15 seconds.

**Toolbox.** Gateway now points users to the JetBrains Toolbox App for remote development. Toolbox 3.9 keeps SSH
environments in `%LOCALAPPDATA%\JetBrains\Toolbox\plugins\ssh\settings.json` and imports hosts from `~/.ssh/config`
by itself. Its SSH plugin also handles a link with the host, port and user (`h`, `p`, `u`) and hints for the project
and IDE, and asks the user before it adds the environment. The spike found neither the URL that reaches that handler
nor whether Toolbox follows the `Include`.

## What the agent must provide

**The project's path.** `agentctl` knows only the platform's working directory, `/home/agent/code`. An image that
clones repositories names them in `/etc/agent-workspace.d/*.list`, where each line's destination below
`/home/agent/code` defaults to the repository's name. When there is one, it is the project to open.

**A trust store with the mediated CA.** The mediated network intercepts every TLS connection, with a CA that is
generated for each host and that Microsandbox appends to the guest's system bundle at boot (see
[`execution.rs`](../../../sandbox/microsandbox/src/execution.rs)). Gateway's worker, which downloads the backend, and
the IntelliJ platform's own HTTPS client read the system bundle, so the download and the Marketplace work. The
JetBrains Runtime's `cacerts` does not have the CA, and the runtime cannot read the PEM bundle as a trust store. Code
in the backend that uses the JVM's default trust store fails with `PKIX path building failed`, as one of WebStorm's
remote configuration fetches did, and so would a Gradle or Maven build that IntelliJ IDEA starts. Since the CA
differs between hosts, an image cannot carry it. A PKCS12 copy of the runtime's `cacerts` with the CA imported, built
after boot and passed as `-Djavax.net.ssl.trustStore`, fixes it.

`/run/agent/tls/ca-bundle.pem` is the bundle `agentd` mounts into nested Podman containers. It does not exist in an
agent's own sandbox.

**Secrets work already.** The backend and everything it starts inherit the SSH session's environment, which `agentd`
writes to the agent's `~/.ssh/environment`. `git` and `gh` in the IDE's terminal see the placeholder token and work
through the mediated network, as they do over `agentctl ssh`.

## Sizes and resources

| What | Size |
| --- | --- |
| Backend archive | 1.15 GB for WebStorm, 1.62 GB for IntelliJ IDEA Ultimate |
| Backend unpacked, in `~/.cache/JetBrains/RemoteDev/dist` | 3.0 GB |
| Backend state after opening the project | 485 MB in `~/.cache/JetBrains/<product>`, 11 MB in `~/.config/JetBrains`, 93 MB in `~/.local/share/JetBrains` |
| JetBrains Client on the host | 1.8 GB in `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist`, 1.2 GB of memory while connected |
| Backend memory | `-Xmx2048m` by default. About 1 GB resident when started, 3 GB with the project open. |
| Backend CPU | A load of about 5 on 6 vCPUs for the first minute of indexing, idle afterwards |

The project had no `node_modules`, so a JavaScript project with its dependencies installed adds the TypeScript
language service and a larger index. Plan for about 4 GiB of memory for the IDE beyond what the agent needs without
it. The sandbox has no swap, and `agentd` protects only the SSH server from the OOM killer, so an agent short of
memory loses the backend or its harness rather than slowing down. A backend keeps its memory after Gateway exits, and
after the client closes when the user chooses to keep it running.

## Proposal

For the base images, a `jetbrains` feature beside `browser` and `desktop` in
[the base images](../../../images/agentctl-base/README.md#features):

1. Installs one backend, chosen by the image (WebStorm, IntelliJ IDEA Ultimate) at a pinned build, outside the home,
   so that agents share the image layer instead of each downloading 1 GB and keeping 3 GB.
2. Describes it in `/etc/agent-access.d/jetbrains.conf`, as the `desktop` feature describes its VNC units in
   `vnc.conf`: the product code, the build and the path.
3. Builds the trust store after boot, with a unit like the `browser` feature's
   [`agent-browser-ca-init`](../../../images/agentctl-base/features/browser/rootfs/usr/local/libexec/agent-browser-ca-init),
   and names it in the backend's VM options.

For `agentctl`, one verb for every client that opens an agent over SSH, which the TUI's open menu and the Claude app
proposal share:

```sh
agentctl connect <agent> --client gateway|claude-app|vscode|zed [--project <path>] [--ide <product>]
```

It sets up what the client needs and opens it:

1. Adds the `Include` if it is missing, as the TUI's "Set up SSH" does.
2. Starts the agent if it is stopped, and waits for `SshReady`.
3. Writes the client's connection entry, `agentctl-<agent>`, where the client needs one: Gateway's `sshConfigs.xml`,
   the Claude app's `ssh_configs.json`.
4. Chooses the project: `--project`, else the one repository the image clones, else `/home/agent/code`.
5. Opens the client. For Gateway that is a `deploy=false` link to the backend `jetbrains.conf` names, or, on an image
   without one, a `deploy=true` link for the product and build `--ide` selects.

The TUI's open menu gets "JetBrains Gateway" next to VS Code and Zed, and runs the same steps. The Claude app
proposal's standing subscription, `agentctl ssh-config install --client`, can keep entries current for every agent
in clients whose own UI lists hosts. `connect` then needs to write only the one entry it opens.

The user agreement stays a step the user takes once per host. `connect` can say so before the first connect, and point
at the standalone start that has no timer.

## Not covered

- Gateway's "New Connection" wizard past its first page, which the links skip.
- macOS and Linux hosts, and reconnecting after the agent is stopped and started or the host sleeps.
- `remote-dev-server.sh warmup`, which may build the project's index before the first connect, and
  `registerBackendLocationForGateway`, which may make Gateway's wizard offer a backend outside its default directory.
- The IDE's GitHub integration for pull requests, which keeps accounts of its own, and whether it can use the
  placeholder token.
