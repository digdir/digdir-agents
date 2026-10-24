# agentctl and agentd, published as ghcr.io/digdir/agentctl for Agent images to copy in, such as an Agent that runs
# nested Agents. Build it from the repository root, as docker-bake.hcl does; AGENT_VERSION is the version they report.
#
#   COPY --from=ghcr.io/digdir/agentctl:v0.1.0-preview.12@sha256:… /agentctl /agentd /usr/local/bin/

# Bookworm's glibc is older than the distributions Agent images build on, so the binaries run on all of them. The
# toolchain itself is the one rust-toolchain.toml names, which rustup installs when this image's differs.
FROM docker.io/library/rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS chef

# libcap-ng, as the release builds install, and cargo-chef, which builds the dependencies apart from the workspace.
ARG CARGO_CHEF_VERSION=0.1.78
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libcap-ng-dev \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}" \
    && rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git"
WORKDIR /src

# The recipe is the workspace reduced to its manifests and lockfile: it changes only when the dependencies do.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The dependencies build in a layer of their own, before AGENT_VERSION and the sources, so a build whose dependencies
# match main's reuses them from its cache and compiles only the workspace. They build with the configuration, toolchain
# and arguments of the workspace build below; with any other, it would compile them again.
FROM chef AS build
COPY .cargo/config.toml .cargo/config.toml
COPY rust-toolchain.toml ./
COPY --from=planner /src/recipe.json recipe.json
# The unpacked crate sources are left out of the layer, which a pull request downloads from main's cache: Cargo
# unpacks them again from the downloaded crates in seconds, without compiling anything again.
RUN cargo chef cook --release --locked -p agent --bins --recipe-path recipe.json \
    && rm -rf "${CARGO_HOME}/registry/src"

COPY . .
ARG AGENT_VERSION
# The target directory is removed in the same step, so that the cached layer holds only the binaries.
RUN test -n "${AGENT_VERSION}" \
    && cargo build --release --locked -p agent --bins \
    && mkdir /out \
    && install -m 0755 target/release/agentctl target/release/agentd /out/ \
    && test "$(/out/agentctl --version)" = "agentctl ${AGENT_VERSION}" \
    && rm -rf target

# Only the binaries: the image is copied from, not run.
FROM scratch
COPY --from=build /out/agentctl /out/agentd /
