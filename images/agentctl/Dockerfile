# agentctl and agentd, published as ghcr.io/digdir/agentctl for Agent images to copy in, such as an Agent that runs
# nested Agents. Build it from the repository root, as docker-bake.hcl does; AGENT_VERSION is the version they report.
#
#   COPY --from=ghcr.io/digdir/agentctl:v0.1.0-preview.12@sha256:… /agentctl /agentd /usr/local/bin/

# Bookworm's glibc is older than the distributions Agent images build on, so the binaries run on all of them. The
# toolchain itself is the one rust-toolchain.toml names, which rustup installs when this image's differs.
FROM docker.io/library/rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS build

# libcap-ng, as the release builds install.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libcap-ng-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

ARG AGENT_VERSION
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    test -n "${AGENT_VERSION}" \
    && cargo build --release --locked -p agent --bins \
    && mkdir /out \
    && install -m 0755 target/release/agentctl target/release/agentd /out/ \
    && test "$(/out/agentctl --version)" = "agentctl ${AGENT_VERSION}"

# Only the binaries: the image is copied from, not run.
FROM scratch
COPY --from=build /out/agentctl /out/agentd /
