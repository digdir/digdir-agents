# The agent's image

Build the image `FROM` an agentctl base image. It already has what agentd needs, the harnesses, Node.js, `gh` and
git; the full list is at https://github.com/digdir/digdir-agents/blob/main/images/agentctl-base/README.md

| Image | For |
| --- | --- |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04` | Headless work. Start here. |
| `ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop` | Adds Chromium with `playwright-cli` and an X11 desktop the person can watch over VNC. |

agentctl does not know the current digest. Look it up and pin it:

```sh
docker buildx imagetools inspect ghcr.io/digdir/agentctl-base:ubuntu-26.04
```

Use the `Digest:` line of the output, the index for every platform, as `FROM ...:ubuntu-26.04@sha256:<digest>`.

## A Dockerfile

```dockerfile
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:<digest>

# The base ends as the agent user; installing needs root.
USER root

RUN apt-get update \
    && apt-get install --yes --no-install-recommends build-essential postgresql-client \
    && rm -rf /var/lib/apt/lists/*

# Steps that download through Node trust the agent's network mediator only with this line. Don't make it an ENV.
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; fi \
    && npm install --global pnpm@9 \
    && rm -rf /home/agent/.npm

# Optional: Chromium and playwright-cli, for testing web apps headless. `desktop` instead adds a desktop with VNC
# (and `access: [{type: vnc}]` in the manifest). Each needs the ENV lines that `agent-feature env <feature>` prints;
# see below for how to get them.
RUN agent-feature install browser
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright

# Fails the build when the image no longer meets what agentd needs, naming what is wrong.
RUN agent-image-check

USER agent
```

- A feature's ENV lines come from the base image itself, so they match the digest you pinned:
  `docker run --rm --entrypoint agent-feature ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:<digest> env browser`.
- The base image's Node.js runs the harnesses' hooks: don't remove it. When the repository needs another major
  version, install it beside it for the agent user, for example with a version manager, or install it over
  `/usr/local` from the official tarball, as the base image's Dockerfile does
  (https://github.com/digdir/digdir-agents/blob/main/images/agentctl-base/Dockerfile), and list it in
  `instructions.md`.
- `HOME` is `/home/agent` in every step, so a root step that writes a cache there (npm, pip, dotnet, cargo) leaves a
  root-owned directory the agent cannot use. Remove it in the same step, as above; `agent-image-check` reports what
  is left.
- Install toolchains at the versions the repository asks for (`.nvmrc`, `global.json`, `rust-toolchain.toml`), and
  list them in `instructions.md` as well (`agentctl guide instructions`). For an agent spanning several projects,
  bake in only what most of its work needs and let the agent install the rest when a task calls for it.
- To run containers, add `RUN agent-feature install containers`: Podman, the `docker` command and Docker Compose.
  agentd sets up Podman's socket for the agent user. Docker's own engine gets no such setup.
- Repositories go in `instructions.md`, not in the image. The base images can also clone at boot from
  `/etc/agent-workspace.d/*.list`, one `OWNER/NAME` per line, if the person wants them there before the first
  session. They clone with the agent's GitHub account, so private repositories need one (`agentctl guide accounts`).
- The image is built when the agent is created. A changed Dockerfile takes effect only when the agent is deleted and
  applied again.

A custom base, not built on these images, must meet the image contract:
https://github.com/digdir/digdir-agents/blob/main/agentctl/docs/image-contract.md
