# The self-development agent's images, for `docker buildx bake`. They develop this repository rather than being part of
# agentctl, so they have their own bake file. Paths here are relative to the repository root, where bake runs:
# `docker buildx bake -f agents/self-dev/docker-bake.hcl --load self-dev-minimal` builds one on the published base
# image, as any agent's image is built.

group "self-dev" {
  targets = ["self-dev-minimal", "self-dev-full"]
}

target "self-dev-minimal" {
  context  = "agents/self-dev"
  target   = "minimal"
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
