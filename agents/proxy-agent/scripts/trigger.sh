#!/usr/bin/env bash
# Appender et trigger-event til triggers/inbox.jsonl.
# Bruk: ./scripts/trigger.sh "<prompt>" [source] [type]
set -euo pipefail

prompt="${1:?Bruk: trigger.sh \"<prompt>\" [source] [type]}"
source_="${2:-manual}"
type_="${3:-message}"
id="${source_}-$(date +%s%3N)"
inbox="$(dirname "$0")/../triggers/inbox.jsonl"

jq -cn \
  --arg id "$id" \
  --arg source "$source_" \
  --arg type "$type_" \
  --arg received_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg prompt "$prompt" \
  '{id: $id, source: $source, type: $type, received_at: $received_at, prompt: $prompt}' \
  >>"$inbox"

echo "Event '$id' lagt i køen."
