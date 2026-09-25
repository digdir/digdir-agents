# nvt-bridge i container (docker-outside-of-docker), slik at oppsettet blir
# likt på Windows/WSL2 og Mac. Se README.md for hvorfor hostens Docker-socket
# mountes inn her og ALDRI i agentcontainerne.
FROM node:24-slim

# docker-cli + compose-plugin: broen snakker med HOSTENS daemon
# (`docker exec … agentdctl`). make/git: nvt sin lokale flyt er Make-basert
# (`make agent-init/agent-up/agent-down`).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gnupg make git \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && chmod a+r /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
      > /etc/apt/sources.list.d/docker.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends docker-ce-cli docker-compose-plugin \
 && apt-get purge -y --auto-remove curl gnupg \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Ingen runtime-avhengigheter, så ingen npm install — og ingen byggesteg:
# Node stripper typene selv.
COPY package.json ./
COPY src ./src

# Kjører som root med vilje: tilgang til hostens Docker-socket krever at
# uid-en er i socketens gruppe, og den gid-en varierer mellom WSL2, Docker
# Desktop og Mac. Socket-tilgangen ER privilegiet her — å droppe til `node`
# ville gitt falsk trygghet, ikke reell isolasjon. Isolasjonsgrensen som
# betyr noe er at AGENTENE aldri får denne socketen.
CMD ["node", "--disable-warning=ExperimentalWarning", "src/index.ts"]
