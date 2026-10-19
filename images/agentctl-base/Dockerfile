# The agentctl base images, published as ghcr.io/digdir/agentctl-base:ubuntu-26.04 (the headless stage) and
# :ubuntu-26.04-desktop. They meet agentd's image contract (agentctl/IMAGE.md) and carry the harness pins agentd's
# adapters are tested with, so an Agent image only adds its own tools. README.md beside this file describes them.
FROM ubuntu:26.04@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7 AS headless

ENV DEBIAN_FRONTEND=noninteractive

# What agentd needs in the guest (tmux, sudo, systemd, openssh-server, ca-certificates, iproute2 for `ss`) and the
# everyday tools an Agent expects to find. dbus provides the system bus `systemctl` needs when not run as root. fd-find provides `fdfind`, which Pi searches files with; tzdata lets an
# image select another time zone than UTC with one symlink.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        btop \
        ca-certificates \
        curl \
        dbus \
        fd-find \
        fonts-jetbrains-mono \
        fonts-liberation \
        git \
        iproute2 \
        jq \
        less \
        neovim \
        openssh-server \
        procps \
        python3 \
        ripgrep \
        sudo \
        systemd \
        tmux \
        tzdata \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# The image's ubuntu user becomes the agent user, UID 1000, with passwordless sudo, which agentd's setup steps use.
# OpenSSH rejects even public-key authentication for a locked account when PAM is disabled. Password and keyboard
# authentication remain disabled by the platform-owned SSH policy.
RUN usermod --login agent --home /home/agent --move-home ubuntu \
    && passwd --delete agent \
    && groupmod --new-name agent ubuntu \
    && printf 'agent ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/agent \
    && chmod 0440 /etc/sudoers.d/agent \
    && install -d -m 0755 -o agent -g agent /home/agent/code \
    && install -d -m 0700 -o agent -g agent /home/agent/.claude \
    && install -d -m 0700 -o agent -g agent /run/user/1000 \
    && install -d -m 0755 /etc/agent-workspace.d \
    && rm -f /etc/machine-id /var/lib/dbus/machine-id \
    && touch /etc/machine-id

ARG TARGETARCH
# agentd runs the harnesses' activity hooks and Claude Code's status line with `node`.
ARG NODE_VERSION=22.23.2
ARG GH_VERSION=2.100.0
ARG GH_STACK_VERSION=0.1.1
# Terminal evidence for pull requests: asciinema records a command, agg renders the cast to a GIF.
ARG ASCIINEMA_VERSION=3.2.1
ARG AGG_VERSION=1.9.0

# gh-stack, asciinema and agg publish no checksum files, so their digests are pinned here and move together with
# the version.
RUN case "${TARGETARCH}" in \
        amd64) NODE_ARCH=x64; RUST_ARCH=x86_64; \
            GH_SHA256=e4d4bb4498e8d007abe545b6568926793ace1b6447da598294a610018cb164be; \
            GH_STACK_SHA256=9ed103934fab0f90d3341fdfc4a342785396d39f5621fc7313a62602ce2b5462; \
            ASCIINEMA_SHA256=1b405bbda565b33c3c4718de67fedc3535580603c0694b1ff3fb04f363430a20; \
            AGG_SHA256=f111e315cd71056b116302342553dd765b7297579ed511f111d0cedb442aeda6 ;; \
        arm64) NODE_ARCH=arm64; RUST_ARCH=aarch64; \
            GH_SHA256=ea4e7a581a32ccad6cc7923cb1576ac5859ba4b9a16ab22eb8f8a96e78e2e961; \
            GH_STACK_SHA256=2da13f8c46f2770237c744b341ab6be9f07508585a6762634c4a88aa355460bc; \
            ASCIINEMA_SHA256=b516a6d896844c0ffbc96e0a55afe4cbcc79216abde0fc64fdda4e39bee421ea; \
            AGG_SHA256=2b4be407b97e00e1c313a41d154ced8fa3d02c560c8f47a0db4950a2576444c9 ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && NODE_ARCHIVE="node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz" \
    && NODE_SHA256="$(curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/SHASUMS256.txt" \
        | awk -v filename="${NODE_ARCHIVE}" '$2 == filename { print $1 }')" \
    && test -n "${NODE_SHA256}" \
    && curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/${NODE_ARCHIVE}" -o /tmp/node.tar.xz \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz --strip-components=1 --no-same-owner -C /usr/local \
        --exclude="node-v${NODE_VERSION}-linux-${NODE_ARCH}/include" \
    && GH_ARCHIVE="gh_${GH_VERSION}_linux_${TARGETARCH}.tar.gz" \
    && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/${GH_ARCHIVE}" -o /tmp/gh.tar.gz \
    && echo "${GH_SHA256}  /tmp/gh.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/gh.tar.gz -C /tmp \
    && install -m 0755 "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}/bin/gh" /usr/local/bin/gh \
    && GH_STACK_DIR=/home/agent/.local/share/gh/extensions/gh-stack \
    && install -d "${GH_STACK_DIR}" \
    && curl -fsSL \
        "https://github.com/github/gh-stack/releases/download/v${GH_STACK_VERSION}/linux-${TARGETARCH}" \
        -o "${GH_STACK_DIR}/gh-stack" \
    && echo "${GH_STACK_SHA256}  ${GH_STACK_DIR}/gh-stack" | sha256sum -c - \
    && chmod 0755 "${GH_STACK_DIR}/gh-stack" \
    && printf '%s\n' \
        'owner: github' \
        'name: gh-stack' \
        'host: github.com' \
        "tag: v${GH_STACK_VERSION}" \
        'ispinned: true' \
        "path: ${GH_STACK_DIR}/gh-stack" \
        >"${GH_STACK_DIR}/manifest.yml" \
    && chown -R agent:agent /home/agent/.local \
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
    && rm -rf /tmp/node.tar.xz /tmp/gh.tar.gz "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}" /tmp/asciinema /tmp/agg \
    && node --version \
    && gh --version \
    && asciinema --version \
    && agg --version

