//! The base image a new agent's Dockerfile starts from, the published `agentctl-base` headless
//! image (`images/agentctl-base`), and what is added to it for what the agent should be able to
//! do: Podman for containers, and the image's own browser and desktop features.

/// The base image: the platform's Linux contract, the harnesses, Git and the GitHub CLI, with the
/// browser and desktop as features to install. Pinned by digest, so an agent's image builds the
/// same way later; a newer agentctl brings a newer one.
pub(crate) const BASE_IMAGE: &str =
    "ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:d6728be9e9972c1c36b6a637ee1a3e984f32f022048e05d39daf498dfe75bc40";

/// The base image's browser feature, with the environment `agent-feature env browser` prints.
pub(crate) const BROWSER: &str = "# Chromium with playwright-cli, from the base image's browser feature.
RUN agent-feature install browser
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
";

/// The base image's desktop feature, which brings the browser, with the environment
/// `agent-feature env desktop` prints.
pub(crate) const DESKTOP: &str = "# A desktop with VNC access and the browser, from the base image's desktop feature.
RUN agent-feature install desktop
ENV NODE_PATH=/usr/local/lib/node_modules
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
ENV DISPLAY=:1
ENV DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ENV ACCESSIBILITY_ENABLED=1
ENV PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/libexec/agent-playwright-chromium
";

/// Podman for containers, with `docker` and `docker compose` working through it. agentd enables
/// the Podman socket and grants it to the agent user when the image has Podman.
pub(crate) const CONTAINERS: &str = r"# Containers for builds and services: Podman, with `docker` and `docker compose` working through it.
# netavark invokes nft for bridge networking and aardvark-dns resolves container names between
# compose services.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        aardvark-dns \
        build-essential \
        docker-compose-v2 \
        nftables \
        podman \
        podman-docker \
    && rm -rf /var/lib/apt/lists/* \
    && touch /etc/containers/nodocker \
    && podman --version \
    && /usr/libexec/docker/cli-plugins/docker-compose version
ENV DOCKER_HOST=unix:///run/podman/podman.sock
";
