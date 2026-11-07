#!/usr/bin/env bash
# Smoke-tests what this image adds to the agentctl base image, inside a running agent: agentctl and agentd, yq, and
# Podman building and running an image, as trying an agent it writes does. It needs the agent's booted systemd and
# Podman socket, so `docker run` against the image alone cannot run it. From this directory:
#
#   agentctl apply --name agent-builder-smoke --wait
#   agentctl exec agent-builder-smoke -- bash -c "$(cat smoke.sh)"
#
# That boots the published image. To test a change to the Dockerfile, apply a variant of your own, an agent.<name>.yaml
# that Git ignores, whose image is `type: build` with `context: .`, and pass `--variant <name>` instead of `--name`.
#
# Runs as the agent user and removes what it starts.
set -euo pipefail

work="$(mktemp -d)"
image="localhost/agent-builder-smoke:latest"
cleanup() {
    docker image rm --force "$image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
cd "$work"
fail() { echo "smoke: $*" >&2; exit 1; }

echo "## tools"
test "$(id -un)" = agent || fail "expected to run as agent, got $(id -un)"
agentctl --version
agentd --version
yq --version

echo "## building and running an image"
cat > Dockerfile <<'EOF'
FROM docker.io/library/alpine:3
RUN echo built > /built
CMD ["cat", "/built"]
EOF
docker build --quiet --tag "$image" . >/dev/null
docker run --rm "$image" | grep -qx built || fail "the image built here did not run"

echo "## nested agents"
if [ -e /dev/kvm ]; then
    echo "/dev/kvm is present"
else
    echo "/dev/kvm is missing: this host runs no nested agents, so the agent says what it could not try"
fi

echo "smoke: ok"
