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
