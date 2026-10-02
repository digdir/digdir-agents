# UTKAST (F0) — modellert på altinn-studio agents/Dockerfile, som er fasiten
# for hva plattformen forventer av et Agent-image (systemd som init, tmux for
# sesjoner, sudo + openssh for SSH-access, en `agent`-bruker). Nøyaktig
# bruker-/init-oppsett kalibreres i F1 mot innflyttet kode.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    curl \
    git \
    jq \
    openssh-server \
    procps \
    ripgrep \
    sudo \
    systemd \
    tmux \
    tzdata \
  && rm -rf /var/lib/apt/lists/*

# Norsk lokaltid, som altinn-imagene: tidsstempler agenten skriver skal matche
# klokka til menneskene som reviewer.
RUN ln -sf /usr/share/zoneinfo/Europe/Oslo /etc/localtime

# Node for Claude Code (harness-pin bor i imaget, jf. HARNESSES.md i
# altinn-studio: adapter-endringer og pin-bump hører sammen).
ARG NODE_VERSION=24.11.0
RUN case "$(dpkg --print-architecture)" in \
      amd64) NODE_ARCH=x64 ;; \
      arm64) NODE_ARCH=arm64 ;; \
      *) echo "unsupported arch" >&2; exit 1 ;; \
    esac \
  && curl -fsSL "https://nodejs.org/download/release/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz" -o /tmp/node.tar.xz \
  && tar -xJf /tmp/node.tar.xz --strip-components=1 -C /usr/local \
  && rm -f /tmp/node.tar.xz

RUN npm install -g @anthropic-ai/claude-code

# GitHub CLI — kloning, issues og PR-er (auth via mediert GITHUB_TOKEN)
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      >/etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*

# Sandbox-brukeren. Kalibreringspunkt: navn/uid og sudo-oppsett skal matche
# det agentd forventer (altinn-imagene bruker `agent` med passordløs sudo).
RUN useradd --create-home --shell /bin/bash agent \
  && echo "agent ALL=(ALL) NOPASSWD:ALL" >/etc/sudoers.d/agent \
  && chmod 0440 /etc/sudoers.d/agent \
  && mkdir -p /home/agent/code /triggers \
  && chown -R agent:agent /home/agent /triggers

# systemd som init (initSystem: image i agent.yaml)
CMD ["/sbin/init"]
