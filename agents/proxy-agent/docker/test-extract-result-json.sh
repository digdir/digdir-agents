#!/usr/bin/env bash
# Standalone regressjonstest for repair_near_json/extract_result_json (#104).
# Ingen testrammeverk i repoet for shell ennå — kjør direkte:
#   bash agents/proxy-agent/docker/test-extract-result-json.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
SCRIPT=entrypoint.sh

# Hent kun funksjonene vi tester, uten å kjøre resten av entrypointet (som
# forventer /triggers, pi osv.).
eval "$(sed -n '/^RESULT_MARKER=/p;/^repair_near_json() {/,/^}/p;/^extract_result_json() {/,/^}/p' "$SCRIPT")"

failures=0

assert_valid_json() {
  local name="$1" out="$2"
  if jq -e . >/dev/null 2>&1 <<<"$out"; then
    echo "OK   $name"
  else
    echo "FAIL $name (ugyldig JSON): $out"
    failures=$((failures + 1))
  fi
}

assert_extract_fails() {
  local name="$1" log_file="$2" out
  if out=$(extract_result_json "$log_file" 2>/dev/null); then
    echo "FAIL $name (forventet feil, fikk: $out)"
    failures=$((failures + 1))
  else
    echo "OK   $name"
  fi
}

assert_extract_ok() {
  local name="$1" log_file="$2" expect_field="$3" expect_value="$4"
  local out got
  if ! out=$(extract_result_json "$log_file" 2>/dev/null); then
    echo "FAIL $name (extract_result_json feilet)"
    failures=$((failures + 1))
    return
  fi
  got=$(jq -r ".$expect_field" <<<"$out")
  if [[ "$got" == "$expect_value" ]]; then
    echo "OK   $name"
  else
    echo "FAIL $name (forventet $expect_field=$expect_value, fikk $got)"
    failures=$((failures + 1))
  fi
}

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# --- repair_near_json: reparasjonspass ---

# Ekte glipp fra issue #104: "…','delegate": i stedet for …","delegate":
comma_glitch='{"intent":"delegate","reply":"noe pa issue #96.'"'"','"'"'delegate":{"agent":"x","prompt":"y","payload":{}}}'
assert_valid_json "repair: comma-glipp (,',)" "$(repair_near_json "$comma_glitch")"

# Ekte glipp: ":' i stedet for ":" foran en verdi
colon_glitch='{"intent":"delegate","delegate":{"agent":'"'"'x","prompt":"y"}}'
assert_valid_json "repair: colon-glipp (\":')" "$(repair_near_json "$colon_glitch")"

# Gyldig JSON skal IKKE endres av reparasjonspasset
valid='{"intent":"action","reply":"Jeg fikset det, det gikk fint"}'
repaired_valid=$(repair_near_json "$valid")
if [[ "$repaired_valid" == "$valid" ]]; then
  echo "OK   repair: gyldig JSON uendret"
else
  echo "FAIL repair: gyldig JSON uendret (ble: $repaired_valid)"
  failures=$((failures + 1))
fi

# Vanlig komma i brødtekst skal ikke trigge reparasjon (kun glippen mellom felt)
prose='{"intent":"ack","reply":"Møtet er kl 14, ikke 15"}'
repaired_prose=$(repair_near_json "$prose")
if [[ "$repaired_prose" == "$prose" ]]; then
  echo "OK   repair: komma i brødtekst uendret"
else
  echo "FAIL repair: komma i brødtekst uendret (ble: $repaired_prose)"
  failures=$((failures + 1))
fi

# --- extract_result_json: hele pipeline via en simulert loggfil ---

log_ok="$tmpdir/ok.log"
printf 'noe agent-tekst\n%s\n{"intent":"ack","reply":"hei"}\n' "$RESULT_MARKER" >"$log_ok"
assert_extract_ok "extract: gyldig blokk" "$log_ok" "intent" "ack"

log_comma="$tmpdir/comma.log"
printf '%s\n%s\n' "$RESULT_MARKER" "$comma_glitch" >"$log_comma"
assert_extract_ok "extract: comma-glipp reparert" "$log_comma" "intent" "delegate"

log_colon="$tmpdir/colon.log"
printf '%s\n%s\n' "$RESULT_MARKER" "$colon_glitch" >"$log_colon"
assert_extract_ok "extract: colon-glipp reparert" "$log_colon" "delegate.agent" "x"

log_fence="$tmpdir/fence.log"
printf '%s\n```json{"intent":"ack","reply":""}\n```\n' "$RESULT_MARKER" >"$log_fence"
assert_extract_ok "extract: sammenklistret kodefence reparert" "$log_fence" "intent" "ack"

log_missing="$tmpdir/missing.log"
printf 'agenten glemte markøren helt\n{"intent":"ack"}\n' >"$log_missing"
assert_extract_fails "extract: manglende markør (#97) feiler rent" "$log_missing"

log_garbage="$tmpdir/garbage.log"
printf '%s\ndette er ikke json i det hele tatt\n' "$RESULT_MARKER" >"$log_garbage"
assert_extract_fails "extract: markør + søppel feiler rent (#91-fallback)" "$log_garbage"

echo
if [[ "$failures" -eq 0 ]]; then
  echo "Alle tester OK"
  exit 0
else
  echo "$failures test(er) feilet"
  exit 1
fi
