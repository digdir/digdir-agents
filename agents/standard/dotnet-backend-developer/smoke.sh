#!/usr/bin/env bash
# Smoke-tests what this image adds to the agentctl base image, inside a running agent: the .NET SDKs, and Podman
# serving containers, Docker Compose and the Docker API socket. It needs the agent's booted systemd and Podman socket,
# so `docker run` against the image alone cannot run it. From this directory:
#
#   agentctl apply --name dotnet-backend-smoke --wait
#   agentctl exec dotnet-backend-smoke -- bash -c "$(cat smoke.sh)"
#
# That boots the published image. To test a change to the Dockerfile, apply a variant of your own, an agent.<name>.yaml
# that Git ignores, whose image is `type: build` with `context: .`, and pass `--variant <name>` instead of `--name`.
#
# Runs as the agent user and removes what it starts.
set -euo pipefail

work="$(mktemp -d)"
cleanup() {
    if [ -f "$work/compose.yaml" ]; then
        docker compose --file "$work/compose.yaml" down --volumes >/dev/null 2>&1 || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT
cd "$work"
fail() { echo "smoke: $*" >&2; exit 1; }

echo "## .NET"
test "$(id -un)" = agent || fail "expected to run as agent, got $(id -un)"
dotnet --list-sdks
for major in 8 10; do
    dotnet new console --name "App${major}" --output "App${major}" --framework "net${major}.0" >/dev/null
    dotnet run --project "App${major}" | grep -qx "Hello, World!" || fail ".NET ${major} did not run"
done

echo "## containers"
docker run --rm docker.io/library/hello-world | grep -q "Hello from Docker!" || fail "hello-world did not run"
curl -fsS --unix-socket /run/docker.sock http://docker/_ping | grep -qx OK || fail "the Docker API socket does not answer"

echo "## docker compose"
cat > compose.yaml <<'EOF'
services:
  web:
    image: docker.io/library/nginx:alpine
    ports:
      - "58080:80"
    healthcheck:
      test: ["CMD", "wget", "-q", "-O", "/dev/null", "http://127.0.0.1/"]
      interval: 1s
      retries: 30
EOF
docker compose up --detach --wait web
curl -fsS http://localhost:58080/ | grep -q nginx || fail "the compose service did not answer"
docker compose down --volumes

echo "smoke: ok"
