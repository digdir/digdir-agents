#!/usr/bin/env bash
# Prints the version a build from main reports, for a commit: `<latest release>.main.g<commit>`, such as
# v0.1.0-preview.13.main.g1a2b3c4, which sorts after the release it follows and before the next one. Needs gh, logged
# in, and GITHUB_REPOSITORY.
set -euo pipefail

commit="${1:?usage: main-version.sh COMMIT}"

# The latest release as the installers resolve it: the newest agentctl/ tag.
tags="$(gh api --paginate "repos/${GITHUB_REPOSITORY}/releases?per_page=100" \
  --jq '.[] | select(.tag_name | startswith("agentctl/v")) | .tag_name')"
latest="$(head -n 1 <<< "${tags}")"
base="${latest#agentctl/}"
if ! printf '%s\n' "${base}" | grep -q -E '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
  echo "::error::no agentctl release to base the version on: '${latest}'" >&2
  exit 1
fi

# After a preview it extends the preview, and after a stable release it is a prerelease of the next patch. The g keeps
# a commit that has only digits and starts with 0 from being an invalid semantic version identifier.
case "${base}" in
  *-*) echo "${base}.main.g${commit::7}" ;;
  *)
    patch="${base##*.}"
    echo "${base%.*}.$((patch + 1))-main.g${commit::7}"
    ;;
esac
