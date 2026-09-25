# Null avhengigheter — ingen npm install, bare Node-runtime og server.mjs.
FROM node:24-alpine
WORKDIR /app
COPY package.json server.mjs ./
USER node
CMD ["node", "server.mjs"]
