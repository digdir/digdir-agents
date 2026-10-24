# agentctl image

`ghcr.io/digdir/agentctl` holds only `/agentctl` and `/agentd`, for Agent images that need them, such as one running
[nested Agents](../../agentctl/README.md#images-home-and-harnesses). Published for `linux/amd64` and `linux/arm64`; the
binaries need glibc 2.36 (Debian 12) or newer.

| Tag | Contents |
| --- | --- |
| `v0.1.0-preview.13` | A release |
| `main` | The newest commit on `main`, reporting e.g. `v0.1.0-preview.12.main.g1c60ebd` |
| `sha-1c60ebdf75` | One commit on `main` |

Pin a digest, which Renovate keeps current:

```dockerfile
COPY --from=ghcr.io/digdir/agentctl:v0.1.0-preview.13@sha256:… /agentctl /agentd /usr/local/bin/
```

A commit that does not change the binaries keeps `main`'s digest. A release can also be installed without this image:

```dockerfile
ARG AGENTCTL_VERSION=v0.1.0-preview.12
RUN curl -fsSL "https://github.com/digdir/digdir-agents/releases/download/agentctl/${AGENTCTL_VERSION}/install.sh" \
        | AGENT_VERSION="${AGENTCTL_VERSION}" AGENT_INSTALL_MODE=standalone AGENT_INSTALL_DIR=/usr/local/bin sh
```

## Trying a change

Build it from the repository root under a `localhost/` tag, so Podman does not pull the published image over it, and
point `COPY --from` at that tag:

```sh
podman build -f images/agentctl/Dockerfile --build-arg AGENT_VERSION=v0.0.1-dev -t localhost/agentctl:dev .
```

The paths the build reads are listed in both `Dockerfile.dockerignore` and
[agentctl-image.yml](../../.github/workflows/agentctl-image.yml); change them together.
