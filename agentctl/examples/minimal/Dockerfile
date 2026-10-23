FROM ubuntu:26.04@sha256:c098726dd9b9733f3e885f8de0ef375206a311b0a317eadc40d347b1115fb944

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        curl \
        git \
        nodejs \
        npm \
        ripgrep \
        sudo \
        tmux \
    && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
ARG GH_VERSION=2.103.0
ARG GH_STACK_VERSION=0.2.1

RUN case "${TARGETARCH}" in \
        amd64) GH_SHA256=fdd768eb70f6409f80fcfa9f3a1c0adf0fc98d827ab3ce0bed2ac22762c5d786; \
            GH_STACK_SHA256=ffe9d48b6342faffc3b0cb420202ff1df32e24faf32693debd7423274bbdf859 ;; \
        arm64) GH_SHA256=7f322df34e624d14f5ec0c4acb121fd6dfa14392fdbbd1066aa8e0a05ac3721e; \
            GH_STACK_SHA256=25ec261a391261a1462d38f85c230b0e0b14ae3547926130996541f62f25f78b ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && GH_ARCHIVE="gh_${GH_VERSION}_linux_${TARGETARCH}.tar.gz" \
    && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/${GH_ARCHIVE}" -o /tmp/gh.tar.gz \
    && echo "${GH_SHA256}  /tmp/gh.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/gh.tar.gz -C /tmp \
    && install -m 0755 "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}/bin/gh" /usr/local/bin/gh \
    && curl -fsSL \
        "https://github.com/github/gh-stack/releases/download/v${GH_STACK_VERSION}/linux-${TARGETARCH}" \
        -o /tmp/gh-stack \
    && echo "${GH_STACK_SHA256}  /tmp/gh-stack" | sha256sum -c - \
    && rm -rf /tmp/gh.tar.gz "/tmp/gh_${GH_VERSION}_linux_${TARGETARCH}" \
    && gh --version

ARG CLAUDE_CODE_VERSION=2.1.309

# Agent Sandboxes mount their CA bundle here; this step's Node downloads trust it.
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then \
      export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; \
    fi; \
    npm install --global --allow-scripts=@anthropic-ai/claude-code \
        "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && usermod --login agent --home /home/agent --move-home ubuntu \
    && groupmod --new-name agent ubuntu \
    && printf 'agent ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/agent \
    && chmod 0440 /etc/sudoers.d/agent \
    && install -d -m 0755 -o agent -g agent \
        /home/agent/.claude /home/agent/.local/share/gh/extensions/gh-stack /home/agent/code \
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
    && rm -f /tmp/gh-stack

ENV DISABLE_AUTOUPDATER=1
ENV GH_NO_UPDATE_NOTIFIER=1
ENV GH_NO_EXTENSION_UPDATE_NOTIFIER=1
ENV HOME=/home/agent
ENV PATH=/home/agent/.local/bin:${PATH}

COPY --chown=agent:agent --chmod=0600 claude-state.json /home/agent/.claude/.claude.json

USER agent
WORKDIR /home/agent/code
