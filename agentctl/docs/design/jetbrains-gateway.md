# JetBrains Gateway against an agent

What it takes to run a JetBrains IDE on the host against code that lives and runs in an agent's sandbox, and what a
base image feature for it should provide. It answers that before anyone builds the feature.

The findings come from one connection: JetBrains Gateway 2026.2.2 on Windows 11, `agentctl` v0.1.0-preview.13, and a
WebStorm 2026.2.3 backend in an agent with 6 vCPUs, 10 GiB of memory, no swap and `network: {mode: mediated, allow:
all}`. The project was a Node and pnpm repository without `node_modules`.

## What works without changes

Gateway reaches the agent through `agentctl ssh-proxy` as it is. Its own SSH client reads `~/.ssh/config`, follows the
`Include` that `agentctl ssh-config install` adds, and takes the `ProxyCommand`, `IdentityFile` and
`UserKnownHostsFile` from the agent's entry. Enter the alias `ssh-info` prints, such as `agentctl-<agent>`, as the
host, `agent` as the user and 22 as the port. Gateway uploads its worker over SFTP, starts the backend over SSH and
forwards the backend's port over the same connection, all of which the agent's
[SSH server configuration](../../src/sandbox/platform/linux/ssh.rs) allows.

The backend and everything it starts inherit the SSH session's environment, which `agentd` writes to the agent's
`~/.ssh/environment`. So `git` and `gh` in the IDE's terminal see the placeholder token and work through the mediated
network, as they do over `agentctl ssh`. The IDE's Git integration calls the same `git`, with the same credential
helper.

Downloads through the mediated network work for anything that trusts the system bundle
`/etc/ssl/certs/ca-certificates.crt`. The backend archive came at about 20 MB/s. The IntelliJ platform's own HTTPS
client reads the system store as well, and its Marketplace requests did not fail.

## What does not

**A plain JVM does not trust the mediated network.** The mediated network intercepts every TLS connection, with a CA
that is generated for each host and that Microsandbox appends to the guest's system bundle at boot (see
[`execution.rs`](../../../sandbox/microsandbox/src/execution.rs)). The JetBrains Runtime's own `cacerts` does not have
it, and it cannot read the PEM bundle as a trust store. Code in the backend that uses the JVM's default trust store
fails with `PKIX path building failed`, as one of WebStorm's remote configuration fetches did. So would a JVM the IDE
starts, such as a Gradle or Maven build in IntelliJ IDEA. Since the CA differs between hosts, an image cannot carry
it. It has to be imported in the guest after boot. A PKCS12 copy of the runtime's `cacerts` with the CA imported, passed
as `-Djavax.net.ssl.trustStore`, fixes it.

`/run/agent/tls/ca-bundle.pem` is the bundle `agentd` mounts into nested Podman containers. It does not exist in an
agent's own sandbox, so the guest's CA source is the system bundle.

**The first connection from a host fails unless the user is quick.** JetBrains Client shows its user agreement on
first start, and Gateway kills the client if it has not connected within about 35 seconds. Starting JetBrains Client
once on its own, from `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist\<build>\bin`, lets the user accept the agreement
without that limit.

**A Gateway link can only reuse an installed backend.** A `jetbrains-gateway://connect#type=ssh&...` link with
`deploy=true` needs the ID of an SSH connection saved in Gateway. One with `deploy=false` takes the host, port and user
directly, plus `idePath` and `projectPath`, so a backend that is already in the sandbox can be opened from a script:

```text
jetbrains-gateway://connect#type=ssh&deploy=false&host=agentctl-<agent>&port=22&user=agent&projectPath=<path>&idePath=<backend>
```

## Sizes and resources

| What | Size |
| --- | --- |
| Backend archive (WebStorm 2026.2.3, `linux`) | 1.15 GB. IntelliJ IDEA Ultimate's is 1.62 GB. |
| Backend unpacked, in `~/.cache/JetBrains/RemoteDev/dist` | 3.0 GB, unpacked in about 35 seconds |
| Backend state after opening the project | 485 MB in `~/.cache/JetBrains/<product>`, 11 MB in `~/.config/JetBrains`, 93 MB in `~/.local/share/JetBrains` |
| JetBrains Client on the host | 682 MB download, 1.8 GB in `%LOCALAPPDATA%\JetBrains\JetBrainsClientDist`, 1.2 GB of memory while connected |
| Backend memory | `-Xmx2048m` by default. About 1 GB resident when started, 3 GB with the project open. |
| Backend CPU | A load of about 5 on 6 vCPUs for the first minute of indexing, idle afterwards |

A JavaScript project with its dependencies installed adds the TypeScript language service and a larger index on top of
that. Plan for about 4 GiB of memory for the IDE beyond what the agent needs without it. The sandbox has no swap, and
`agentd` protects only the SSH server from the OOM killer, so an agent short of memory loses the backend or its
harness rather than slowing down.

## What a feature should provide

A `jetbrains` feature in [the base images](../../../images/agentctl-base/README.md#features) would provide:

- **A preinstalled backend**, chosen by the image (WebStorm, IntelliJ IDEA Ultimate) at a pinned build, outside the
  home. Each agent then shares the image layer instead of downloading 1 GB and keeping 3 GB in its home. Gateway
  downloads a matching JetBrains Client to the host by itself, so the host needs nothing pinned. The backend's
  `remote-dev-server.sh registerBackendLocationForGateway` and `warmup` subcommands are the candidates for making
  Gateway's wizard offer it and for building the project's index ahead of the first connection.
- **A trust store with the mediated CA**, built after boot from the system bundle by a unit like the `browser`
  feature's [`agent-browser-ca-init`](../../../images/agentctl-base/features/browser/rootfs/usr/local/libexec/agent-browser-ca-init),
  and named in the backend's VM options. JVMs the IDE starts need it too. Whether to set it for every JVM through
  `JAVA_TOOL_OPTIONS` is the image's choice, since that variable also makes every JVM print a notice.
- **Resource guidance** in the feature's documentation, from the table above.

The SSH access, port forwarding, environment, token mediation and downloads need nothing new.

## Not covered

- Gateway's own deployment flow, which downloads the backend from its wizard. It needs a saved connection, so it was
  not run, and nothing above shows whether its download trusts the mediated CA.
- The JetBrains Toolbox App, which Gateway now recommends for remote development and which also reads `~/.ssh/config`.
- macOS and Linux hosts, and reconnecting after the agent is stopped and started or the host sleeps.
- The IDE's GitHub integration for pull requests, which keeps accounts of its own, and whether it can use the
  placeholder token.
