#!/bin/sh
# Tests for how install.sh resolves the latest release when AGENT_VERSION is unset. A stub `curl`
# serves release pages from fixtures and records every request, and a stub `uname` fixes the
# platform; the installer stops at the first archive download, which names the resolved version.
# Nothing is installed: every installation path is under a temporary directory.
#
# Usage: install_test.sh   (or `make install-test`)
#
# INSTALL_TEST_SHELL selects the shell that runs install.sh (default `sh`). Bash enters POSIX mode
# when POSIXLY_CORRECT is set: `INSTALL_TEST_SHELL=bash POSIXLY_CORRECT=1 install_test.sh`.

set -eu

SCRIPT_DIRECTORY="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
INSTALLER="${SCRIPT_DIRECTORY}/install.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT HUP INT TERM

failures=0
checks=0

mkdir "${WORK}/bin"
cat >"${WORK}/bin/curl" <<'STUB'
#!/bin/sh
for argument in "$@"; do
  case "${argument}" in https://*) url="${argument}" ;; esac
done
case "${url}" in
  https://api.github.com/repos/digdir/digdir-agents/releases\?per_page=100\&page=*)
    page="${url##*page=}"
    printf 'page %s\n' "${page}" >>"${INSTALL_TEST_CASE}/requests"
    if [ -f "${INSTALL_TEST_CASE}/page-${page}.error" ]; then
      echo "curl: (22) The requested URL returned error: $(cat "${INSTALL_TEST_CASE}/page-${page}.error")" >&2
      exit 22
    fi
    if [ -f "${INSTALL_TEST_CASE}/page-${page}.json" ]; then
      cat "${INSTALL_TEST_CASE}/page-${page}.json"
      exit 0
    fi
    echo "curl: (22) The requested URL returned error: 422" >&2
    exit 22
    ;;
esac
printf 'download %s\n' "${url}" >>"${INSTALL_TEST_CASE}/requests"
echo "curl: (22) The requested URL returned error: 404" >&2
exit 22
STUB
cat >"${WORK}/bin/uname" <<'STUB'
#!/bin/sh
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
esac
STUB
chmod 0755 "${WORK}/bin/curl" "${WORK}/bin/uname"

# page <case> <number> — reads a release page from standard input.
page() {
  mkdir -p "${WORK}/$1"
  cat >"${WORK}/$1/page-$2.json"
}

# page_error <case> <number> <status> — makes the request for a release page fail with <status>.
page_error() {
  mkdir -p "${WORK}/$1"
  printf '%s\n' "$3" >"${WORK}/$1/page-$2.error"
}

# run_installer <case> — runs the installer against the case's release pages, in the locale
# ${installer_locale} when it is set, and prints its requests.
installer_locale=""
run_installer() {
  case_directory="${WORK}/$1"
  mkdir -p "${case_directory}"
  : >"${case_directory}/requests"
  status=0
  (
    unset AGENT_VERSION AGENT_LOCAL_ARCHIVE AGENT_INSTALL_MODE AGENT_GITHUB_REPOSITORY
    if [ -n "${installer_locale}" ]; then
      LC_ALL="${installer_locale}"
      export LC_ALL
    fi
    HOME="${case_directory}/home" \
      AGENT_HOME="${case_directory}/agent-home" \
      AGENT_INSTALL_ROOT="${case_directory}/install" \
      AGENT_INSTALL_DIR="${case_directory}/bin" \
      INSTALL_TEST_CASE="${case_directory}" \
      TMPDIR="${case_directory}" \
      PATH="${WORK}/bin:${PATH}" \
      "${INSTALL_TEST_SHELL:-sh}" "${INSTALLER}"
  ) >"${case_directory}/stdout" 2>"${case_directory}/stderr" || status=$?
  printf '%s\n' "${status}" >"${case_directory}/status"
  cat "${case_directory}/requests"
}

download() {
  printf 'download https://github.com/digdir/digdir-agents/releases/download/agentctl/%s/agent-linux-x86_64.tar.gz\n' "$1"
}

