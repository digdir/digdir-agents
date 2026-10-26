# agentctl image

`ghcr.io/digdir/agentctl` holds only `/agentctl` and `/agentd`, for agent images that need them, such as one running
nested agents. It is published for `linux/amd64` and `linux/arm64`. The binaries need glibc 2.36 (Debian 12) or newer.

| Tag | Contents |
| --- | --- |
| `v<version>` | A release |
| `main` | The newest commit on `main`. Its digest moves only when the binaries change |
| `sha-<commit>` | A commit on `main` whose push ran the image workflow |

Pin a digest, which Renovate keeps current:

```dockerfile
COPY --from=ghcr.io/digdir/agentctl:<tag>@sha256:… /agentctl /agentd /usr/local/bin/
```

A release can also be installed without this image:

```dockerfile
ARG AGENTCTL_VERSION=v<version>
RUN curl -fsSL "https://github.com/digdir/digdir-agents/releases/download/agentctl/${AGENTCTL_VERSION}/install.sh" \
        | AGENT_VERSION="${AGENTCTL_VERSION}" AGENT_INSTALL_MODE=standalone AGENT_INSTALL_DIR=/usr/local/bin sh
```

## Trying a change

Build it from the repository root under a `localhost/` tag, so Podman does not pull the published image over it, and
point `COPY --from` at that tag:

```sh
podman build -f images/agentctl/Dockerfile --build-arg AGENT_VERSION=v0.0.1-dev -t localhost/agentctl:dev .
```

`Dockerfile.dockerignore` and [agentctl-image.yml](../../.github/workflows/agentctl-image.yml) both list the paths the
build reads. Change them together.
