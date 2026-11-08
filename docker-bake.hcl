# The images that are part of agentctl, published under ghcr.io/digdir, for `docker buildx bake`: the base images agents
# build on and the image of agentctl and agentd. This repository's own agent has its own bake file,
# agents/self-dev/docker-bake.hcl. CI builds each architecture on its own runner and publishes from main;
# `docker buildx bake -f docker-bake.hcl --load base-headless` builds one locally under its published name. Always pass
# `-f docker-bake.hcl`: without it, bake also reads the root docker-compose.yml.

# The agentctl version the images belong to, which their labels carry and the agentctl image's binaries report. CI sets
# the release's, or `<latest release>.main.g<commit>` for any other build.
variable "AGENT_VERSION" {
  default = "v0.0.1-dev"
}

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
    "org.opencontainers.image.description" = "Headless base image for agentctl agents, with Claude Code, Codex and Pi"
    "org.opencontainers.image.version"     = AGENT_VERSION
    # Images built on the base inherit its labels, and may set the OCI version to their own. This one keeps saying
    # which agentctl version their base belongs to.
    "no.digdir.agentctl.base.version" = AGENT_VERSION
  }
}

target "base-desktop" {
  inherits = ["base-headless"]
  target   = "desktop"
  tags     = ["${BASE_REPOSITORY}:${BASE_DISTRIBUTION}-desktop"]
  labels = {
    "org.opencontainers.image.description" = "Desktop base image for agentctl agents: the headless image plus Chromium and a VNC desktop"
  }
}

# agentctl and agentd alone, for agent images to copy in. Built from the repository root; locally under a localhost/
# tag, since Podman pulls a published name over a local image when agentd builds for a platform.
target "agentctl" {
  inherits   = ["_source"]
  context    = "."
  dockerfile = "images/agentctl/Dockerfile"
  args = {
    AGENT_VERSION = AGENT_VERSION
  }
  tags = ["localhost/agentctl:dev"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl"
    "org.opencontainers.image.description" = "agentctl and agentd, for agent images to copy in"
    "org.opencontainers.image.version"     = AGENT_VERSION
  }
}
