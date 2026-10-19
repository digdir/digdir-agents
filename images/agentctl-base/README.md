# agentctl base images

Images to build Agent images on. They meet `agentd`'s [image contract](../../agentctl/IMAGE.md) and carry the harness
versions its adapters are tested with, so an Agent's Dockerfile only adds the tools its work needs.

| Image | Contents |
| --- | --- |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04` | Headless: Ubuntu 26.04 booting systemd, the `agent` user, Claude Code, Codex and Pi, Node.js, `gh` with `gh-stack`, git, tmux, Neovim, ripgrep, fd, jq, btop, asciinema and agg |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop` | The headless image plus the [`desktop` feature](#features): Chromium with `playwright-cli`, ffmpeg, and an X11 desktop with VNC access and the `computer-use` skill |

Both are published for `linux/amd64` and `linux/arm64` from `main` whenever this directory changes, and also tagged
`<tag>-sha-<commit>`. Pin a digest, which Renovate keeps current:

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

# Optional: a time zone other than UTC.
RUN ln -sf /usr/share/zoneinfo/Europe/Oslo /etc/localtime

# Optional: repositories to clone at boot, one OWNER/NAME [DESTINATION] per line.
COPY workspace.list /etc/agent-workspace.d/workspace.list

RUN agent-image-check

USER agent
```

- `HOME` is `/home/agent` in every stage, so a root build step that writes a cache there, such as `npm install`, leaves
  a root-owned directory the Agent cannot use. Remove it in the same step; `agent-image-check` reports what is left.
- Build steps inside an Agent trust its mediated CA through the system store and `/run/agent/tls/ca-bundle.pem`. Node
  reads neither on its own, so a step that downloads through Node starts with
  `if [ -r /run/agent/tls/ca-bundle.pem ]; then export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; fi`. Do not
  persist that with `ENV`.
- `agent-workspace-init.service` clones the repositories `/etc/agent-workspace.d/*.list` names into `/home/agent/code`
  at boot with `gh repo clone`, authenticated by the Agent's `GITHUB_TOKEN`, and leaves a destination that already has
  `.git` alone. Without a list it does not start.
- The base images' OCI labels, such as `org.opencontainers.image.source` and `title`, are inherited. Set your own when
  you publish an image, so that the registry links it to its own repository.
- Skills an image ships go in `~/.claude/skills` for Claude Code and `~/.agents/skills` for Codex and Pi; `agentd`
  leaves skills it does not manage alone.

## Features

The headless image carries the optional parts the desktop image adds, so an image built on it can add them after its
own tools, at the versions the base pins. An image with a `minimal` and a desktop variant can then build the desktop
variant on its `minimal` one and share those layers, instead of installing its tools twice on two base images:

```dockerfile
FROM minimal AS desktop
USER root
RUN agent-feature install desktop
# What `agent-feature env desktop` prints. A script cannot set an image's environment, and agent-image-check fails an
# image without it.
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
ENV DISPLAY=:1
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium
RUN agent-image-check
USER agent
```

| Feature | Adds |
| --- | --- |
| `browser` | Chromium with `playwright-cli` and its skill, ffmpeg, and the `video-to-gif` and `media-preview` helpers. Chromium trusts the Agent's mediated CA once `$XDG_RUNTIME_DIR/chromium-ca-ready` exists, shortly after boot. |
| `desktop` | `browser`, plus an X11 desktop on display `:1` with VNC access, the `desktop`, `desktop-terminal` and `chromium` commands, and the `computer-use` skill. Provides `/etc/agent-access.d/vnc.conf`, so an Agent on it can declare `access: [{type: vnc}]`. |

`agent-feature list` shows what an image has. The instructions about the desktop belong to the Agent; see the self-dev
Agent's [desktop.md](../../agents/self-dev/full/desktop.md). The desktop image is the headless one after
`agent-feature install desktop`.

## Changing them

The harness versions here are the platform's pins: update them as [HARNESSES.md](../../agentctl/HARNESSES.md)
describes, in the same pull request as any adapter change they need. To try a change locally, build the images under
their published names; a build whose `FROM` names the tag without a digest, including a nested Agent's, then uses
them:

```sh
podman build --target headless --tag ghcr.io/digdir/agentctl-base:ubuntu-26.04 images/agentctl-base
podman build --target desktop --tag ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop images/agentctl-base
```

CI builds both from [docker-bake.hcl](../../docker-bake.hcl).
