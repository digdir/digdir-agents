# The image of the standard ASP.NET Core backend agent, built on the agentctl base images in images/agentctl-base. FROM
# names the base tag without a digest, so that this image follows the platform it runs on: CI builds it on the base
# built from the same commit (docker-bake.hcl). To build it against a local base, tag that base under localhost/ and
# point FROM at it while you test.
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04

# The base ends as the unprivileged agent user; installing packages needs root again.
USER root

# Podman, with the `docker` command and Docker Compose, for what the agent runs in containers.
RUN agent-feature install containers

# No telemetry or first-run banner from the .NET CLI. Local development runs over HTTP, so the SDK does not generate
# a development certificate the host would not trust anyway.
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1
ENV DOTNET_NOLOGO=1
ENV DOTNET_GENERATE_ASPNET_CERTIFICATE=false

# The SDKs of the two supported long-term .NET releases. A global.json's rollForward never crosses a major, so each
# needs an SDK of its own. Ubuntu has .NET 10. .NET 8 comes from the backports archive of Canonical's .NET team,
# ppa:dotnet/backports, signed with the key in dotnet-backports.asc.
COPY dotnet-backports.asc /etc/apt/keyrings/dotnet-backports.asc
RUN . /etc/os-release \
    && printf '%s\n' \
        'Types: deb' \
        'URIs: https://ppa.launchpadcontent.net/dotnet/backports/ubuntu' \
        "Suites: ${VERSION_CODENAME}" \
        'Components: main' \
        'Signed-By: /etc/apt/keyrings/dotnet-backports.asc' \
        >/etc/apt/sources.list.d/dotnet-backports.sources \
    && apt-get update \
    && apt-get install --yes --no-install-recommends dotnet-sdk-8.0 dotnet-sdk-10.0 \
    && rm -rf /var/lib/apt/lists/* \
    && dotnet --list-sdks | grep -q '^8\.' \
    && dotnet --list-sdks | grep -q '^10\.'

RUN agent-image-check

USER agent
