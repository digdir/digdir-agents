# The images that are part of agentctl, published under ghcr.io/digdir, for `docker buildx bake`: the base images agents
# build on, the image of agentctl and agentd, and the agent builder's. This repository's own agent has its own bake
# file, agents/dev/docker-bake.hcl. CI builds each architecture on its own runner and publishes from main;
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

# The sign-in images share the headless image's layers for the tools they hold, which only holds when one build makes
# them all.
group "base" {
  targets = ["base-headless", "base-desktop", "base-sign-in-claude", "base-sign-in-codex", "base-sign-in-github"]
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

# The images agentctl's sign-ins run in when the machine does not have the program for one, published in the base
# images' repository, which holds the layers they share.
target "base-sign-in" {
  inherits = ["_source"]
  context  = "images/agentctl-base"
  name     = "base-sign-in-${service}"
  matrix = {
    service = ["claude", "codex", "github"]
  }
  target = "sign-in-${service}"
  tags   = ["${BASE_REPOSITORY}:${BASE_DISTRIBUTION}-sign-in-${service}"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-base"
    "org.opencontainers.image.description" = "The image agentctl's ${service} sign-in runs in when the machine does not have the program for it"
    "org.opencontainers.image.version"     = AGENT_VERSION
    "no.digdir.agentctl.base.version"      = AGENT_VERSION
  }
}

# A build context to use in place of the published base image for the agent builder, such as `target:base-headless` to
# build it on the base from the same checkout, as CI does for pull requests, or a release's base image by digest.
variable "BASE_CONTEXT" {
  default = ""
}

# A build context to use in place of `ghcr.io/digdir/agentctl:main` for the agent builder, such as a release's agentctl
# image by digest, so that the builder a release publishes holds that release's agentctl.
variable "AGENTCTL_CONTEXT" {
  default = ""
}

# The agent builder's image, which `agentctl new` runs to make a new agent. It builds on the published base image and
# copies agentctl and agentd from the published agentctl image.
target "agent-builder" {
  inherits = ["_source"]
  context  = "images/agentctl-agent-builder"
  contexts = merge(
    BASE_CONTEXT == "" ? {} : { ("${BASE_REPOSITORY}:${BASE_DISTRIBUTION}") = BASE_CONTEXT },
    AGENTCTL_CONTEXT == "" ? {} : { "ghcr.io/digdir/agentctl:main" = AGENTCTL_CONTEXT },
  )
  tags     = ["ghcr.io/digdir/agentctl-agent-builder:main"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-agent-builder"
    "org.opencontainers.image.description" = "The agent builder's image, which agentctl new runs to make a new agent"
    "org.opencontainers.image.version"     = AGENT_VERSION
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
