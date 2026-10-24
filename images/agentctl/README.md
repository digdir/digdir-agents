# agentctl image

`ghcr.io/digdir/agentctl` holds `agentctl` and `agentd` alone, at the image root, for an Agent image to copy in: an
Agent that runs [nested Agents](../../agentctl/README.md#images-home-and-harnesses), or one whose tools drive
`agentctl`, such as `agentctl ssh-config install`. It is published for `linux/amd64` and `linux/arm64`, built on
Debian 12, so the binaries run on the base images and on any distribution with a glibc at least as new as Debian 12's.

| Tag | Contents | `agentctl --version` |
| --- | --- | --- |
| `v0.1.0-preview.13` | A release, built from its tag and published after the release | `agentctl v0.1.0-preview.13` |
| `main` | The newest commit on `main` | `agentctl v0.1.0-preview.12.main.g1c60ebd` |
| `sha-1c60ebdf75` | One commit on `main`, by its first ten characters | as for `main` |

Every commit on `main` is published. A `main` build reports the latest release, then `.main.g` and the newest commit
that changed what the build reads; after a stable release such as `v0.2.0`, it reports `v0.2.1-main.g…`. A commit that
changes nothing the build reads, such as one to the documentation, reuses the cached build and so gets the same image,
digest included: a digest pin on `main` moves only when the binaries do. Pin a digest, which Renovate keeps current, and copy the binaries onto
the `PATH`:

```dockerfile
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:…

COPY --from=ghcr.io/digdir/agentctl:v0.1.0-preview.13@sha256:… /agentctl /agentd /usr/local/bin/
```

Use `main@sha256:…` to follow `main` through Renovate's digest updates, or `sha-<commit>` for an exact commit.
`agentctl self update` does not manage a copy in an image: rebuild the image to change it.

A release can also be installed without this image, with the installer in standalone mode:

```dockerfile
ARG AGENTCTL_VERSION=v0.1.0-preview.12
RUN curl -fsSL "https://github.com/digdir/digdir-agents/releases/download/agentctl/${AGENTCTL_VERSION}/install.sh" \
        | AGENT_VERSION="${AGENTCTL_VERSION}" AGENT_INSTALL_MODE=standalone AGENT_INSTALL_DIR=/usr/local/bin sh \
    && agentctl --version
```

## Trying a change

To give an Agent image an `agentctl` that is not on `main`, build this image from your checkout under a `localhost/`
tag and point the image's `COPY --from` at it while you test. Run it from the repository root:

```sh
podman build --file images/agentctl/Dockerfile --build-arg AGENT_VERSION=v0.0.1-dev \
  --tag localhost/agentctl:dev .
```

Do not tag it with the published name: agentd builds an Agent's image for a platform, and Podman then pulls the
published image over a local one with the same name. `AGENT_VERSION` is required and becomes the version the binaries
report.

CI builds it from [docker-bake.hcl](../../docker-bake.hcl) on every pull request that changes the Rust workspace or this
directory, and in a release's dry run, and [publishes](../../.github/workflows/agentctl-image.yml) it from `main` and
from releases. `.github/workflows/agentctl-image.yml` lists the paths the build reads, as `Dockerfile.dockerignore`
does: change both together.
