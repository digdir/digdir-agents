# agentctl base images

Images to build Agent images on. They meet `agentd`'s [image contract](../../agentctl/IMAGE.md) and carry the harness
versions its adapters are tested with, so an Agent's Dockerfile only adds the tools its work needs.

| Image | Contents |
| --- | --- |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04` | Headless: Ubuntu 26.04 booting systemd, the `agent` user, Claude Code, Codex and Pi, Node.js, `gh` with `gh-stack`, git, tmux, Neovim, ripgrep, fd, jq, btop, asciinema and agg |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop` | The headless image plus Chromium with `playwright-cli`, ffmpeg, and an X11 desktop with VNC access and the `computer-use` skill |

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
  leaves skills it does not manage alone. The desktop image ships `playwright-cli` and `computer-use` there.
- The desktop image provides `/etc/agent-access.d/vnc.conf`, so an Agent on it can declare `access: [{type: vnc}]`.
  Its Sessions have `DISPLAY=:1`. The instructions about the desktop belong to the Agent; see the self-dev Agent's
  [desktop.md](../../agents/self-dev/full/desktop.md).

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
