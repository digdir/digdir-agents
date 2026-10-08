#!/bin/sh
set -eu

# The agent platform and the Microsandbox and libkrunfw forks it builds on,
# cloned beside each other. A checkout that already has `.git`, such as a
# bind-mounted host checkout, is left alone.
missing=
for repository in digdir/digdir-agents digdir/microsandbox digdir/libkrunfw; do
    if [ ! -e "/home/agent/code/${repository#*/}/.git" ]; then
        missing="$missing $repository"
    fi
done
if [ -z "$missing" ]; then
    exit 0
fi
mkdir -p /home/agent/code

# Guest boot can race the host-mediated network handshake. Wait for DNS,
# while leaving each repository operation itself as one best-effort attempt.
remaining=30
while ! /usr/bin/getent ahosts github.com >/dev/null 2>&1; do
    if [ "$remaining" -eq 0 ]; then
        echo "github.com did not become resolvable within 30 seconds" >&2
        exit 1
    fi
    remaining=$((remaining - 1))
    /usr/bin/sleep 1
done

status=0
for repository in $missing; do
    /usr/local/bin/gh repo clone "$repository" "/home/agent/code/${repository#*/}" || status=1
done
exit "$status"
