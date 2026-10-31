# The image of the standard agent builder, built on the agentctl base images in images/agentctl-base. FROM names the
# base tag without a digest, so that this image follows the platform it runs on: CI builds it on the base built from
# the same commit (docker-bake.hcl). To build it against a local base, tag that base under localhost/ and point FROM at
# it while you test.
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04

# The base ends as the unprivileged agent user; installing packages needs root again.
USER root

# Podman, with the `docker` command, to build the images of the agents it writes and run them as nested agents.
RUN agent-feature install containers

# agentctl and agentd, to try the agents it writes as nested agents. Like FROM, the main tag follows the platform.
COPY --from=ghcr.io/digdir/agentctl:main /agentctl /agentd /usr/local/bin/

# yq, to read the manifests and CI workflows of the repositories it makes agents for.
ARG TARGETARCH
ARG YQ_VERSION=4.54.1
RUN case "${TARGETARCH}" in \
        amd64) YQ_SHA256=8e34fc298390875de416e6a4afcb8cabeceb25d9aa8506c1a2f9353cf702ea5f ;; \
        arm64) YQ_SHA256=189088da0c6429ec5178dfaab1a114805f6cab0b61b165ab236efedf1d57a71b ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && curl -fsSL "https://github.com/mikefarah/yq/releases/download/v${YQ_VERSION}/yq_linux_${TARGETARCH}" -o /tmp/yq \
    && echo "${YQ_SHA256}  /tmp/yq" | sha256sum -c - \
    && install -m 0755 /tmp/yq /usr/local/bin/yq \
    && rm /tmp/yq

RUN agentctl --version \
    && yq --version \
    && agent-image-check

USER agent
