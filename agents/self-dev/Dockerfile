FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b AS minimal

# The repository's rust-toolchain.toml selects the toolchain inside a checkout, and Renovate moves
# this ARG together with it. The agent user (the image's ubuntu user until it is renamed below)
# owns RUSTUP_HOME, so a checkout pinned ahead of this image installs its toolchain on first use
# instead of failing.
ARG RUST_VERSION=1.97.1
ARG CARGO_MACHETE_VERSION=0.9.2

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

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
        btop \
        build-essential \
        ca-certificates \
        clang \
        curl \
        flex \
        fonts-jetbrains-mono \
        fonts-liberation \
        git \
        hyperfine \
        iproute2 \
        jq \
        libcap-ng-dev \
        libelf-dev \
        libssl-dev \
        lldb \
        linux-perf \
        neovim \
        nftables \
        openssh-server \
        pkg-config \
        podman \
        podman-docker \
        procps \
        python3 \
        python3-pyelftools \
        ripgrep \
        rustup \
        shellcheck \
        socat \
        sqlite3 \
        strace \
        sudo \
        systemd \
        tcpdump \
        tmux \
    && rm -rf /var/lib/apt/lists/* \
    && nvim --version \
    && shellcheck --version \
    && btop --version \
    && hyperfine --version \
    && sqlite3 --version \
    && socat -V >/dev/null \
    && tcpdump --version \
    && python3 -c 'import elftools' \
    && bison --version >/dev/null \
    && flex --version \
    && CARGO_HOME=/usr/local/cargo rustup set profile minimal \
    && CARGO_HOME=/usr/local/cargo rustup default "${RUST_VERSION}" \
    && CARGO_HOME=/usr/local/cargo rustup component add clippy rustfmt \
    && CARGO_HOME=/usr/local/cargo cargo install cargo-machete --version "${CARGO_MACHETE_VERSION}" --locked \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git \
    && chmod -R a+rX /usr/local/cargo \
    && chown -R ubuntu:ubuntu /usr/local/rustup

COPY common/nvim-sysinit.vim /etc/xdg/nvim/sysinit.vim

RUN nvim --headless \
    "+lua assert(vim.g.colors_name == 'habamax'); assert(vim.o.number); assert(vim.o.cursorline); assert(vim.o.termguicolors)" \
    +quit

ARG TARGETARCH
ARG GH_VERSION=2.100.0
ARG GH_STACK_VERSION=0.1.1
ARG ASCIINEMA_VERSION=3.2.1
ARG AGG_VERSION=1.9.0
ARG NODE_VERSION=22.23.2
ARG ACTIONLINT_VERSION=1.7.12
ARG YQ_VERSION=4.54.1
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_ABOUT_VERSION=0.9.2

RUN case "${TARGETARCH}" in \
        amd64) NODE_ARCH=x64; RUST_ARCH=x86_64; \
            GH_SHA256=e4d4bb4498e8d007abe545b6568926793ace1b6447da598294a610018cb164be; \
            GH_STACK_SHA256=9ed103934fab0f90d3341fdfc4a342785396d39f5621fc7313a62602ce2b5462; \
            ASCIINEMA_SHA256=1b405bbda565b33c3c4718de67fedc3535580603c0694b1ff3fb04f363430a20; \
            AGG_SHA256=f111e315cd71056b116302342553dd765b7297579ed511f111d0cedb442aeda6; \
            ACTIONLINT_SHA256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8; \
            YQ_SHA256=8e34fc298390875de416e6a4afcb8cabeceb25d9aa8506c1a2f9353cf702ea5f; \
            CARGO_DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f; \
            CARGO_ABOUT_SHA256=9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af ;; \
        arm64) NODE_ARCH=arm64; RUST_ARCH=aarch64; \
            GH_SHA256=ea4e7a581a32ccad6cc7923cb1576ac5859ba4b9a16ab22eb8f8a96e78e2e961; \
            GH_STACK_SHA256=2da13f8c46f2770237c744b341ab6be9f07508585a6762634c4a88aa355460bc; \
            ASCIINEMA_SHA256=b516a6d896844c0ffbc96e0a55afe4cbcc79216abde0fc64fdda4e39bee421ea; \
            AGG_SHA256=2b4be407b97e00e1c313a41d154ced8fa3d02c560c8f47a0db4950a2576444c9; \
            ACTIONLINT_SHA256=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6; \
            YQ_SHA256=189088da0c6429ec5178dfaab1a114805f6cab0b61b165ab236efedf1d57a71b; \
            CARGO_DENY_SHA256=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d; \
            CARGO_ABOUT_SHA256=af5169282fb6f84e13471493f405437e43ac517744c9ae12fbe2cdf0a6f0e5a8 ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && NODE_ARCHIVE="node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz" \
    && NODE_SHA256="$(curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/SHASUMS256.txt" \
        | awk -v filename="${NODE_ARCHIVE}" '$2 == filename { print $1 }')" \
    && test -n "${NODE_SHA256}" \
    && curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/${NODE_ARCHIVE}" -o /tmp/node.tar.xz \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz --strip-components=1 -C /usr/local \
    && GH_ARCHIVE="gh_${GH_VERSION}_linux_${TARGETARCH}.tar.gz" \
    && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/${GH_ARCHIVE}" -o /tmp/gh.tar.gz \
    && echo "${GH_SHA256}  /tmp/gh.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/gh.tar.gz -C /tmp \
    && install -m 0755 "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}/bin/gh" /usr/local/bin/gh \
    && curl -fsSL \
        "https://github.com/github/gh-stack/releases/download/v${GH_STACK_VERSION}/linux-${TARGETARCH}" \
        -o /tmp/gh-stack \
    && echo "${GH_STACK_SHA256}  /tmp/gh-stack" | sha256sum -c - \
    && curl -fsSL \
        "https://github.com/asciinema/asciinema/releases/download/v${ASCIINEMA_VERSION}/asciinema-${RUST_ARCH}-unknown-linux-gnu" \
        -o /tmp/asciinema \
    && echo "${ASCIINEMA_SHA256}  /tmp/asciinema" | sha256sum -c - \
    && install -m 0755 /tmp/asciinema /usr/local/bin/asciinema \
    && curl -fsSL \
        "https://github.com/asciinema/agg/releases/download/v${AGG_VERSION}/agg-${RUST_ARCH}-unknown-linux-gnu" \
        -o /tmp/agg \
    && echo "${AGG_SHA256}  /tmp/agg" | sha256sum -c - \
    && install -m 0755 /tmp/agg /usr/local/bin/agg \
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
    && rm -rf /tmp/node.tar.xz /tmp/gh.tar.gz "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}" /tmp/asciinema /tmp/agg \
        /tmp/actionlint.tar.gz /tmp/yq /tmp/cargo-deny.tar.gz "/tmp/${CARGO_DENY_DIR}" /tmp/cargo-about.tar.gz \
        "/tmp/${CARGO_ABOUT_DIR}" \
    && node --version \
    && gh --version \
    && asciinema --version \
    && agg --version \
    && actionlint --version \
    && yq --version \
    && cargo-deny --version \
    && cargo-about --version

# Route GitHub credentials through the gh CLI so plain git commands authenticate with the
# host-mediated token.
RUN git config --system credential.https://github.com.helper '!/usr/local/bin/gh auth git-credential'

ARG CODEX_VERSION=0.160.1
ARG CLAUDE_CODE_VERSION=2.1.292

# Node ignores the system trust store, and Buildah drops default environment from build stages,
# so a build inside an Agent needs the system store selected per step for npm to trust mediation.
RUN NODE_OPTIONS=--use-openssl-ca npm install --global "@openai/codex@${CODEX_VERSION}" \
    && NODE_OPTIONS=--use-openssl-ca npm install --global --allow-scripts=@anthropic-ai/claude-code \
        "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && claude --version \
    && codex --version

# A nested Microsandbox runtime needs /dev/kvm; tmpfiles.conf hands the device to the kvm group.
# OpenSSH rejects even public-key authentication for a locked account when PAM is disabled.
# Password and keyboard authentication remain disabled by the platform-owned SSH policy.
RUN { getent group kvm >/dev/null || groupadd --system kvm; } \
    && usermod --login agent --home /home/agent --move-home ubuntu \
    && passwd --delete agent \
    && groupmod --new-name agent ubuntu \
    && usermod --append --groups kvm agent \
    && printf 'agent ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/agent \
    && chmod 0440 /etc/sudoers.d/agent \
    && install -d -m 0755 -o agent -g agent \
        /home/agent/.local/share/gh/extensions/gh-stack /home/agent/code \
    && install -m 0755 -o agent -g agent \
        /tmp/gh-stack /home/agent/.local/share/gh/extensions/gh-stack/gh-stack \
    && printf '%s\n' \
        'owner: github' \
        'name: gh-stack' \
        'host: github.com' \
        "tag: v${GH_STACK_VERSION}" \
        'ispinned: true' \
        'path: /home/agent/.local/share/gh/extensions/gh-stack/gh-stack' \
        >/home/agent/.local/share/gh/extensions/gh-stack/manifest.yml \
    && chown -R agent:agent /home/agent/.local \
    && rm -f /tmp/gh-stack \
    && rm -f /etc/machine-id /var/lib/dbus/machine-id \
    && touch /etc/machine-id

COPY common/tmpfiles.conf /usr/lib/tmpfiles.d/agent-dev.conf
COPY common/workspace-init.service /etc/systemd/system/agent-workspace-init.service
COPY --chmod=0755 common/workspace-init.sh /usr/local/libexec/agent-workspace-init

# agentd owns the loopback SSH server policy, unit and per-Agent state. The distribution's own
# ssh.service and socket are masked so the image cannot expose its package-generated host keys.
RUN systemctl mask ssh.service ssh.socket \
    && rm -f /etc/ssh/ssh_host_*_key /etc/ssh/ssh_host_*_key.pub

# All guest Executions inherit init.scope, whose default task limit is too low
# for concurrent harnesses. The kernel pid limit remains the Sandbox boundary.
# The guest also has no udev to activate hvc0's device unit, so its generated
# serial login would otherwise wait 90 seconds at boot.
RUN install -d /etc/systemd/system/init.scope.d \
    && printf '[Scope]\nTasksMax=infinity\n' >/etc/systemd/system/init.scope.d/50-agent-tasks.conf \
    && install -d /usr/lib/sysctl.d \
    && printf 'fs.inotify.max_user_instances = 1024\nfs.inotify.max_user_watches = 1048576\nfs.inotify.max_queued_events = 32768\n' \
        >/usr/lib/sysctl.d/50-agent-inotify.conf \
    && systemctl mask serial-getty@hvc0.service \
    && systemctl enable agent-workspace-init.service

# Keep Cargo's registry and build output on the Sandbox root filesystem so a worktree mount does
# not share target state with the host and a fresh checkout keeps reusable artifacts outside Git.
ENV CARGO_HOME=/home/agent/.cargo
ENV CARGO_TARGET_DIR=/home/agent/.cache/cargo-target
# The Sandbox Image Backend speaks the Docker Engine API; point it at the Podman socket the
# platform enables and grants to the agent user.
ENV DOCKER_HOST=unix:///run/podman/podman.sock
ENV DISABLE_AUTOUPDATER=1
ENV GH_NO_UPDATE_NOTIFIER=1
ENV GH_NO_EXTENSION_UPDATE_NOTIFIER=1
ENV HOME=/home/agent
ENV PATH=/home/agent/.local/bin:/home/agent/.cargo/bin:${PATH}

USER agent
WORKDIR /home/agent/code

ENTRYPOINT ["/usr/lib/systemd/systemd"]

# The minimal image plus a browser and a graphical desktop, for work that needs a real screen: browser captures for
# pull request evidence, and the Agent platform's own VNC access, which only an image like this one offers.
FROM minimal AS full

# The minimal stage ends as the unprivileged Agent user; installing packages needs root again.
USER root

ARG PLAYWRIGHT_CLI_VERSION=0.1.20

# Standalone scripts resolve the globally installed `playwright` package from any directory.
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
# The desktop's session bus lives in the per-user runtime directory. Nothing else creates one, since
# systemd-logind owns that job and a Session is a tmux process rather than a login.
ENV XDG_RUNTIME_DIR=/run/user/1000

# ffmpeg converts and inspects browser captures and libnss3-tools imports the Agent CA into Chromium.
# xdg-utils lets `xdg-open`, and agentctl's open actions, open links on the desktop as on any other.
# The rest is a minimal X11 desktop: one process that is both X and VNC server, a window manager, a
# panel, the input and window tools the `desktop` helper uses, AT-SPI for `desktop tree`, and sakura
# as the terminal, whose text AT-SPI exposes too. wmctrl is left out because its build here segfaults
# on an empty window list; the helper reads the same properties with xprop and xdotool.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        at-spi2-core \
        dbus-x11 \
        ffmpeg \
        fonts-noto-color-emoji \
        fonts-noto-core \
        libnss3-tools \
        openbox \
        python3-pyatspi \
        sakura \
        tigervnc-standalone-server \
        tint2 \
        x11-utils \
        x11-xserver-utils \
        xdg-utils \
        xdotool \
    && rm -rf /var/lib/apt/lists/* \
    && test "$(id -u agent)" = 1000 \
    && install -d -m 0700 -o agent -g agent "${XDG_RUNTIME_DIR}"

# Playwright ties every package version to one Chromium build. The browser CLI owns the version:
# the `playwright` package is installed at exactly the version the CLI depends on, so the CLI,
# standalone scripts and the single browser under PLAYWRIGHT_BROWSERS_PATH cannot drift apart.
# The bundled skill is installed user-wide where Claude Code and Codex look for skills;
# `playwright-cli install --skills` would otherwise write it per working directory. Root-owned
# build caches under the agent home would deny the agent user its own cache directories later.
# Agent Sandboxes mount their CA bundle here; this step's Node downloads trust it.
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then \
      export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; \
    fi; \
    npm install --global "@playwright/cli@${PLAYWRIGHT_CLI_VERSION}" \
    && PLAYWRIGHT_VERSION="$(node -p "require('${NODE_PATH}/@playwright/cli/package.json').dependencies.playwright")" \
    && test -n "${PLAYWRIGHT_VERSION}" \
    && npm install --global "playwright@${PLAYWRIGHT_VERSION}" \
    && test "$(node -p "require('playwright/package.json').version")" = "${PLAYWRIGHT_VERSION}" \
    && playwright install --with-deps chromium \
    && chmod -R a+rX "${PLAYWRIGHT_BROWSERS_PATH}" \
    && playwright --version \
    && playwright-cli --version \
    && for skills in /home/agent/.claude/skills /home/agent/.agents/skills; do \
        mkdir -p "${skills}" \
        && cp -R "${NODE_PATH}/@playwright/cli/skills/playwright-cli" "${skills}/playwright-cli"; \
    done \
    && chown -R agent:agent /home/agent/.claude /home/agent/.agents \
    && rm -rf /var/lib/apt/lists/* /home/agent/.npm /home/agent/.cache \
    && if find /home/agent ! -user agent -print -quit | grep -q .; then \
        echo "build left files under /home/agent not owned by agent" >&2; exit 1; \
    fi

ARG NOVNC_VERSION=1.7.0
ARG NOVNC_SHA256=b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6
ARG WS_VERSION=8.21.3

# The browser viewer: noVNC, pinned by digest and trimmed to the files it serves, and `ws` for its
# WebSocket bridge. Neither is packaged for Ubuntu. Agent Sandboxes mount their CA bundle at the
# path below, so this step's Node downloads trust it.
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then \
      export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; \
    fi; \
    curl -fsSL "https://github.com/novnc/noVNC/archive/refs/tags/v${NOVNC_VERSION}.tar.gz" -o /tmp/novnc.tar.gz \
    && echo "${NOVNC_SHA256}  /tmp/novnc.tar.gz" | sha256sum -c - \
    && install -d -m 0755 /usr/local/share/novnc \
    && tar -xzf /tmp/novnc.tar.gz -C /usr/local/share/novnc --strip-components=1 \
        "noVNC-${NOVNC_VERSION}/vnc.html" \
        "noVNC-${NOVNC_VERSION}/app" \
        "noVNC-${NOVNC_VERSION}/core" \
        "noVNC-${NOVNC_VERSION}/vendor" \
        "noVNC-${NOVNC_VERSION}/po" \
        "noVNC-${NOVNC_VERSION}/defaults.json" \
        "noVNC-${NOVNC_VERSION}/mandatory.json" \
        "noVNC-${NOVNC_VERSION}/package.json" \
        "noVNC-${NOVNC_VERSION}/LICENSE.txt" \
    && rm -f /tmp/novnc.tar.gz \
    && npm install --global "ws@${WS_VERSION}" \
    && node -e "require('ws')" \
    && test -f /usr/local/share/novnc/vnc.html \
    && rm -rf /home/agent/.npm /home/agent/.cache

COPY --chmod=0755 common/bin/video-to-gif common/bin/media-preview /usr/local/bin/
COPY common/full-tmpfiles.conf /usr/lib/tmpfiles.d/agent-full.conf
COPY common/browser-ca-init.service /etc/systemd/system/agent-browser-ca-init.service
COPY --chmod=0755 common/browser-ca-init.sh /usr/local/libexec/agent-browser-ca-init
COPY --chmod=0755 common/novnc-service.js /usr/local/libexec/agent-novnc

# What this image offers for `access: [{type: vnc}]`: units shipped disabled, which agentd enables
# when an Agent declares the capability.
COPY common/vnc.socket /etc/systemd/system/agent-vnc.socket
COPY common/vnc.service /etc/systemd/system/agent-vnc.service
COPY common/vnc-web.service /etc/systemd/system/agent-vnc-web.service
COPY common/access-vnc.conf /etc/agent-access.d/vnc.conf

COPY common/desktop.conf /etc/agent-desktop.conf
COPY common/desktop-display.service /etc/systemd/system/agent-desktop-display.service
COPY common/desktop-session.service /etc/systemd/system/agent-desktop-session.service
COPY common/desktop-dbus.service /etc/systemd/system/agent-desktop-dbus.service
COPY --chmod=0755 common/desktop-tree.py /usr/local/libexec/agent-desktop-tree
COPY --chmod=0755 common/playwright-chromium.sh /usr/local/libexec/agent-playwright-chromium
COPY common/desktop-openbox-rc.xml /etc/xdg/openbox/rc.xml
COPY common/desktop-tint2rc /etc/xdg/tint2/tint2rc
COPY --chmod=0755 common/desktop-session.sh /usr/local/libexec/agent-desktop-session
COPY --chmod=0755 common/bin/chromium common/bin/desktop common/bin/desktop-terminal /usr/local/bin/
COPY common/desktop-terminal.desktop /usr/local/share/applications/desktop-terminal.desktop
COPY common/chromium.desktop /usr/local/share/applications/chromium.desktop
COPY --chmod=0755 common/browser-open.sh /usr/local/libexec/agent-browser-open
COPY common/mimeapps.list /etc/xdg/mimeapps.list

# xdotool refuses multi-byte characters such as æ, ø and å under the POSIX locale.
ENV LANG=C.UTF-8
# Every Session uses the Agent's own display. The desktop units read the same value from
# /etc/agent-desktop.conf, and the build below fails if the two disagree.
ENV DISPLAY=:1
# AT-SPI needs a session bus shared by Sessions and desktop programs. A Session is a tmux process
# rather than a login, so agent-desktop-dbus.service provides one at this fixed address.
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
# Chromium joins the accessibility bus only with ACCESSIBILITY_ENABLED, and fills in its tree only
# with --force-renderer-accessibility (about 20 MiB and 90 ms at startup). The `chromium` launcher
# passes the switch; playwright-cli starts headed browsers through it and headless ones as before.
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium

# These checks run as root with a scratch HOME: openbox writes under $HOME/.cache when invoked,
# and a root-owned file there would lock the Agent out of its own cache. The home ownership check
# below catches a future check that writes there anyway.
RUN systemctl enable agent-browser-ca-init.service \
        agent-desktop-display.service agent-desktop-session.service agent-desktop-dbus.service \
    && HOME=/tmp/image-checks sh -c ' \
        Xtigervnc -version 2>&1 | head -1 \
        && openbox --version | head -1 \
        && tint2 --version \
        && xdotool --version \
        && command -v sakura >/dev/null \
        && ffmpeg -version | head -1 \
        && AGENT_BROWSER_CA_READY_FILE=/dev/null chromium --version \
        && /usr/local/libexec/agent-playwright-chromium --headless --version \
        && desktop help >/dev/null \
        && test "$(xdg-mime query default x-scheme-handler/https)" = chromium.desktop' \
    && rm -rf /tmp/image-checks \
    && python3 -c 'import pyatspi' \
    && test -f /usr/share/dbus-1/services/org.a11y.Bus.service \
    && test "${DBUS_SESSION_BUS_ADDRESS}" = "unix:path=${XDG_RUNTIME_DIR}/bus" \
    && . /etc/agent-desktop.conf \
    && test "${DISPLAY}" = "${AGENT_DESKTOP_DISPLAY}" \
    && vnc_port="$(sed -n 's/^port=//p' /etc/agent-access.d/vnc.conf)" \
    && web_port="$(sed -n 's/^web-port=//p' /etc/agent-access.d/vnc.conf)" \
    && { grep -qxF "ListenStream=127.0.0.1:${vnc_port}" /etc/systemd/system/agent-vnc.socket \
        || { echo "agent-vnc.socket does not listen on the descriptor's port ${vnc_port}" >&2; exit 1; }; } \
    && { grep -qxF "Environment=AGENT_NOVNC_PORT=${web_port}" /etc/systemd/system/agent-vnc-web.service \
        || { echo "agent-vnc-web.service does not serve the descriptor's web-port ${web_port}" >&2; exit 1; }; } \
    && for unit in agent-vnc.socket agent-vnc.service agent-vnc-web.service; do \
        state="$(systemctl is-enabled "${unit}" 2>/dev/null || true)"; \
        case "${state}" in disabled | static) ;; \
            *) echo "${unit} is ${state}; it must ship disabled" >&2; exit 1 ;; \
        esac; \
    done \
    && command -v ss >/dev/null \
    && if find /home/agent ! -user agent -print -quit | grep -q .; then \
        echo "build left files under /home/agent not owned by agent" >&2; exit 1; \
    fi

USER agent
WORKDIR /home/agent/code

ENTRYPOINT ["/usr/lib/systemd/systemd"]