# assert_requests <name> <case> <expected requests> [<stderr substring>]
# The installer never succeeds here: at the latest, it stops at the archive download.
assert_requests() {
  checks=$((checks + 1))
  actual="$(run_installer "$2")"
  if [ "$(cat "${WORK}/$2/status")" -eq 0 ]; then
    failures=$((failures + 1))
    printf 'FAIL %s: the installer succeeded\n' "$1"
  elif [ "${actual}" != "$3" ]; then
    failures=$((failures + 1))
    printf 'FAIL %s: unexpected requests\n' "$1"
    printf '     expected:\n'
    printf '%s\n' "$3" | sed 's/^/       /'
    printf '     actual:\n'
    printf '%s\n' "${actual}" | sed 's/^/       /'
    sed 's/^/     /' "${WORK}/$2/stderr"
  elif [ -n "${4:-}" ] && ! grep -qF "$4" "${WORK}/$2/stderr"; then
    failures=$((failures + 1))
    printf 'FAIL %s: stderr did not mention "%s"\n' "$1" "$4"
    sed 's/^/     /' "${WORK}/$2/stderr"
  else
    printf 'ok   %s\n' "$1"
  fi
}

# Compact JSON has every release on one line. The body's escaped tag must not count as a tag.
printf '%s' '[{"id":5,"tag_name":"web/v2.0.0","name":"web","body":"Not \"tag_name\": \"agentctl/v9.9.9\""},{"id":4,"tag_name":"agentctl/v0.1.0-preview.10","assets":[{"name":"agent-linux-x86_64.tar.gz"}]},{"id":3,"tag_name":"web/v1.0.0"},{"id":2,"tag_name":"agentctl/v0.1.0-preview.9"}]' \
  | page compact 1
assert_requests 'compact JSON resolves the newest agentctl release' compact \
  "page 1
$(download v0.1.0-preview.10)"

page pretty 1 <<'JSON'
[
  {
    "id": 5,
    "tag_name": "web/v2.0.0",
    "name": "web"
  },
  {
    "id": 4,
    "tag_name": "agentctl/v0.1.0-preview.10",
    "assets": [
      {
        "name": "agent-linux-x86_64.tar.gz"
      }
    ]
  },
  {
    "id": 2,
    "tag_name": "agentctl/v0.1.0-preview.9"
  }
]
JSON
assert_requests 'pretty-printed JSON resolves the newest agentctl release' pretty \
  "page 1
$(download v0.1.0-preview.10)"

printf '%s' '[{"tag_name":"web/v2.0.0"},{"tag_name":"web/v1.0.0"}]' | page second-page 1
printf '%s' '[{"tag_name" : "web/v0.9.0"},{"tag_name" : "agentctl/v0.1.0-preview.1"}]' | page second-page 2
assert_requests 'a later page resolves the newest agentctl release' second-page \
  "page 1
page 2
$(download v0.1.0-preview.1)"

printf '%s' '[{"tag_name":"web/v2.0.0"}]' | page none 1
printf '[\n\n]\n' | page none 2
assert_requests 'an empty page ends the lookup without an agentctl release' none \
  "page 1
page 2" 'Could not resolve the latest agentctl release'

printf '[]' | page empty 1
assert_requests 'no releases at all ends the lookup' empty \
  "page 1" 'Could not resolve the latest agentctl release'

page_error first-page-fails 1 403
assert_requests 'a failed first page request stops the installer' first-page-fails \
  "page 1" 'The requested URL returned error: 403'

printf '%s' '[{"tag_name":"web/v2.0.0"}]' | page second-page-fails 1
page_error second-page-fails 2 403
assert_requests 'a failed later page request stops the installer' second-page-fails \
  "page 1
page 2" 'The requested URL returned error: 403'

# GitHub lists releases newest first, so the first agentctl release wins over a higher version.
printf '%s' '[{"tag_name":"agentctl/v0.1.0-preview.11"},{"tag_name":"agentctl/v0.1.0"}]' | page api-order 1
assert_requests 'the first agentctl release in API order wins' api-order \
  "page 1
$(download v0.1.0-preview.11)"

printf '%s' '[{"tag_name":"agentctl/v0.1.0"},{"tag_name":"agentctl/v0.1.0-preview.11"}]' | page api-order-reversed 1
assert_requests 'the first agentctl release in API order wins when it has the higher version' \
  api-order-reversed "page 1
$(download v0.1.0)"

# A byte that is not valid UTF-8 after the tag must not hide it in a UTF-8 locale.
if locale -a 2>/dev/null | grep -Eiqx 'c\.utf-?8'; then
  printf '[{"tag_name":"agentctl/v0.1.0-preview.10","body":"\377"}]' | page invalid-utf-8 1
  installer_locale=C.UTF-8
  assert_requests 'a byte that is not valid UTF-8 does not hide the release' invalid-utf-8 \
    "page 1
$(download v0.1.0-preview.10)"
  installer_locale=""
else
  printf 'skip a byte that is not valid UTF-8 does not hide the release: no C.UTF-8 locale\n'
fi

printf '\n%d checks, %d failures\n' "${checks}" "${failures}"
[ "${failures}" -eq 0 ]
