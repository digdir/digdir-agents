# agentctl base images

Images to build Agent images on. They meet `agentd`'s [image contract](../../agentctl/docs/image-contract.md) and
carry the harness versions its adapters are tested with, so an Agent's Dockerfile only adds the tools its work needs.

| Image | Contents |
| --- | --- |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04` | Headless: Ubuntu 26.04 booting systemd, the `agent` user, Claude Code, Codex and Pi, Node.js and common developer tools. The [Dockerfile](Dockerfile) has the full list |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop` | The headless image plus the [`desktop` feature](#features) |

Both are published for `linux/amd64` and `linux/arm64` from `main` whenever this directory changes, and also tagged
`<tag>-sha-<commit>`. Pin a digest, which Renovate keeps current.

```dockerfile
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:…
```

## Building on them

The images end as `USER agent` in `/home/agent/code`, with systemd as the entrypoint. Switch to root for your own
steps, finish with `agent-image-check` and switch back:

```dockerfile
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:…

USER root

RUN apt-get update \
    && apt-get install --yes --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*

# Optional: repositories to clone at boot.
COPY workspace.list /etc/agent-workspace.d/workspace.list

RUN agent-image-check

USER agent
```

- `HOME` is `/home/agent` in every stage, so remove caches a root build step writes there, such as npm's.
  `agent-image-check` reports what is left.
- When an Agent builds the image, a build step that downloads through Node must trust the Agent's mediated CA. Start
  it with `if [ -r /run/agent/tls/ca-bundle.pem ]; then export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; fi`,
  and do not set that with `ENV`.
- At boot, the image clones the repositories listed in `/etc/agent-workspace.d/*.list`, one `OWNER/NAME [DESTINATION]`
  per line, into `/home/agent/code` with the Agent's `GITHUB_TOKEN`.
- Skills an image ships go in `~/.claude/skills` for Claude Code and `~/.agents/skills` for Codex and Pi.

## Features

The headless image carries the optional features below, so an image built on it can add them after its own tools, at
the versions it pins. An image with a `minimal` and a desktop variant then builds the desktop variant on `minimal` and
shares its layers, instead of installing its tools on two base images:

```dockerfile
FROM minimal AS desktop
USER root
RUN agent-feature install desktop
# The ENV lines `agent-feature env desktop` prints.
RUN agent-image-check
USER agent
```

A script cannot set an image's environment, so add the `ENV` lines `agent-feature env <feature>` prints.
`agent-image-check` fails an image without them.

| Feature | Adds |
| --- | --- |
| `browser` | Chromium with `playwright-cli` and its skill, ffmpeg, and the `video-to-gif` and `media-preview` helpers. Chromium trusts the Agent's mediated CA once `$XDG_RUNTIME_DIR/chromium-ca-ready` exists, shortly after boot. |
| `containers` | Podman, with bridge networks and container name resolution. `agentd` sets up Podman's socket for the agent user, and Docker API clients reach it at the default `/run/docker.sock`. The `docker` command runs Podman, so do not install Docker's own CLI beside it. `docker compose` and `podman compose` run Docker Compose. |
| `desktop` | `browser`, plus an X11 desktop on display `:1` with VNC access and a Secret Service, the `desktop`, `desktop-terminal` and `chromium` commands, and the `computer-use` skill. Provides `/etc/agent-access.d/vnc.conf`, so an Agent on it can declare `access: [{type: vnc}]`. |

`agent-feature list` shows what an image has. The [self-dev images](../../agents/self-dev/Dockerfile) are built this
way. What to tell the harness about the browser and the desktop belongs in the Agent's instructions, as in the self-dev
Agent's [desktop.md](../../agents/self-dev/full/desktop.md).

## Adding agentctl

The base images do not include `agentctl` and `agentd`. An image that runs nested Agents copies them from the
[agentctl image](../agentctl/README.md).

## Changing them

The harness versions here are the platform's pins: update them as
[harness-upgrades.md](../../agentctl/docs/harness-upgrades.md) describes, in the same pull request as any adapter
change they need. To try a change locally, build the images under `localhost/` tags and point your image's `FROM` at
one for the test:

```sh
podman build --target headless --tag localhost/agentctl-base:ubuntu-26.04 images/agentctl-base
podman build --target desktop --tag localhost/agentctl-base:ubuntu-26.04-desktop images/agentctl-base
```

Do not tag them with the published names: when `agentd` builds an Agent's image, Podman pulls the published image over
a local one with the same name.

CI builds both from [docker-bake.hcl](../../docker-bake.hcl).
