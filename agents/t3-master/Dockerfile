# The T3 orchestrator: T3 Code's server and web client (the martinothamar/t3code fork, which manages agentctl
# sandboxes) with agentctl, agentd and Podman, so that it starts worker agents as nested agents and delegates tasks to
# them.
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04

USER root

# libatomic1 for the Node.js that T3 Code's server runs on.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libatomic1 \
    && rm -rf /var/lib/apt/lists/*

# Podman, so the nested agentd can build the workers' images. The `docker` command runs it.
RUN agent-feature install containers

# agentctl and agentd for the nested agents. The main tag follows the platform, as the base image's tag does.
COPY --from=ghcr.io/digdir/agentctl:main /agentctl /agentd /usr/local/bin/

# T3 Code's server, started as a service. The build it runs comes with the home, since it has no release yet, and
# t3-server installs it on first start.
COPY rootfs/ /

# A checkout on Windows has no executable bits.
RUN chmod 0755 /usr/local/libexec/t3-server /usr/local/bin/t3 \
    && chmod 0644 /etc/systemd/system/t3-server.service /etc/systemd/system/agent-ssh-config-init.service \
    && systemctl enable t3-server.service agent-ssh-config-init.service \
    && agentctl --version \
    && agent-image-check

USER agent
