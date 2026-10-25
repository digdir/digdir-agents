#!/bin/sh
# Downloads the Microsandbox runtime bundles of a digdir/microsandbox commit before they are
# released, for AGENT_MICROSANDBOX_RUNTIME_DIR (see sandbox/docs/microsandbox-upgrade.md).
#
# The fork's checks assemble the release assets of every tree they build and keep them as the
# `release-<tree>` artifact. Its release later publishes the assets of the oldest successful
# check run of the landed tree, so this script takes that run's, the bundles whose digests a pin
# update copies into client.rs. Installing still checks a bundle against the pinned digest, so
# these files cannot replace the runtime the pin names.
#
# Usage:
#   fetch-runtime.sh <directory> [commit]
#       Download the release assets into <directory>, which must not exist yet, check them
#       against their checksums.sha256, and print each bundle's digest and whether client.rs pins
#       it. `commit` defaults to the Microsandbox revision in Cargo.lock.
#
# Needs gh, logged in with read access to digdir/microsandbox's Actions, or GH_TOKEN.

set -eu

REPOSITORY=digdir/microsandbox
ROOT="$(cd -- "$(dirname -- "$0")/../.." && pwd)"
CLIENT="${ROOT}/sandbox/microsandbox/src/client.rs"

fail() {
  printf 'fetch-runtime.sh: %s\n' "$*" >&2
  exit 1
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || fail "usage: fetch-runtime.sh <directory> [commit]"
directory="$1"
[ ! -e "${directory}" ] || fail "${directory} already exists"

commit="${2:-$(awk '/^name = "microsandbox"$/ { found = 1 } found && /^source = / { sub(/.*#/, ""); sub(/"$/, ""); print; exit }' "${ROOT}/Cargo.lock")}"
[ -n "${commit}" ] || fail "Cargo.lock pins no microsandbox revision"

tree="$(gh api "repos/${REPOSITORY}/git/commits/${commit}" --jq .tree.sha)"
name="release-${tree}"
repository_id="$(gh api "repos/${REPOSITORY}" --jq .id)"

# The same choice as the fork's release-digdir.yml: the oldest successful check-digdir.yml run in
# the repository itself. A newer run has other digests, so an unfinished older one is waited for.
run_id=""
for id in $(gh api "repos/${REPOSITORY}/actions/artifacts?name=${name}&per_page=100" \
  --jq "[.artifacts[] | select((.expired | not) and .workflow_run.head_repository_id == ${repository_id})]
    | sort_by(.created_at) | .[].workflow_run.id"); do
  run="$(gh api "repos/${REPOSITORY}/actions/runs/${id}" --jq '"\(.status) \(.conclusion) \(.path)"')"
  case "${run}" in
    "completed success .github/workflows/check-digdir.yml") run_id="${id}"; break ;;
    "completed "*" .github/workflows/check-digdir.yml") ;;
    *" .github/workflows/check-digdir.yml") fail "check run ${id} of ${commit} has not finished; try again once it has" ;;
  esac
done
[ -n "${run_id}" ] || fail "no successful check-digdir.yml run in ${REPOSITORY} built ${commit} (${name})"

printf 'Downloading %s from https://github.com/%s/actions/runs/%s\n' "${name}" "${REPOSITORY}" "${run_id}"
gh run download "${run_id}" --repo "${REPOSITORY}" --name "${name}" --dir "${directory}"

if command -v sha256sum > /dev/null; then
  (cd -- "${directory}" && sha256sum --check --strict --quiet checksums.sha256)
else
  (cd -- "${directory}" && shasum -a 256 --check --strict --quiet checksums.sha256)
fi

awk '$2 ~ /^microsandbox-.*\.tar\.gz$/ { print $1, $2 }' "${directory}/checksums.sha256" |
  while read -r digest bundle; do
    if grep -q "\"${digest}\"" "${CLIENT}"; then
      printf '%s %s (pinned)\n' "${digest}" "${bundle}"
    else
      printf '%s %s (not pinned in client.rs)\n' "${digest}" "${bundle}"
    fi
  done
