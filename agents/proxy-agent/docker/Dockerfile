FROM node:24-bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends bash ca-certificates curl git jq ripgrep \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent

# agent-browser + Chromium — brukes av web-research-skillen. Vi bruker
# Debians chromium-pakke (arkitektur-riktig, apt-forvaltede avhengigheter)
# i stedet for agent-browsers egen Chrome-nedlasting.
RUN apt-get update \
  && apt-get install -y --no-install-recommends chromium \
  && rm -rf /var/lib/apt/lists/* \
  && npm install -g agent-browser
# Chromiums eget sandbox-lag krever user namespaces som Dockers default
# seccomp-profil blokkerer for ikke-root; containeren er isolasjonsgrensen.
# --disable-dev-shm-usage: /dev/shm er bare 64MB i Docker som standard.
ENV AGENT_BROWSER_EXECUTABLE_PATH=/usr/bin/chromium \
    CHROMIUM_FLAGS="--no-sandbox --disable-dev-shm-usage" \
    AGENT_BROWSER_IDLE_TIMEOUT_MS=300000

# GitHub CLI — brukes av github-issues-prs-skillen (auth via GH_TOKEN)
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      >/etc/apt/sources.list.d/github-cli.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends gh \
  && rm -rf /var/lib/apt/lists/*

# Skills bakes inn utenfor /home/node/.pi — pi-home-volumet ville ellers
# skygget dem ved image-oppdateringer. Entrypointet laster dem med --skill.
COPY skills /opt/pi-skills
RUN find /opt/pi-skills -type f -exec sed -i 's/\r$//' {} +

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
# CRLF-stripp i tilfelle imaget bygges fra en Windows-checkout
RUN sed -i 's/\r$//' /usr/local/bin/entrypoint.sh \
  && chmod +x /usr/local/bin/entrypoint.sh

# Kjør som ikke-root ("node"-brukeren følger med basisimaget)
# /home/node/.pi må finnes med riktig eier i imaget, slik at named volume
# som mountes der arver node-eierskapet ved første oppstart
RUN mkdir -p /workspace /triggers /home/node/.pi/agent \
  && chown -R node:node /workspace /triggers /home/node/.pi
USER node
ENV HOME=/home/node

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["watch"]
