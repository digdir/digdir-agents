# The container images this repository publishes, for `docker buildx bake`. CI builds each architecture on its own
# runner and publishes from main; `docker buildx bake -f docker-bake.hcl --load base-headless` builds one locally
# under its published name. Always pass `-f docker-bake.hcl`: without it, bake also reads the root docker-compose.yml.

# Where the agentctl base images are published.
variable "BASE_REPOSITORY" {
  default = "ghcr.io/digdir/agentctl-base"
}

# The distribution in the base image tags, so that a later Ubuntu can be published beside this one.
variable "BASE_DISTRIBUTION" {
  default = "ubuntu-26.04"
}

group "base" {
  targets = ["base-headless", "base-desktop"]
}

target "_source" {
  labels = {
    "org.opencontainers.image.source" = "https://github.com/digdir/digdir-agents"
  }
}

target "base-headless" {
  inherits = ["_source"]
  context  = "images/agentctl-base"
  target   = "headless"
  tags     = ["${BASE_REPOSITORY}:${BASE_DISTRIBUTION}"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-base"
    "org.opencontainers.image.description" = "Headless base image for agentctl Agents, with Claude Code, Codex and Pi"
  }
}

target "base-desktop" {
  inherits = ["base-headless"]
  target   = "desktop"
  tags     = ["${BASE_REPOSITORY}:${BASE_DISTRIBUTION}-desktop"]
  labels = {
    "org.opencontainers.image.description" = "Desktop base image for agentctl Agents: the headless image plus Chromium and a VNC desktop"
  }
}

group "self-dev" {
  targets = ["self-dev-minimal", "self-dev-full"]
}

# The self-dev images build on the base image from this file rather than the published one, so that CI tests a change
# to the base together with the image that develops the platform.
target "self-dev-minimal" {
  inherits = ["_source"]
  context  = "agents/self-dev"
  target   = "minimal"
  contexts = {
    ("${BASE_REPOSITORY}:${BASE_DISTRIBUTION}") = "target:base-headless"
  }
  tags = ["ghcr.io/digdir/digdir-agents/agentctl-self-dev-minimal:latest"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-self-dev-minimal"
    "org.opencontainers.image.description" = "The agentctl self-development Agent's image"
  }
}

target "self-dev-full" {
  inherits = ["self-dev-minimal"]
  target   = "full"
  tags     = ["ghcr.io/digdir/digdir-agents/agentctl-self-dev-full:latest"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-self-dev-full"
    "org.opencontainers.image.description" = "The agentctl self-development Agent's image, with a browser and a desktop"
  }
}
