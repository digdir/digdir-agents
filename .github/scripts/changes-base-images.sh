#!/usr/bin/env bash
# Exits 0 when the commits from BEFORE to AFTER change what agentctl-base-images.yml publishes on, and 1 when they
# don't. A workflow that builds on the published base images leaves such a push to the base images' run, which starts
# it again once it has published them, since building right away would use the base from before the push. Needs gh,
# logged in, and GITHUB_REPOSITORY.
set -euo pipefail

before="${1:?usage: changes-base-images.sh BEFORE AFTER}"
after="${2:?usage: changes-base-images.sh BEFORE AFTER}"

# The push paths of agentctl-base-images.yml. Kept in step by hand: a path missing here costs only a build on the old
# base, which the base images' run then replaces.
base='^(images/agentctl-base/|\.github/workflows/agentctl-base-images\.yml$)'
base_docs='^images/agentctl-base/[^/]*\.md$'

changed="$(gh api "repos/${GITHUB_REPOSITORY}/compare/${before}...${after}" \
  --jq '.files[] | .filename, (.previous_filename // empty)')"
relevant="$(grep -E "${base}" <<< "${changed}" | grep -v -E "${base_docs}" || true)"
[ -n "${relevant}" ]
