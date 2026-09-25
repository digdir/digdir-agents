#!/usr/bin/env bash
# Standalone regressjonstest for løkkevakta (find_latest_session_file /
# recent_tool_calls / detect_loop, #112). Ingen testrammeverk i repoet for
# shell ennå — kjør direkte:
#   bash agents/proxy-agent/docker/test-loop-guard.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
SCRIPT=entrypoint.sh

# Hent kun funksjonene vi tester, uten å kjøre resten av entrypointet (som
# forventer /triggers, pi osv.).
eval "$(sed -n '/^find_latest_session_file() {/,/^}/p;/^recent_tool_calls() {/,/^}/p;/^detect_loop() {/,/^}/p' "$SCRIPT")"

# Deteksjonsregel besluttet 2026-07-30 (samme default som entrypoint.sh).
LOOP_DETECTOR_WINDOW=10
LOOP_DETECTOR_THRESHOLD=5

failures=0

ok() { echo "OK   $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# --- find_latest_session_file ---

PI_SESSIONS_DIR="$tmpdir/sessions/nested"
mkdir -p "$PI_SESSIONS_DIR"
printf '{}\n' >"$PI_SESSIONS_DIR/old.jsonl"
sleep 1.1
printf '{}\n' >"$PI_SESSIONS_DIR/new.jsonl"

latest=$(find_latest_session_file)
if [[ "$latest" == "$PI_SESSIONS_DIR/new.jsonl" ]]; then
  ok "find_latest_session_file: velger nyeste fil i undermappe"
else
  fail "find_latest_session_file: forventet new.jsonl, fikk '$latest'"
fi

empty_dir_var="$tmpdir/does-not-exist"
PI_SESSIONS_DIR="$empty_dir_var"
latest=$(find_latest_session_file)
if [[ -z "$latest" ]]; then
  ok "find_latest_session_file: manglende katalog gir tomt svar (ingen feil)"
else
  fail "find_latest_session_file: forventet tomt svar, fikk '$latest'"
fi
PI_SESSIONS_DIR="$tmpdir/sessions/nested"

# --- helper: bygg en syntetisk sesjonsfil (JSONL) av verktøykall ---
# Args: <fil> <navn> <argumenter-json> <antall> ...(gjentas for flere kall)
build_session() {
  local file="$1"
  shift
  : >"$file"
  while (( $# > 0 )); do
    local name="$1" args="$2" count="$3"
    shift 3
    for ((i = 0; i < count; i++)); do
      jq -cn --arg n "$name" --argjson a "$args" \
        '{message: {role: "assistant", toolCall: {name: $n, arguments: $a}}}' >>"$file"
    done
  done
}

# --- recent_tool_calls ---

session="$tmpdir/recent.jsonl"
build_session "$session" bash '{"cmd":"ls"}' 2 read '{"path":"a.txt"}' 1
# Legg til en melding uten toolCall — skal ignoreres, ikke telle med
jq -cn '{message: {role: "assistant", text: "hei"}}' >>"$session"

calls=$(recent_tool_calls "$session" 10)
count=$(printf '%s\n' "$calls" | grep -c .)
if [[ "$count" -eq 3 ]]; then
  ok "recent_tool_calls: teller kun toolCall-oppføringer (ignorerer andre meldinger)"
else
  fail "recent_tool_calls: forventet 3 kall, fikk $count"
fi

# toolCall som liste (flere kall i samme melding) skal også plukkes opp
session_list="$tmpdir/recent-list.jsonl"
jq -cn '{message: {role: "assistant", toolCall: [{name:"a",arguments:{}},{name:"b",arguments:{}}]}}' >"$session_list"
calls=$(recent_tool_calls "$session_list" 10)
count=$(printf '%s\n' "$calls" | grep -c .)
if [[ "$count" -eq 2 ]]; then
  ok "recent_tool_calls: toolCall som liste gir ett innslag per kall"
else
  fail "recent_tool_calls: forventet 2 kall fra liste, fikk $count"
fi

# --- detect_loop ---

# Ren løkke: samme verktøykall 10 ganger på rad (obduksjonstilfellet, #112)
session_pure="$tmpdir/pure-loop.jsonl"
build_session "$session_pure" bash '{"cmd":"gh issue list | sort -n"}' 10
if detect_loop "$session_pure"; then
  ok "detect_loop: ren repetisjon (10/10 identiske) detekteres"
  [[ "$LOOP_DETECTOR_MESSAGE" == "løkke detektert: samme verktøykall x 10 av siste 10" ]] \
    && ok "detect_loop: meldingsteksten er korrekt ($LOOP_DETECTOR_MESSAGE)" \
    || fail "detect_loop: uventet meldingstekst: $LOOP_DETECTOR_MESSAGE"
else
  fail "detect_loop: ren repetisjon ble IKKE detektert"
fi

# Vekslende løkke: A-B-A-B-... (5 A + 5 B) — skal også fanges opp
session_alt="$tmpdir/alt-loop.jsonl"
: >"$session_alt"
for ((i = 0; i < 5; i++)); do
  jq -cn '{message:{role:"assistant",toolCall:{name:"read",arguments:{path:"x"}}}}' >>"$session_alt"
  jq -cn '{message:{role:"assistant",toolCall:{name:"write",arguments:{path:"x"}}}}' >>"$session_alt"
done
if detect_loop "$session_alt"; then
  ok "detect_loop: vekslende løkke (A-B-A-B-...) detekteres"
else
  fail "detect_loop: vekslende løkke ble IKKE detektert"
fi

# Under terskel: kun 4 av 10 identiske — skal IKKE detekteres
session_under="$tmpdir/under-threshold.jsonl"
build_session "$session_under" bash '{"cmd":"ls"}' 4 read '{"path":"a"}' 1 read '{"path":"b"}' 1 \
  read '{"path":"c"}' 1 read '{"path":"d"}' 1 read '{"path":"e"}' 1 read '{"path":"f"}' 1
if detect_loop "$session_under"; then
  fail "detect_loop: 4/10 identiske ble feilaktig detektert som løkke"
else
  ok "detect_loop: 4/10 identiske (under terskel) detekteres IKKE"
fi

# Normal variasjon: alle kall ulike — skal IKKE detekteres
session_varied="$tmpdir/varied.jsonl"
: >"$session_varied"
for ((i = 0; i < 10; i++)); do
  jq -cn --arg n "tool$i" '{message:{role:"assistant",toolCall:{name:$n,arguments:{i:$n}}}}' >>"$session_varied"
done
if detect_loop "$session_varied"; then
  fail "detect_loop: normal variasjon ble feilaktig detektert som løkke"
else
  ok "detect_loop: normal variasjon detekteres IKKE"
fi

echo
if [[ "$failures" -eq 0 ]]; then
  echo "Alle tester OK"
  exit 0
else
  echo "$failures test(er) feilet"
  exit 1
fi
