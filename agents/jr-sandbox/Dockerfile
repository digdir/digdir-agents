# Kalibrert mot agentctl/examples/minimal/Dockerfile (fasiten for hva
# plattformen forventer av et Agent-image: tmux for sesjoner, `agent`-bruker
# med passordløs sudo, pre-seedet Claude Code-state, intet systemd/ssh for en
# headless agent). Ikke bygget/kjørt ennå — F1-E2E gjenstår.
FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    curl \
    git \
    jq \
    nodejs \
    npm \
    ripgrep \
    sudo \
    tmux \
    tzdata \
  && rm -rf /var/lib/apt/lists/*

# Norsk lokaltid: tidsstempler agenten skriver skal matche klokka til
# menneskene som reviewer (symlenken er systeminnstillingen alt leser).
RUN ln -sf /usr/share/zoneinfo/Europe/Oslo /etc/localtime

# GitHub CLI — kloning, issues og PR-er (auth via mediert GITHUB_TOKEN)
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      >/etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*

# Harness-pin: bor i imaget (jf. agentctl/HARNESSES.md — adapter-endringer og
# pin-bump hører sammen; følg samme versjon som agentctl/examples).
ARG CLAUDE_CODE_VERSION=2.1.292

# Agent-sandboxer mounter CA-bundelen sin her; dette stegets Node-nedlastinger
# skal stole på den (mønster fra examples/minimal).
RUN if [ -r /run/agent/tls/ca-bundle.pem ]; then \
      export NODE_EXTRA_CA_CERTS=/run/agent/tls/ca-bundle.pem; \
    fi; \
    npm install --global --allow-scripts=@anthropic-ai/claude-code \
        "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
  && usermod --login agent --home /home/agent --move-home ubuntu \
  && groupmod --new-name agent ubuntu \
  && printf 'agent ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/agent \
  && chmod 0440 /etc/sudoers.d/agent \
  && install -d -m 0755 -o agent -g agent /home/agent/.claude /home/agent/code /triggers

ENV DISABLE_AUTOUPDATER=1
ENV GH_NO_UPDATE_NOTIFIER=1
ENV HOME=/home/agent
ENV PATH=/home/agent/.local/bin:${PATH}

# Pre-seedet harness-state: hopper over onboarding/trust-dialogene som spiste
# prompts i M0 (hasCompletedOnboarding + trust for /home/agent/code).
COPY --chown=agent:agent --chmod=0600 claude-state.json /home/agent/.claude/.claude.json

USER agent
WORKDIR /home/agent/code
