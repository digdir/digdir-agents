#!/bin/sh
# agent-session: lists this agent's sessions by name, or attaches to one.
# Installed by agentd; changes are replaced on the next session launch.
set -eu
tmux=/usr/bin/tmux

sessions() {
    "$tmux" list-sessions -F '#{@agent_session_name}	#{session_name}' 2>/dev/null |
        awk -F '\t' '$1 != ""'
}

case "${1:-}" in
-h | --help)
    echo "usage: agent-session          list this agent's sessions"
    echo "       agent-session NAME     attach to the session named NAME"
    ;;
"")
    [ -n "${AGENT_NAME:-}" ] && echo "agent: $AGENT_NAME${AGENT_HOST:+ on $AGENT_HOST}"
    sessions | while IFS="$(printf '\t')" read -r name id; do
        if [ "$name" = "${AGENT_SESSION_NAME:-}" ]; then
            echo "* $name (this session)"
        else
            echo "  $name"
        fi
    done
    ;;
*)
    id=$(sessions | awk -F '\t' -v name="$1" '$1 == name { print $2; exit }')
    if [ -z "$id" ]; then
        echo "agent-session: no session named $1" >&2
        exit 1
    fi
    if [ -n "${TMUX:-}" ]; then
        exec "$tmux" switch-client -t "=$id"
    fi
    exec "$tmux" attach-session -t "=$id"
    ;;
esac
