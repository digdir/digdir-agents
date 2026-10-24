# agentctl base images

Images to build Agent images on. They meet `agentd`'s [image contract](../../agentctl/IMAGE.md) and carry the harness
versions its adapters are tested with, so an Agent's Dockerfile only adds the tools its work needs.

| Image | Contents |
| --- | --- |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04` | Headless: Ubuntu 26.04 booting systemd, the `agent` user, Claude Code, Codex and Pi, Node.js, `gh` with `gh-stack`, git, tmux, Neovim, ripgrep, fd, jq, btop, asciinema and agg |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop` | The headless image plus the [`desktop` feature](#features): Chromium with `playwright-cli`, ffmpeg, and an X11 desktop with VNC access and the `computer-use` skill |

Both are published for `linux/amd64` and `linux/arm64` from `main` whenever this directory changes, and also tagged
`<tag>-sha-<commit>`. Pin a digest, which Renovate keeps current. The self-dev images are the exception: they build on
the base from the same commit, so their `FROM` names the tag alone.

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
  at boot with `gh repo clone`, authenticated by the Agent's `GITHUB_TOKEN`. Each line names one repository as
  `OWNER/NAME`, optionally followed by a destination below `/home/agent/code`, which defaults to `NAME`; `#` starts a
  comment. Without a list the service does not start.
  - A clone is made in `<destination>.partial` and moved into place once it is complete, so an interrupted one is
    made again at the next boot. A destination that already has `.git`, such as a bind-mounted checkout or a clone a
    Session made first, is left alone.
  - The listed repository becomes the clone's default for `gh`. When it is a fork, `gh` adds the parent as `upstream`,
    and `gh pr create` and `gh issue` would otherwise target the parent.
- The base images' OCI labels, such as `org.opencontainers.image.source` and `title`, are inherited. Set your own when
  you publish an image, so that the registry links it to its own repository.
- Skills an image ships go in `~/.claude/skills` for Claude Code and `~/.agents/skills` for Codex and Pi; `agentd`
  leaves skills it does not manage alone.

## Features

The headless image carries optional features, including those the desktop image adds, so an image built on it can add
them after its own tools, at the versions the base pins. An image with a `minimal` and a desktop variant can then build
the desktop variant on its `minimal` one and share those layers, instead of installing its tools twice on two base
images:

```dockerfile
FROM minimal AS desktop
USER root
RUN agent-feature install desktop
# What `agent-feature env desktop` prints. A script cannot set an image's environment, and agent-image-check fails an
# image without it.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
ENV DISPLAY=:1
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium
ENV XDG_CURRENT_DESKTOP=X-Openbox:XFCE
RUN agent-image-check
USER agent
```

| Feature | Adds |
| --- | --- |
| `browser` | Chromium with `playwright-cli` and its skill, ffmpeg, and the `video-to-gif` and `media-preview` helpers. Chromium trusts the Agent's mediated CA once `$XDG_RUNTIME_DIR/chromium-ca-ready` exists, shortly after boot. |
| `containers` | Podman, with bridge networks and container name resolution. `podman.socket` belongs to the `agent` group from boot, and Docker API clients reach it at the default `/run/docker.sock`. The `docker` command runs Podman; it comes from podman-docker, which conflicts with Docker's own CLI packages. `docker compose` and `podman compose` run Docker Compose. |
| `desktop` | `browser`, plus an X11 desktop on display `:1` with VNC access and a [Secret Service](#the-secret-service), the `desktop`, `desktop-terminal` and `chromium` commands, and the `computer-use` skill. Provides `/etc/agent-access.d/vnc.conf`, so an Agent on it can declare `access: [{type: vnc}]`. |

`agent-feature list` shows what an image has. The [self-dev images](../../agents/self-dev/Dockerfile) are built this
way. The instructions about the desktop belong to the Agent; see the self-dev
Agent's [desktop.md](../../agents/self-dev/full/desktop.md). The desktop image is the headless one after
`agent-feature install desktop`.

### Desktop settings

`/etc/agent-desktop.conf` sets the desktop's display, geometry, keyboard layout and VNC socket. Override settings in
`/etc/agent-desktop.conf.d/*.conf` instead of editing it; files are read in name order and later ones win.

```dockerfile
COPY desktop.conf /etc/agent-desktop.conf.d/my-agent.conf
```

```sh
# desktop.conf
AGENT_DESKTOP_KEYBOARD=no
```

- `AGENT_DESKTOP_KEYBOARD` takes `setxkbmap` arguments, quoted if more than one, and defaults to `us`.
- `AGENT_DESKTOP_DISPLAY` must match the image's `ENV DISPLAY`; `agent-image-check` checks it.
- `AGENT_DESKTOP_GEOMETRY` is the display size, `1600x900` by default.

### The screenshot budget

The `desktop` helper downscales screenshots to fit a budget, a pixel count and a longest edge, and maps the model's
coordinates back to the display. The budget depends on the Session's harness, which agentd sets in `AGENT_HARNESS`:

| Harness | Budget | 1600x900 is sent at |
| --- | --- | --- |
| `claudeCode` | 1,175,000 px, 1568 px edge (Anthropic's standard tier) | 1445x812 |
| `codex` | 2,457,600 px, 2048 px edge (OpenAI's `high` detail) | 1600x900 |
| `pi`, unknown or none | 1,175,000 px, 1568 px edge | 1445x812 |

`DESKTOP_SCREENSHOT_PIXELS` and `DESKTOP_SCREENSHOT_MAX_EDGE` override the budget. `DESKTOP_MAX_WIDTH` and
`DESKTOP_MAX_HEIGHT` still work and replace it with a box. `desktop display` shows the budget in use.

### The browser

The `browser` feature installs `playwright` and `playwright-cli` globally, with the Chromium build they are pinned to
under `/opt/ms-playwright`.

A script outside a project finds the global `playwright` only when you set `NODE_PATH` for that command:

```sh
NODE_PATH="$(npm root -g)" node script.js
```

An ES module's `import` ignores `NODE_PATH`. Load Playwright with `createRequire` instead, and run the script the same
way:

```js
import { createRequire } from 'node:module';
const { chromium } = createRequire(import.meta.url)('playwright');
```

A repository that uses another Playwright version installs its build beside the base's: run
`npx playwright install chromium` in the repository, after `npm install`, as the agent user. The base's build belongs
to root, so the agent cannot remove it, and Playwright's cleanup of unused builds keeps it while the base's
`playwright-cli` is installed. If that Playwright needs more system packages, install them with
`sudo npx playwright install-deps chromium`.

To bake another build into an image, install it as root from a Playwright package that stays in the image, so that
Playwright's cleanup keeps the build and every user can read it:

```dockerfile
USER root
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; fi \
    && npm install --prefix /opt/playwright-1.63 playwright@1.63.0 \
    && PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright /opt/playwright-1.63/node_modules/.bin/playwright \
        install --with-deps chromium \
    && find /opt/ms-playwright ! -perm -a+r -exec chmod a+rX {} + \
    && rm -rf /home/agent/.npm /var/lib/apt/lists/*
```

### The Secret Service

The `desktop` feature runs gnome-keyring as the Secret Service, unlocked at boot, for what uses libsecret: `secret-tool`,
Chromium's password store and Electron's `safeStorage`, as in VS Code. `XDG_CURRENT_DESKTOP=X-Openbox:XFCE` makes
Chromium and Electron choose it; they do not recognize openbox. A Sandbox has no login to unlock a keyring with, so the
keyring's password is a fixed newline: `~/.local/share/keyrings` is unencrypted at rest, in effect, and protected only
by the Sandbox boundary. It persists with the home directory across Stop and Start.

## Using another Node.js

The base installs Node.js in `/usr/local`. It runs agentd's hooks, Claude Code's status line and the global harnesses.
An image that needs another Node.js installs it over `/usr/local` from the official tarball, as the base does; the
harnesses stay where they are. Remove the old npm first so that its files do not mix with the new one's. Node.js 26 also
needs `libatomic1`, and ships without Corepack.

```dockerfile
USER root
ARG TARGETARCH
ARG NODE_VERSION=26.12.1
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libatomic1 \
    && rm -rf /var/lib/apt/lists/* \
    && case "${TARGETARCH}" in amd64) NODE_ARCH=x64 ;; arm64) NODE_ARCH=arm64 ;; *) exit 1 ;; esac \
    && NODE_ARCHIVE="node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz" \
    && NODE_SHA256="$(curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/SHASUMS256.txt" \
        | awk -v filename="${NODE_ARCHIVE}" '$2 == filename { print $1 }')" \
    && test -n "${NODE_SHA256}" \
    && curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/${NODE_ARCHIVE}" -o /tmp/node.tar.xz \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && rm -rf /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/corepack \
        /usr/local/bin/npm /usr/local/bin/npx /usr/local/bin/corepack \
    && tar -xJf /tmp/node.tar.xz --strip-components=1 --no-same-owner -C /usr/local \
        --exclude="node-v${NODE_VERSION}-linux-${NODE_ARCH}/include" \
    && rm /tmp/node.tar.xz
RUN agent-image-check
```

Pi needs Node.js 22.19.0 or later, and Codex needs Node.js 16 or later; Claude Code is a native binary.
`agent-image-check` runs `node` and each harness with `--version`, so it fails a build whose Node.js cannot start one.

## Changing them

The harness versions here are the platform's pins: update them as [HARNESSES.md](../../agentctl/HARNESSES.md)
describes, in the same pull request as any adapter change they need. To try a change locally, build the images under
`localhost/` tags and point your image's `FROM` at one for the test:

```sh
podman build --target headless --tag localhost/agentctl-base:ubuntu-26.04 images/agentctl-base
podman build --target desktop --tag localhost/agentctl-base:ubuntu-26.04-desktop images/agentctl-base
```

Do not tag them with the published names: agentd builds an Agent's image for a platform, and Podman then pulls the
published image over a local one with the same name.

CI builds both from [docker-bake.hcl](../../docker-bake.hcl).
