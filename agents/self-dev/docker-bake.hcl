# The self-development agent's images, for `docker buildx bake`. They develop this repository rather than being part of
# agentctl, so they have their own bake file. Paths here are relative to the repository root, where bake runs:
# `docker buildx bake -f agents/self-dev/docker-bake.hcl --load self-dev-minimal` builds one on the published base
# image, as any agent's image is built.

# A build context to use in place of the published base image. With the root docker-bake.hcl loaded too,
# `target:base-headless` builds on the base from the same checkout, as CI does for pull requests.
variable "BASE_CONTEXT" {
  default = ""
}

group "self-dev" {
  targets = ["self-dev-minimal", "self-dev-full"]
}

target "self-dev-minimal" {
  context  = "agents/self-dev"
  target   = "minimal"
  contexts = BASE_CONTEXT == "" ? {} : { "ghcr.io/digdir/agentctl-base:ubuntu-26.04" = BASE_CONTEXT }
  tags     = ["ghcr.io/digdir/digdir-agents/agentctl-self-dev-minimal:latest"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/digdir/digdir-agents"
    "org.opencontainers.image.title"       = "agentctl-self-dev-minimal"
    "org.opencontainers.image.description" = "The agentctl self-development agent's image"
  }
}

target "self-dev-full" {
  inherits = ["self-dev-minimal"]
  target   = "full"
  tags     = ["ghcr.io/digdir/digdir-agents/agentctl-self-dev-full:latest"]
  labels = {
    "org.opencontainers.image.title"       = "agentctl-self-dev-full"
    "org.opencontainers.image.description" = "The agentctl self-development agent's image, with a browser and a desktop"
  }
}
