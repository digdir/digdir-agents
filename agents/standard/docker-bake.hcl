# The standard agents' images, for `docker buildx bake`. Like the self-dev images, they are agents rather than part of
# agentctl, so they have their own bake file. Paths here are relative to the repository root, where bake runs:
# `docker buildx bake -f agents/standard/docker-bake.hcl --load standard-dotnet-backend-developer` builds one on the
# published base image, as any agent's image is built.

# A build context to use in place of the published base image. With the root docker-bake.hcl loaded too,
# `target:base-headless` builds on the base from the same checkout, as CI does for pull requests.
variable "BASE_CONTEXT" {
  default = ""
}

group "standard" {
  targets = ["standard-dotnet-backend-developer"]
}

target "standard-dotnet-backend-developer" {
  context  = "agents/standard/dotnet-backend-developer"
  contexts = BASE_CONTEXT == "" ? {} : { "ghcr.io/digdir/agentctl-base:ubuntu-26.04" = BASE_CONTEXT }
  tags     = ["ghcr.io/digdir/digdir-agents/agent-dotnet-backend-developer:latest"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/digdir/digdir-agents"
    "org.opencontainers.image.title"       = "agent-dotnet-backend-developer"
    "org.opencontainers.image.description" = "The standard ASP.NET Core backend agent's image"
  }
}
