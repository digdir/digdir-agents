//! The base image a new agent's Dockerfile starts from, the published `agentctl-base` headless
//! image (`images/agentctl-base`), and what is added to it for what the agent should be able to
//! do: Podman for containers, and the image's own browser and desktop features.

use super::sizing::Size;

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

/// What an agent's image can do; each tier adds to the one before.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tier {
    /// The harnesses, Git and the GitHub CLI.
    Basic,
    /// Also containers, through Podman with a Docker-compatible CLI and Compose.
    Developer,
    /// Also a browser for testing web applications.
    Web,
    /// Also a graphical desktop, opened over VNC.
    Desktop,
}

impl Tier {
    pub(super) const ALL: [Self; 4] = [Self::Basic, Self::Developer, Self::Web, Self::Desktop];

    /// What a person chooses, in their words.
    pub(super) const fn describe(self) -> &'static str {
        match self {
            Self::Basic => "Write code: Claude Code, Codex, Git and GitHub",
            Self::Developer => "Also build and run things in containers",
            Self::Web => "Also test web apps in a browser",
            Self::Desktop => "Also use a desktop you can watch and take over",
        }
    }

    /// Whether the agent's own stage installs Podman.
    pub(super) const fn has_containers(self) -> bool {
        !matches!(self, Self::Basic)
    }

    /// The most a size recommendation for this tier asks for.
    pub(super) const fn most(self) -> Size {
        match self {
            Self::Basic => Size {
                cpus: 2,
                memory_gib: 4,
                disk_gib: 32,
            },
            Self::Developer | Self::Web | Self::Desktop => Size {
                cpus: 4,
                memory_gib: 8,
                disk_gib: 64,
            },
        }
    }

    /// Whether the agent opens the image's desktop with `access: [{type: vnc}]`.
    pub(super) const fn has_desktop(self) -> bool {
        matches!(self, Self::Desktop)
    }

    /// Whether the image has a browser, whose shared memory lands in `/tmp`.
    pub(super) const fn has_browser(self) -> bool {
        matches!(self, Self::Web | Self::Desktop)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_add_containers_a_browser_and_a_desktop() {
        assert!(!Tier::Basic.has_containers() && Tier::Developer.has_containers());
        assert!(!Tier::Developer.has_browser() && Tier::Web.has_browser() && Tier::Desktop.has_browser());
        assert!(Tier::Desktop.has_desktop() && !Tier::Web.has_desktop());
        assert!(BASE_IMAGE.contains("@sha256:"), "pinned by digest");
    }
}
