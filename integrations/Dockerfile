FROM node:24-bookworm-slim

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY src ./src

# Kjør som ikke-root ("node"-brukeren følger med basisimaget). /state og
# /triggers må finnes med riktig eier i imaget, slik at volumes som mountes
# der arver node-eierskapet ved første oppstart.
RUN mkdir -p /state /triggers && chown -R node:node /state /triggers /app
USER node

# Defaults for kjøring i container; compose setter de samme eksplisitt slik at
# de vinner over verdier fra env_file (.env er skrevet for kjøring på hosten).
ENV AGENT_TRIGGERS_DIR=/triggers \
    AGENT_STATE_DIR=/state

CMD ["node", "--disable-warning=ExperimentalWarning", "src/index.ts"]
