# The self-development Agent's images, built on the agentctl base images in images/agentctl-base. FROM names the base
# tag without a digest, so that these images follow the platform they develop: CI builds them on the base built from
# the same commit (docker-bake.hcl), and a nested Agent builds on the base Podman has under that name, pulled or built
# locally. renovate.json keeps Renovate from pinning it.
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04 AS minimal

# The base ends as the unprivileged Agent user; installing packages needs root again.
USER root

# The repository's rust-toolchain.toml selects the toolchain inside a checkout, and Renovate moves
# this ARG together with it. The agent user owns RUSTUP_HOME, so a checkout pinned ahead of this
# image installs its toolchain on first use instead of failing.
ARG RUST_VERSION=1.97.1

ENV RUSTUP_HOME=/usr/local/rustup

# The toolchain the agent platform's own Rust workspace needs (libcap-ng-dev links the Microsandbox
# runtime), plus Podman so that a nested agentd can build Agent images through the Docker Engine API.
# Podman's netavark backend invokes nft for rootful bridge networking and aardvark-dns for
# container-name resolution. bc, bison, flex, libelf-dev and python3-pyelftools check libkrunfw's
# kernel configuration; sqlite3, socat and tcpdump inspect agentd's database, sockets and Sandbox
# traffic; hyperfine measures performance changes.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        aardvark-dns \
        bc \
        bison \
        build-essential \
        flex \
        hyperfine \
        libcap-ng-dev \
        libelf-dev \
        libssl-dev \
        linux-perf \
        nftables \
        pkg-config \
        podman \
        podman-docker \
        python3-pyelftools \
        rustup \
        shellcheck \
        socat \
        sqlite3 \
        strace \
        tcpdump \
    && rm -rf /var/lib/apt/lists/* \
    && shellcheck --version \
    && hyperfine --version \
    && sqlite3 --version \
    && socat -V >/dev/null \
    && tcpdump --version \
    && python3 -c 'import elftools' \
    && bison --version >/dev/null \
    && flex --version \
    && rustup set profile minimal \
    && rustup default "${RUST_VERSION}" \
    && rustup component add clippy rustfmt \
    && chown -R agent:agent /usr/local/rustup

# A nested Microsandbox runtime needs /dev/kvm; tmpfiles.conf hands the device to the kvm group. The
# workspace list names the repositories the base image's workspace unit clones at boot.
COPY common/tmpfiles.conf /usr/lib/tmpfiles.d/agent-dev.conf
COPY common/workspace.list /etc/agent-workspace.d/self-dev.list
RUN { getent group kvm >/dev/null || groupadd --system kvm; } \
    && usermod --append --groups kvm agent

ARG TARGETARCH
ARG ACTIONLINT_VERSION=1.7.12
ARG YQ_VERSION=4.54.1
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_ABOUT_VERSION=0.9.2
ARG CARGO_MACHETE_VERSION=0.9.2

RUN case "${TARGETARCH}" in \
        amd64) RUST_ARCH=x86_64; \
            ACTIONLINT_SHA256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8; \
            YQ_SHA256=8e34fc298390875de416e6a4afcb8cabeceb25d9aa8506c1a2f9353cf702ea5f; \
            CARGO_DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f; \
            CARGO_ABOUT_SHA256=9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af; \
            CARGO_MACHETE_TARGET=x86_64-unknown-linux-musl; \
            CARGO_MACHETE_SHA256=48200087f54c55aabcd4db4af1e25742b49846c02a1b1bfa134711945b35b2e9 ;; \
        arm64) RUST_ARCH=aarch64; \
            ACTIONLINT_SHA256=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6; \
            YQ_SHA256=189088da0c6429ec5178dfaab1a114805f6cab0b61b165ab236efedf1d57a71b; \
            CARGO_DENY_SHA256=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d; \
            CARGO_ABOUT_SHA256=af5169282fb6f84e13471493f405437e43ac517744c9ae12fbe2cdf0a6f0e5a8; \
            CARGO_MACHETE_TARGET=aarch64-unknown-linux-gnu; \
            CARGO_MACHETE_SHA256=6f96c3e6026a5bdd241b6ae600c6fb86c9197c6e189a894f91371baa01fd10f5 ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && curl -fsSL \
        "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_linux_${TARGETARCH}.tar.gz" \
        -o /tmp/actionlint.tar.gz \
    && echo "${ACTIONLINT_SHA256}  /tmp/actionlint.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/actionlint.tar.gz -C /usr/local/bin actionlint \
    && curl -fsSL \
        "https://github.com/mikefarah/yq/releases/download/v${YQ_VERSION}/yq_linux_${TARGETARCH}" \
        -o /tmp/yq \
    && echo "${YQ_SHA256}  /tmp/yq" | sha256sum -c - \
    && install -m 0755 /tmp/yq /usr/local/bin/yq \
    && CARGO_DENY_DIR="cargo-deny-${CARGO_DENY_VERSION}-${RUST_ARCH}-unknown-linux-musl" \
    && curl -fsSL \
        "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/${CARGO_DENY_DIR}.tar.gz" \
        -o /tmp/cargo-deny.tar.gz \
    && echo "${CARGO_DENY_SHA256}  /tmp/cargo-deny.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/cargo-deny.tar.gz -C /tmp "${CARGO_DENY_DIR}/cargo-deny" \
    && install -m 0755 "/tmp/${CARGO_DENY_DIR}/cargo-deny" /usr/local/bin/cargo-deny \
    && CARGO_ABOUT_DIR="cargo-about-${CARGO_ABOUT_VERSION}-${RUST_ARCH}-unknown-linux-musl" \
    && curl -fsSL \
        "https://github.com/EmbarkStudios/cargo-about/releases/download/${CARGO_ABOUT_VERSION}/${CARGO_ABOUT_DIR}.tar.gz" \
        -o /tmp/cargo-about.tar.gz \
    && echo "${CARGO_ABOUT_SHA256}  /tmp/cargo-about.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/cargo-about.tar.gz -C /tmp "${CARGO_ABOUT_DIR}/cargo-about" \
    && install -m 0755 "/tmp/${CARGO_ABOUT_DIR}/cargo-about" /usr/local/bin/cargo-about \
    && CARGO_MACHETE_DIR="cargo-machete-v${CARGO_MACHETE_VERSION}-${CARGO_MACHETE_TARGET}" \
    && curl -fsSL \
        "https://github.com/bnjbvr/cargo-machete/releases/download/v${CARGO_MACHETE_VERSION}/${CARGO_MACHETE_DIR}.tar.gz" \
        -o /tmp/cargo-machete.tar.gz \
    && echo "${CARGO_MACHETE_SHA256}  /tmp/cargo-machete.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/cargo-machete.tar.gz -C /tmp "${CARGO_MACHETE_DIR}/cargo-machete" \
    && install -m 0755 "/tmp/${CARGO_MACHETE_DIR}/cargo-machete" /usr/local/bin/cargo-machete \
    && rm -rf /tmp/actionlint.tar.gz /tmp/yq /tmp/cargo-deny.tar.gz "/tmp/${CARGO_DENY_DIR}" /tmp/cargo-about.tar.gz \
        "/tmp/${CARGO_ABOUT_DIR}" /tmp/cargo-machete.tar.gz "/tmp/${CARGO_MACHETE_DIR}" \
    && actionlint --version \
    && yq --version \
    && cargo-deny --version \
    && cargo-about --version \
    && cargo-machete --version

# Keep Cargo's registry and build output on the Sandbox root filesystem so a worktree mount does
# not share target state with the host and a fresh checkout keeps reusable artifacts outside Git.
ENV CARGO_HOME=/home/agent/.cargo
ENV CARGO_TARGET_DIR=/home/agent/.cache/cargo-target
# The Sandbox Image Backend speaks the Docker Engine API; point it at the Podman socket the
# platform enables and grants to the agent user.
ENV DOCKER_HOST=unix:///run/podman/podman.sock
ENV PATH=/home/agent/.local/bin:/home/agent/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN agent-image-check

USER agent

# The minimal image plus a browser and a graphical desktop, for work that needs a real screen: browser captures for
# pull request evidence, and the Agent platform's own VNC access, which only an image like this one offers.
FROM minimal AS full

USER root

RUN agent-feature install desktop

# What `agent-feature env desktop` prints.
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
ENV DISPLAY=:1
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium

RUN agent-image-check

USER agent
