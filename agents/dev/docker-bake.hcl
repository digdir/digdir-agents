# The dev agent's images, for `docker buildx bake`. They develop this repository rather than being part of agentctl, so
# they have their own bake file. Paths here are relative to the repository root, where bake runs:
# `docker buildx bake -f agents/dev/docker-bake.hcl --load dev-minimal` builds one on the published base image, as any
# agent's image is built.

# A build context to use in place of the published base image. With the root docker-bake.hcl loaded too,
# `target:base-headless` builds on the base from the same checkout, as CI does for pull requests.
variable "BASE_CONTEXT" {
  default = ""
}

group "dev" {
  targets = ["dev-minimal", "dev-desktop"]
}

target "dev-minimal" {
  context  = "agents/dev"
  target   = "minimal"
  contexts = BASE_CONTEXT == "" ? {} : { "ghcr.io/digdir/agentctl-base:ubuntu-26.04" = BASE_CONTEXT }
  tags     = ["ghcr.io/digdir/digdir-agents/agentctl-dev-minimal:latest"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/digdir/digdir-agents"
    "org.opencontainers.image.title"       = "agentctl-dev-minimal"
    "org.opencontainers.image.description" = "The image of the agent that develops agentctl"
  }
}

target "dev-desktop" {
  inherits = ["dev-minimal"]
  target   = "desktop"
  tags     = ["ghcr.io/digdir/digdir-agents/agentctl-dev-desktop:latest"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-dev-desktop"
    "org.opencontainers.image.description" = "The image of the agent that develops agentctl, with a browser and a desktop"
  }
}
