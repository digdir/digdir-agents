#!/usr/bin/env bash
# Entrypoint for junior-kodeagenten. Behandler NØYAKTIG ETT event per kjøring:
# leser event-JSON fra EVENT_FILE, kjører Claude Code headless i
# topic-workspacet (/workspace), og skriver resultatlinje + logg til
# /triggers. Sesjonsstate ligger i workspacet (CLAUDE_CONFIG_DIR), så
# oppfølgingsevents i samme topic gjenopptar samme samtale.
set -euo pipefail

TRIGGERS_DIR="${TRIGGERS_DIR:-/triggers}"
WORKSPACE="${WORKSPACE:-/workspace}"
KNOWLEDGE_DIR="${KNOWLEDGE_DIR:-/knowledge}"
RESULT_FILE="${RESULT_FILE:-$TRIGGERS_DIR/results.jsonl}"
LOG_DIR="${LOG_DIR:-$TRIGGERS_DIR/logs}"
EVENT_FILE="${EVENT_FILE:?EVENT_FILE må peke på fila med eventets JSON}"

log() {
  printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

event_json=$(cat "$EVENT_FILE")
if ! jq -e . >/dev/null 2>&1 <<<"$event_json"; then
  log "EVENT_FILE inneholder ikke gyldig JSON — avbryter uten resultatlinje"
  exit 1
fi

id=$(jq -r '.id // empty' <<<"$event_json")
[[ -n "$id" ]] || id="evt-$(date +%s%N)"
id="${id//[^a-zA-Z0-9._-]/_}"

mkdir -p "$LOG_DIR"
log_file="$LOG_DIR/$id.log"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Resultatlinja skrives alltid — også når noe uventet feiler underveis.
# Uten den blir eventet stående som ubehandlet og runneren spinner.
result_written=0
append_result() { # $1=status $2=exit_code $3=reply
  local finished
  finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -cn \
    --arg id "$id" \
    --arg status "$1" \
    --argjson exit_code "$2" \
    --arg log "logs/$id.log" \
    --arg reply "$3" \
    --arg started_at "$started" \
    --arg finished_at "$finished" \
    '{id: $id, status: $status, exit_code: $exit_code, log: $log,
      intent: "action", reply: $reply,
      started_at: $started_at, finished_at: $finished_at}' \
    >>"$RESULT_FILE"
  result_written=1
}
on_exit() {
  local rc=$?
  if (( result_written == 0 )); then
    append_result error "$rc" \
      "Kjøringen feilet uventet (exit $rc) før noe svar ble produsert — se loggen logs/$id.log."
  fi
}
trap on_exit EXIT

# --- Git-/GitHub-identitet: agentens eget scopede token, aldri operatørens ---
if [[ -n "${GH_TOKEN:-}" ]]; then
  gh auth setup-git >/dev/null 2>&1 || log "gh auth setup-git feilet — git-push kan mangle auth"
fi
git config --global user.name  "${GIT_USER_NAME:-local-cc-jr-developer}"
git config --global user.email "${GIT_USER_EMAIL:-local-cc-jr-developer@users.noreply.github.com}"
# Bind-mounts fra hosten kan ha annen eier enn container-brukeren
git config --global --add safe.directory "$KNOWLEDGE_DIR"
git config --global --add safe.directory "$WORKSPACE"

# --- Sesjonsstate per topic: alt Claude Code-state ligger i workspacet ---
export CLAUDE_CONFIG_DIR="$WORKSPACE/.claude"
mkdir -p "$CLAUDE_CONFIG_DIR"
session_file="$CLAUDE_CONFIG_DIR/last-session-id"

# Instruksen lastes som CLAUDE.md i arbeidskatalogen; imagets versjon vinner,
# slik at oppdaterte instrukser når gamle topic-workspaces.
cp -f /opt/agent/CLAUDE.md "$WORKSPACE/CLAUDE.md"

prompt_text=$(jq -r '.prompt // empty' <<<"$event_json")
prompt=$(printf '%s\n\nKontekst – komplett trigger-event (JSON):\n%s\n\n---\nDu kjører headless i en engangs-container. Den SISTE meldingen din i denne kjøringen postes som svar til brukeren i den opprinnelige Slack-/GitHub-tråden: skriv den kort, på norsk, og pek på PR-en når en finnes (URL fra ekte gh-output). Instruksene dine ligger i CLAUDE.md i arbeidskatalogen.\n' \
  "${prompt_text:-Utfør oppgaven eventet beskriver.}" "$event_json")

run_claude() { # $@ = ekstra argumenter (f.eks. --resume <id>)
  local rc
  set +e
  claude -p --output-format stream-json --verbose \
    --dangerously-skip-permissions "$@" "$prompt" >>"$log_file" 2>&1
  rc=$?
  set -e
  return $rc
}

extract_result_json() {
  # Siste stream-json-linje med type "result" — inneholder svartekst,
  # session_id og feilstatus.
  grep -a '"type":"result"' "$log_file" 2>/dev/null | tail -n 1 | \
    { read -r line || true; \
      if [[ -n "${line:-}" ]] && jq -e . >/dev/null 2>&1 <<<"$line"; then printf '%s' "$line"; fi; }
}

log "Behandler event $id (workspace: $WORKSPACE, logg: $log_file)"
resume_args=()
if [[ -s "$session_file" ]]; then
  resume_args=(--resume "$(<"$session_file")")
  log "Gjenopptar sesjon $(<"$session_file")"
fi

exit_code=0
run_claude "${resume_args[@]}" || exit_code=$?

result_json=$(extract_result_json)
if (( exit_code != 0 )) && [[ -z "$result_json" && ${#resume_args[@]} -gt 0 ]]; then
  # Sesjonen kan være borte (slettet state, imageskifte) — prøv én gang ferskt
  log "Resume feilet (exit $exit_code) — prøver på nytt med fersk sesjon"
  : >"$session_file"
  exit_code=0
  run_claude || exit_code=$?
  result_json=$(extract_result_json)
fi

reply=""
is_error="false"
if [[ -n "$result_json" ]]; then
  reply=$(jq -r '.result // empty' <<<"$result_json")
  is_error=$(jq -r '.is_error // false' <<<"$result_json")
  session_id=$(jq -r '.session_id // empty' <<<"$result_json")
  [[ -z "$session_id" ]] || printf '%s' "$session_id" >"$session_file"
fi

status=ok
if (( exit_code != 0 )) || [[ "$is_error" == "true" ]]; then
  status=error
fi
if [[ -z "$reply" ]]; then
  # Aldri rå logg som svar (jf. issue #83) — pek på loggen i stedet.
  if [[ "$status" == ok ]]; then
    reply="Kjøringen fullførte, men ga ingen svartekst — se loggen logs/$id.log."
  else
    reply="Kjøringen feilet (exit $exit_code) — se loggen logs/$id.log."
  fi
fi

append_result "$status" "$exit_code" "$reply"
log "Event $id ferdig (status=$status, exit=$exit_code)"
