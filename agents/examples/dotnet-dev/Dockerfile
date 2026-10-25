# The agentctl base image already has Claude Code, Codex, Git, the GitHub CLI and what agentd needs, so an Agent's
# image adds only the tools its work needs. images/agentctl-base/README.md describes what the base offers. The
# desktop variant adds Chromium and the desktop for the VNC access in agent.yaml. Without that, build on
# ghcr.io/digdir/agentctl-base:ubuntu-26.04 instead.
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04-desktop@sha256:ec5ce4ba36f5f8a71078fb02703d4604d28411fc3a035f2ee2c24d1febb3839e

# The base image ends as the agent user. Install as root, then switch back at the end.
USER root

# Ubuntu's .NET SDK, which Ubuntu's security updates keep patched. Add your other tools to this list.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends dotnet-sdk-10.0 \
    && rm -rf /var/lib/apt/lists/*

# No telemetry or first-run banner from the .NET CLI.
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1
ENV DOTNET_NOLOGO=1

# Podman, with the `docker` command and Docker Compose, for Testcontainers and `docker compose`, and for building the
# images of the Agents this Agent runs.
RUN agent-feature install containers

# The repositories to clone into /home/agent/code at boot.
COPY workspace.list /etc/agent-workspace.d/workspace.list

# Checks that the image still has what agentd needs. Keep it last, after every step that writes files.
RUN agent-image-check

USER agent
