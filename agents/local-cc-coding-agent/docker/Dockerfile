FROM node:24-bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends bash ca-certificates curl git jq ripgrep \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

# GitHub CLI — kloning, issues og PR-er (auth via GH_TOKEN)
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      >/etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*

# Agent-instruksen bakes inn og kopieres til /workspace av entrypointet, slik
# at Claude Code laster den som CLAUDE.md i arbeidskatalogen. Build-konteksten
# er agentkatalogen (bygg med: docker build -f docker/Dockerfile .).
COPY CLAUDE.md /opt/agent/CLAUDE.md
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# CRLF-stripp i tilfelle imaget bygges fra en Windows-checkout
RUN sed -i 's/\r$//' /opt/agent/CLAUDE.md /usr/local/bin/entrypoint.sh \
  && chmod +x /usr/local/bin/entrypoint.sh

# Kjør som ikke-root ("node"-brukeren følger med basisimaget)
RUN mkdir -p /workspace /triggers /knowledge \
  && chown -R node:node /workspace /triggers /knowledge
USER node
ENV HOME=/home/node

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