# Route GitHub credentials through the gh CLI so plain git commands authenticate with the host-mediated token.
# Written system-wide, below any user configuration, because the home directory is resynchronized from the host.
RUN git config --system credential.https://github.com.helper '!/usr/local/bin/gh auth git-credential' \
    && git config --system credential.https://gist.github.com.helper '!/usr/local/bin/gh auth git-credential'

# The harness pins agentd's adapters are tested with (agentctl/HARNESSES.md). agentd verifies and configures the
# harnesses an Agent declares, but never installs one.
ARG CLAUDE_CODE_VERSION=2.1.302
ARG CODEX_VERSION=0.163.0
ARG PI_VERSION=1.3.0

# Agent Sandboxes mount their CA bundle here; this step's Node downloads trust it.
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then \
      export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; \
    fi; \
    npm install --global --allow-scripts=@anthropic-ai/claude-code \
        "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npm install --global "@openai/codex@${CODEX_VERSION}" \
    && npm install --global --ignore-scripts "@earendil-works/pi-coding-agent@${PI_VERSION}" \
    && scratch="$(mktemp -d)" \
    && HOME="${scratch}" TMPDIR="${scratch}" claude --version \
    && HOME="${scratch}" TMPDIR="${scratch}" codex --version \
    && HOME="${scratch}" TMPDIR="${scratch}" pi --version \
    && rm -rf "${scratch}" /root/.npm /root/.cache /tmp/node-compile-cache

COPY headless/ /
# The optional parts `agent-feature install` adds to an image built on this one.
COPY features/ /usr/local/share/agentctl-base/features/
# Claude Code's onboarding and workspace trust, which agentd does not write. Seeded here rather than through an
# Agent's home overlay, which is applied again on every pass and would undo what Claude Code records in the file.
# The directory exists already: BuildKit would give a parent directory COPY creates the file's mode, 0600.
COPY --chown=agent:agent --chmod=0600 claude-state.json /home/agent/.claude/.claude.json

# agentd owns the loopback SSH server policy, unit and per-Agent state. The distribution's own ssh.service and socket
# are masked so the image cannot expose its package-generated host keys. The guest has no udev to activate hvc0's
# device unit, so its generated serial login would otherwise wait 90 seconds at boot. The Sandbox kernel has no
# loadable modules and rejects Ubuntu's raised pid_max, which would otherwise leave the system degraded at every boot.
RUN systemctl mask ssh.service ssh.socket serial-getty@hvc0.service systemd-modules-load.service \
    && ln -s /dev/null /etc/sysctl.d/50-pid-max.conf \
    && rm -f /etc/ssh/ssh_host_*_key /etc/ssh/ssh_host_*_key.pub \
    && systemctl enable agent-workspace-init.service \
    && nvim --headless \
        "+lua assert(vim.g.colors_name == 'habamax'); assert(vim.o.number); assert(vim.o.cursorline); assert(vim.o.termguicolors)" \
        +quit \
    && rm -rf /tmp/nvim.root /root/.local \
    && agent-image-check

ENV DISABLE_AUTOUPDATER=1
ENV GH_NO_UPDATE_NOTIFIER=1
ENV GH_NO_EXTENSION_UPDATE_NOTIFIER=1
ENV HOME=/home/agent
ENV LANG=C.UTF-8
ENV PATH=/home/agent/.local/bin:${PATH}
# Nothing else creates a per-user runtime directory: systemd-logind owns that job and a Session is a tmux process
# rather than a login. Without one, tools such as skopeo and buildah fall back to a directory the Agent cannot use.
ENV XDG_RUNTIME_DIR=/run/user/1000

USER agent
WORKDIR /home/agent/code

ENTRYPOINT ["/usr/lib/systemd/systemd"]

# The headless image plus Chromium and a graphical desktop with VNC access, for work that needs a real screen:
# browser captures for pull request evidence, and people watching or taking over. It adds the desktop with
# agent-feature, exactly as an image built on the headless one would.
FROM headless AS desktop

# The headless stage ends as the unprivileged Agent user; installing packages needs root again.
USER root

RUN agent-feature install desktop

# What `agent-feature env desktop` prints; features/*/env explain each variable.
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
ENV DISPLAY=:1
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium

RUN agent-image-check

USER agent
WORKDIR /home/agent/code

ENTRYPOINT ["/usr/lib/systemd/systemd"]
