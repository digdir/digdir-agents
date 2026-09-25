#!/usr/bin/env bash
# Entrypoint for Pi-agenten. To moduser:
#   watch            – poller TRIGGER_FILE (jsonl) og kjører pi per nye event
#   oneshot <tekst>  – kjører pi én gang med prompten fra argument/AGENT_PROMPT/stdin
#   pi <args...>     – kjør pi direkte (f.eks. `pi --version`)
set -euo pipefail

TRIGGER_FILE="${TRIGGER_FILE:-/triggers/inbox.jsonl}"
RESULT_FILE="${RESULT_FILE:-/triggers/results.jsonl}"
LOG_DIR="${LOG_DIR:-/triggers/logs}"
STATE_FILE="${STATE_FILE:-/triggers/.state}"
POLL_INTERVAL="${POLL_INTERVAL:-5}"
PI_MODEL="${PI_MODEL:-}"

# Løkkevakt + backstop (#112): en pi-kjøring kjørte 1972 ganger på rad samme
# verktøykall med gyldig svar hver gang — ingen feil å reagere på, så vanlig
# feilhåndtering fanger den ikke. To uavhengige vern rundt hvert pi-kall:
#   1. Løkkevakt (primær) — se detect_loop(): glidende vindu over de siste
#      LOOP_DETECTOR_WINDOW verktøykallene fra sesjonsfila; >=
#      LOOP_DETECTOR_THRESHOLD identiske => løkke.
#   2. Backstop-timeout (sekundær) — PI_MAX_DURATION er en svært romslig
#      absolutt makstid som fanger patologier løkkevakta ikke ser.
# Begge dreper kun pi-prosessen (aldri containeren); process_event sin vanlige
# opprydding (knowledge_push_pending m.m.) kjører uendret etterpå.
PI_MAX_DURATION="${PI_MAX_DURATION:-7200}"
LOOP_DETECTOR_WINDOW="${LOOP_DETECTOR_WINDOW:-10}"
LOOP_DETECTOR_THRESHOLD="${LOOP_DETECTOR_THRESHOLD:-5}"
LOOP_DETECTOR_INTERVAL="${LOOP_DETECTOR_INTERVAL:-60}"
PI_SESSIONS_DIR="${PI_SESSIONS_DIR:-$HOME/.pi/agent/sessions}"

# Lokalt/OpenAI-kompatibelt LLM-endepunkt (f.eks. Envoy AI Gateway på hosten).
# Settes LLM_BASE_URL genereres ~/.pi/agent/models.json ved oppstart, og
# LLM_MODEL_ID brukes som default modell (med mindre PI_MODEL overstyrer).
LLM_BASE_URL="${LLM_BASE_URL:-}"
LLM_MODEL_ID="${LLM_MODEL_ID:-local}"
LLM_API_KEY="${LLM_API_KEY:-none}"

if [[ -n "$LLM_BASE_URL" ]]; then
  mkdir -p "$HOME/.pi/agent"
  jq -n \
    --arg baseUrl "$LLM_BASE_URL" \
    --arg apiKey "$LLM_API_KEY" \
    --arg model "$LLM_MODEL_ID" \
    '{providers: {"local-llm": {baseUrl: $baseUrl, api: "openai-completions", apiKey: $apiKey, models: [{id: $model}]}}}' \
    >"$HOME/.pi/agent/models.json"
  [[ -n "$PI_MODEL" ]] || PI_MODEL="$LLM_MODEL_ID"
fi

pi_args=(-p)
if [[ -n "$PI_MODEL" ]]; then
  pi_args+=(--model "$PI_MODEL")
fi

# Kunnskapsbase (OKF-wiki): KB_REPO klones/pulles til KNOWLEDGE_DIR ved
# oppstart. KB_REPO kan være full URL eller owner/repo (github.com antas).
# Tokenet leses fra env av credential-helperen ved bruk — det lagres aldri
# i .git/config. Tom KB_REPO/KB_GH_TOKEN = kunnskapsbasen er inaktiv.
KB_REPO="${KB_REPO:-}"
KB_GH_TOKEN="${KB_GH_TOKEN:-}"
KNOWLEDGE_DIR="${KNOWLEDGE_DIR:-/knowledge}"
KB_CRED_HELPER='!f() { echo username=x-access-token; echo "password=${KB_GH_TOKEN}"; }; f'

sync_knowledge() {
  [[ -n "$KB_REPO" && -n "$KB_GH_TOKEN" ]] || return 0
  local url="$KB_REPO"
  case "$url" in
    http://*|https://*) url="${url%.git}.git" ;;
    *) url="https://github.com/${KB_REPO}.git" ;;
  esac
  mkdir -p "$KNOWLEDGE_DIR"
  # Bind-mount fra hosten kan ha en annen eier enn container-brukeren;
  # uten safe.directory nekter git å røre repoet ("not in a git directory").
  git config --global --add safe.directory "$KNOWLEDGE_DIR" 2>/dev/null || true
  if [[ -d "$KNOWLEDGE_DIR/.git" ]]; then
    git -C "$KNOWLEDGE_DIR" config credential.helper "$KB_CRED_HELPER" 2>/dev/null || true
    if git -C "$KNOWLEDGE_DIR" pull --ff-only --quiet 2>/dev/null; then
      log "Kunnskapsbase: $KNOWLEDGE_DIR oppdatert fra remote"
    else
      log "Kunnskapsbase: pull feilet – fortsetter med eksisterende innhold"
    fi
  elif [[ -z "$(ls -A "$KNOWLEDGE_DIR" 2>/dev/null)" ]]; then
    if git clone --config credential.helper="$KB_CRED_HELPER" --quiet "$url" "$KNOWLEDGE_DIR" 2>/dev/null; then
      log "Kunnskapsbase: klonet til $KNOWLEDGE_DIR"
    else
      log "Kunnskapsbase: klarte ikke klone KB_REPO – fortsetter uten"
    fi
  else
    log "Kunnskapsbase: $KNOWLEDGE_DIR er ikke tom og ikke et git-repo – hopper over sync"
  fi
  # Klargjør for fangst av læringer (M3): agenten committer selv i
  # /knowledge, så repoet trenger identitet — og evt. lokale commits som
  # ikke kom av gårde ved forrige push-feil prøves på nytt her.
  if [[ -d "$KNOWLEDGE_DIR/.git" ]]; then
    git -C "$KNOWLEDGE_DIR" config user.name  "${KB_GIT_NAME:-proxy-agent}" 2>/dev/null || true
    git -C "$KNOWLEDGE_DIR" config user.email "${KB_GIT_EMAIL:-proxy-agent@users.noreply.github.com}" 2>/dev/null || true
    git -C "$KNOWLEDGE_DIR" config pull.rebase true 2>/dev/null || true
    # Første push mot et nyopprettet (tomt) repo skal etablere upstream selv
    git -C "$KNOWLEDGE_DIR" config push.autoSetupRemote true 2>/dev/null || true
    git -C "$KNOWLEDGE_DIR" push --quiet 2>/dev/null || true
  fi
}

# Sikkerhetsnett etter hver kjøring: sync_knowledge pusher bare ved oppstart,
# og containeren kjører lenge — commits en kjøring etterlater upushet ville
# ellers blitt liggende lokalt på ubestemt tid. Et skittent arbeidstre betyr
# at kjøringen hoppet over commit-steget i skillen sin; det kan ikke
# repareres trygt herfra (vi vet ikke hva som hører sammen), så det varsles.
knowledge_push_pending() {
  [[ -n "$KB_GH_TOKEN" && -d "$KNOWLEDGE_DIR/.git" ]] || return 0
  if [[ -n "$(git -C "$KNOWLEDGE_DIR" status --porcelain 2>/dev/null)" ]]; then
    log "WARN: kunnskapsbasen har ukommitterte endringer — kjøringen hoppet over commit-steget"
  fi
  if [[ -n "$(git -C "$KNOWLEDGE_DIR" log --branches --not --remotes --oneline 2>/dev/null | head -n 1)" ]]; then
    if git -C "$KNOWLEDGE_DIR" push --quiet 2>/dev/null; then
      log "Kunnskapsbase: pushet commits som lå igjen lokalt"
    else
      log "WARN: Kunnskapsbase: push feilet — nytt forsøk etter neste kjøring"
    fi
  fi
}

# Skills bakt inn i imaget (se Dockerfile). Lastes eksplisitt med --skill
# siden ~/.pi ligger på et volum som ville skygget image-innhold.
SKILLS_DIR="${SKILLS_DIR:-/opt/pi-skills}"
if [[ -d "$SKILLS_DIR" ]]; then
  for skill_dir in "$SKILLS_DIR"/*/; do
    [[ -f "$skill_dir/SKILL.md" ]] && pi_args+=(--skill "${skill_dir%/}")
  done
fi

log() {
  printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

# Marker the agent must print before its machine-readable result block.
RESULT_MARKER="===AGENT-RESULT==="

# Delegering: DELEGATE_AGENTS="navn:beskrivelse;navn2:beskrivelse" gjør
# "delegate" tilgjengelig som intent — broen (integrations) ruter da oppgaven
# videre til målagentens innboks. Tomt = ingen delegering.
DELEGATE_AGENTS="${DELEGATE_AGENTS:-}"

delegate_targets() {
  local entry name desc
  local IFS=';'
  for entry in $DELEGATE_AGENTS; do
    name="${entry%%:*}"
    desc="${entry#*:}"
    [[ -n "$name" ]] || continue
    [[ "$desc" != "$entry" ]] || desc="(ingen beskrivelse)"
    printf '     * "%s": %s\n' "$name" "$desc"
  done
}

classification_block() {
  local delegate_line="" delegate_doc="" intents='action|feedback|ack'
  if [[ -n "$DELEGATE_AGENTS" ]]; then
    intents='action|feedback|ack|delegate'
    delegate_line=$(printf '   - "delegate": oppgaven bør utføres av en annen agent. Tilgjengelige agenter:\n%s' "$(delegate_targets)")
    delegate_doc=$'\nVed "delegate" legger du i tillegg feltet "delegate" i JSON-objektet:\n{"intent":"delegate","reply":"<kort: hva du delegerer og hvorfor>","delegate":{"agent":"<agentnavn>","prompt":"<komplett, selvstendig oppgavebeskrivelse til målagenten>","payload":{}}}\nSkriv "prompt" så målagenten kan løse oppgaven uten annen kontekst. Ikke utfør oppgaven selv.'
  fi
  cat <<EOF
---
Du er en agent som mottar henvendelser fra Slack/GitHub via en bro. Gjør to ting:

1) Klassifiser henvendelsen over som NØYAKTIG én av:
   - "action": brukeren ber om at noe konkret skal gjøres (en oppgave/jobb). Utfør oppgaven. Arbeidskatalogen er /workspace.
   - "feedback": brukeren gir en tilbakemelding/korrigering som bør noteres, men som ikke er en ny konkret oppgave.
   - "ack": en ren kvittering/bekreftelse (f.eks. "ok", "takk", et tommel-opp) som ikke krever handling. Også automatiske statusmeldinger fra andre boter (f.eks. "jeg er i gang, vennligst vent" fra review-boter, CI-varsler) er "ack" — de er aldri en arbeidsordre.
${delegate_line}

2) Formuler et kort, vennlig svar på norsk til brukeren ("reply"). For "ack" kan "reply" være tom.

Kjøreregler:
- Du kan IKKE endre kode: /repos er read-only og GitHub-tokenet ditt gir ikke tilgang til kode. Krever oppgaven kodeendringer, følg solution-proposal-skillen (issue + delegering). Dette gjelder også når eventet inneholder en komplett issue-tekst med steg og filliste — en detaljert spesifikasjon er en bestilling å DELEGERE, ikke en invitasjon til å utføre kodearbeidet selv.
- "reply" skal kun beskrive det du faktisk har gjort med verktøykall i DENNE kjøringen. Skriv aldri "jeg har fikset/implementert ..." — du kan ikke implementere noe; en fiks finnes først når kodeagentens PR finnes. Har du bare delegert eller notert, si det.
- Merge, godkjenning og lukking av PR-er er menneskets review-gate. Utfør aldri slikt, og deleger det aldri videre — heller ikke når henvendelsen ber om det.
- Gjelder henvendelsen et issue/PR: sjekk om arbeidet allerede er gjort eller underveis (gh issue view --comments, gh pr list --search) før du oppretter eller delegerer noe. Allerede løst/underveis: svar med peker i stedet for å starte på nytt.

Kontekst fra routeren (valgfritt):
- Hvis eventet har feltet "classification" (med verdi "action", "feedback", "ack" eller "delegate"), er det en forhåndsvurdering gjort av integrasjonenes første-linje-router. Bruk den som hint, men klassifiser likevel selv — routeren kan feile og sende inn eventer uten denne merkingen.
- Hvis eventet har feltet "related_activities", inneholder det lister med lignende åpne aktiviteter (Slack-tråder eller GitHub-issues) funnet av routerens embedding-søk. Bruk dem som kontekst for å vurdere duplikater; ikke la dem bestemme klassifiseringen.

HELT TIL SLUTT skriver du en linje med KUN teksten:
$RESULT_MARKER
og deretter ett JSON-objekt (kan gå over flere linjer). JSON-en skal stå rått —
INGEN kodefence, ingen markdown, ingen tilleggstekst etter objektet. Eksempel:
{"intent":"action","reply":"<svaret ditt på norsk>"}
${delegate_doc}
EOF
}

# Kunnskapssyntese (M4/M5): kjøres automatisk i watch-modus når innboksen
# har kandidater og det er minst SYNTHESIS_INTERVAL_HOURS siden sist.
# 0 = aldri automatisk (syntese kan alltid trigges manuelt via et event).
SYNTHESIS_INTERVAL_HOURS="${SYNTHESIS_INTERVAL_HOURS:-24}"
SYNTHESIS_STATE_FILE="${SYNTHESIS_STATE_FILE:-/triggers/.synthesis-last}"

synthesis_due() {
  [[ "$SYNTHESIS_INTERVAL_HOURS" =~ ^[0-9]+$ ]] || return 1
  (( SYNTHESIS_INTERVAL_HOURS > 0 )) || return 1
  [[ -f "$KNOWLEDGE_DIR/index.md" ]] || return 1
  [[ -s "$KNOWLEDGE_DIR/inbox/learnings.jsonl" ]] || return 1
  local last=0 now
  if [[ -f "$SYNTHESIS_STATE_FILE" ]]; then
    last=$(<"$SYNTHESIS_STATE_FILE")
    [[ "$last" =~ ^[0-9]+$ ]] || last=0
  fi
  now=$(date +%s)
  (( now - last >= SYNTHESIS_INTERVAL_HOURS * 3600 ))
}

run_synthesis() {
  local ts log_file rc
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  log_file="$LOG_DIR/synthesis-$ts.log"
  # Stemples før kjøring, så en feilende syntese ikke spinner hvert poll
  date +%s >"$SYNTHESIS_STATE_FILE"
  log "Kunnskapssyntese: starter (logg: $log_file)"
  set +e
  pi "${pi_args[@]}" "Kjør kunnskapssyntese på kunnskapsbasen i $KNOWLEDGE_DIR: følg prosedyren i knowledge-synthesis-skillen trinn for trinn." >"$log_file" 2>&1
  rc=$?
  set -e
  log "Kunnskapssyntese: ferdig (exit $rc)"
  knowledge_push_pending
}

# Kort hint om kunnskapsbasen, kun når den faktisk er tilgjengelig.
knowledge_block() {
  [[ -f "$KNOWLEDGE_DIR/index.md" ]] || return 0
  printf 'Kunnskapsbase: %s er en OKF-wiki med domenekunnskap og tidligere lærdommer. Les %s/index.md og følg lenkene derfra hvis oppgaven kan dra nytte av det.\n\n' "$KNOWLEDGE_DIR" "$KNOWLEDGE_DIR"
}

build_prompt() {
  local event_json="$1" prompt kb
  kb=$(knowledge_block)
  [[ -z "$kb" ]] || kb="$kb"$'\n\n'
  prompt=$(jq -r '.prompt // empty' <<<"$event_json")
  if [[ -n "$prompt" ]]; then
    printf '%s\n\n%sKontekst – komplett trigger-event (JSON):\n%s\n\n%s\n' "$prompt" "$kb" "$event_json" "$(classification_block)"
  else
    printf 'Du har mottatt et eksternt trigger-event (f.eks. fra Slack eller GitHub).\nUtfør oppgaven eventet beskriver. Arbeidskatalogen er /workspace.\n\n%sEvent (JSON):\n%s\n\n%s\n' "$kb" "$event_json" "$(classification_block)"
  fi
}

# Reparasjonspass for nesten-JSON (#104): den lokale modellen glipper noen
# ganger på anførselstegn rundt feltskillet — "," (mellom to strengfelt) eller
# ":" (mellom nøkkel og verdi) kommer ut med en apostrof i stedet for et
# anførselstegn. Disse to erstatningene er snevre og målrettede — kun de to
# kjente glippene — IKKE en generell anførselstegn-erstatning som kunne
# ødelagt tekst inni selve strengverdiene. Håndterer i tillegg at hele blokken
# er pakket i en kodefence uten at fence-linjen står helt for seg selv (#83):
# en ledende ```json limt inntil JSON-en, eller en avsluttende ``` limt rett
# etter siste "}".
repair_near_json() {
  local s="$1"
  s=$(printf '%s' "$s" | sed -E '1s/^```[a-zA-Z]*[[:space:]]*//')
  s=$(printf '%s' "$s" | sed -E '$s/```+[[:space:]]*$//')
  s=$(printf '%s' "$s" | sed -e "s/','/\", \"/g")
  s=$(printf '%s' "$s" | sed -e "s/\":'/\": \"/g")
  printf '%s' "$s"
}

# Extracts the JSON block the agent printed after RESULT_MARKER (everything
# after the last marker line). Handles markdown fences the LLM sometimes wraps
# the result in (```json ... ```), which would otherwise make jq reject it.
# Logs a WARN when the marker is found but extraction still fails — so we do
# not silently drop delegations.
extract_result_json() {
  local log_file="$1" block has_marker=false repaired
  # Check whether the marker exists at all, so we can warn on malformed output.
  if grep -q "^${RESULT_MARKER}$" "$log_file"; then
    has_marker=true
  fi
  block=$(awk -v m="$RESULT_MARKER" '$0==m{f=1;buf="";next} f{buf=buf $0 ORS} END{printf "%s",buf}' "$log_file")

  # Strip leading/trailing fence lines: ```json / ``` (case-insensitive on the
  # language tag). This is needed because LLM-er gjerne pakker JSON inni en
  # markdown-kodefence selv når de blir bedt om rått objekt.
  block=$(printf '%s' "$block" | sed -E '1{/^```[a-zA-Z]*$/d}' | sed -E '${/^```$/d}')

  if [[ -n "$block" ]] && jq -e 'if type == "object" then . else error("not an object") end' >/dev/null 2>&1 <<<"$block"; then
    printf '%s' "$block"
    return 0
  fi

  # Direkte parsing feilet. Prøv reparasjonspasset (#104) før vi gir opp — det
  # rører aldri en blokk som allerede var gyldig JSON, siden vi bare når hit
  # når jq-sjekken over allerede har feilet.
  if [[ -n "$block" ]]; then
    repaired=$(repair_near_json "$block")
    if [[ "$repaired" != "$block" ]] && jq -e 'if type == "object" then . else error("not an object") end' >/dev/null 2>&1 <<<"$repaired"; then
      printf '%s' "$repaired"
      return 0
    fi
  fi

  # Marker funnet, men vi klarte ikke parse JSON-en (heller ikke etter
  # reparasjonspasset). Logg en WARN slik at delegeringen ikke går tapt
  # stille — broen får en generisk feilmelding i stedet for å poste hele
  # agentloggen som offentlig GitHub-kommentar.
  if [[ "$has_marker" == true ]]; then
    printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "WARN: AGENT-RESULT-blokk funnet, men ugyldig JSON — delegering tapt. Fikk:${#block} tegn." >&2
  fi
  result_json=""
  return 1
}

# Nyeste sesjonsfil under PI_SESSIONS_DIR (pi appender denne live, i
# motsetning til loggfila til eventet som forblir 0 bytes til pi er ferdig —
# derfor må løkkevakta lese sesjonsfila, ikke loggen, se #112).
find_latest_session_file() {
  [[ -d "$PI_SESSIONS_DIR" ]] || return 0
  find "$PI_SESSIONS_DIR" -type f -name '*.jsonl' -printf '%T@ %p\n' 2>/dev/null \
    | sort -rn \
    | head -n1 \
    | cut -d' ' -f2-
}

# Trekker ut verktøykall (navn + argumenter) fra en pi-sesjonsfil (JSONL med
# message-objekter; assistant-meldinger har et toolCall-felt med "name" +
# "arguments" — enten som ett objekt eller en liste). Returnerer ett kanonisk
# JSON-objekt per linje, kun de siste $2.
recent_tool_calls() {
  local session_file="$1" window="$2"
  jq -c '
    (.message.toolCall // empty) as $tc
    | if $tc == null then empty
      elif ($tc | type) == "array" then $tc[] | {name, arguments}
      else {name: $tc.name, arguments: $tc.arguments}
      end
  ' "$session_file" 2>/dev/null | tail -n "$window"
}

# Deteksjonsregel besluttet 2026-07-30 (#112): glidende vindu over de siste
# LOOP_DETECTOR_WINDOW verktøykallene — hvis LOOP_DETECTOR_THRESHOLD eller
# flere er identiske (samme verktøy + samme argumenter), er dette en
# degenerert løkke. Vindusvarianten fanger også vekslende løkker
# (A-B-A-B-...), ikke bare rene repetisjoner. Setter LOOP_DETECTOR_MESSAGE ved
# treff.
detect_loop() {
  local session_file="$1" calls total top_count
  calls=$(recent_tool_calls "$session_file" "$LOOP_DETECTOR_WINDOW")
  [[ -n "$calls" ]] || return 1
  total=$(printf '%s\n' "$calls" | grep -c .)
  top_count=$(printf '%s\n' "$calls" | sort | uniq -c | sort -rn | head -n1 | awk '{print $1}')
  [[ "$top_count" =~ ^[0-9]+$ ]] || return 1
  if (( top_count >= LOOP_DETECTOR_THRESHOLD )); then
    LOOP_DETECTOR_MESSAGE="løkke detektert: samme verktøykall x ${top_count} av siste ${total}"
    return 0
  fi
  return 1
}

# Kjører pi i bakgrunnen under oppsyn av løkkevakt + backstop-timeout.
# $3 (valgfritt) = "append" for å skrive til $log_file med >> (brukt av
# #104-retry-forsøket), ellers overskrives loggfila som normalt.
# Setter PI_GUARD_REASON ved drept prosess (tom = pi fullførte selv);
# returnerer pi sin exit-kode (eller signalets ved drept prosess).
run_pi_supervised() {
  local log_file="$1" prompt="$2" mode="${3:-truncate}"
  local pi_pid start_ts last_check now session_file
  PI_GUARD_REASON=""
  if [[ "$mode" == append ]]; then
    pi "${pi_args[@]}" "$prompt" >>"$log_file" 2>&1 &
  else
    pi "${pi_args[@]}" "$prompt" >"$log_file" 2>&1 &
  fi
  pi_pid=$!
  start_ts=$(date +%s)
  last_check=$start_ts

  # Poller prosessen hvert sekund (så en normal, rask kjøring ikke forsinkes),
  # men gjør selve løkke-/backstop-sjekken kun hvert LOOP_DETECTOR_INTERVAL —
  # den leser og parser hele sesjonsfila og trenger ikke kjøres oftere.
  while kill -0 "$pi_pid" 2>/dev/null; do
    sleep 1
    kill -0 "$pi_pid" 2>/dev/null || break

    now=$(date +%s)
    (( now - last_check >= LOOP_DETECTOR_INTERVAL )) || continue
    last_check=$now

    if (( now - start_ts >= PI_MAX_DURATION )); then
      PI_GUARD_REASON="backstop-timeout: pi-kjøringen overskred makstid (${PI_MAX_DURATION}s) uten å fullføre"
      log "Backstop-timeout: dreper pi-prosess (pid $pi_pid) etter $((now - start_ts))s"
      kill -TERM "$pi_pid" 2>/dev/null || true
      sleep 2
      kill -KILL "$pi_pid" 2>/dev/null || true
      break
    fi

    session_file=$(find_latest_session_file)
    if [[ -n "$session_file" ]] && detect_loop "$session_file"; then
      PI_GUARD_REASON="$LOOP_DETECTOR_MESSAGE"
      log "Løkkevakt: dreper pi-prosess (pid $pi_pid) — $PI_GUARD_REASON"
      kill -TERM "$pi_pid" 2>/dev/null || true
      sleep 2
      kill -KILL "$pi_pid" 2>/dev/null || true
      break
    fi
  done

  wait "$pi_pid" 2>/dev/null
  return $?
}

process_event() {
  local event_json="$1" id started finished exit_code log_file prompt guard_reason
  id=$(jq -r '.id // empty' <<<"$event_json")
  [[ -n "$id" ]] || id="evt-$(date +%s%N)"
  id="${id//[^a-zA-Z0-9._-]/_}"

  log_file="$LOG_DIR/$id.log"
  prompt=$(build_prompt "$event_json")
  started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  log "Behandler event $id ..."

  set +e
  run_pi_supervised "$log_file" "$prompt"
  exit_code=$?
  set -e
  guard_reason="$PI_GUARD_REASON"

  # Pull the agent's classification (intent + reply + evt. delegate) out of
  # the log, if present. extract_result_json may return non-zero when the
  # RESULT_MARKER is found but the JSON block is malformed — we catch that so
  # `set -e` does not abort the process_event loop. Ble kjøringen drept av
  # løkkevakt/backstop (#112) er loggen uinteressant — vi vet allerede hvorfor
  # og skal aldri prøve å tolke en avkuttet pi-output som gyldig resultat.
  local result_json="" intent reply delegate extraction_failed=false
  if [[ -z "$guard_reason" ]]; then
    set +e
    result_json=$(extract_result_json "$log_file") || extraction_failed=true
    set -e
  fi

  # Automatisk retry (#104): første forsøk feilet enten med ugyldig JSON etter
  # markøren (anførselstegn-glipp), eller markøren manglet helt (#97). Gi
  # modellen ÉN ny sjanse med samme prompt + et kort notat om hva som gikk
  # galt, før vi faller gjennom til eksisterende #91-fallback. Kun aktuelt når
  # løkkevakt/backstop ikke allerede har avgjort utfallet — å prøve på nytt
  # etter en drept kjøring ville bare risikere å gjenta samme løkke.
  if [[ "$extraction_failed" == true && -z "$guard_reason" ]]; then
    log "Event $id: AGENT-RESULT mangler eller er ugyldig JSON — prøver ett automatisk retry-forsøk."
    local retry_prompt
    retry_prompt=$(printf '%s\n\n---\nMERK: forrige svaret ditt ble avvist av broen — enten manglet linjen "%s" helt, eller JSON-objektet etter den var ugyldig (f.eks. feil anførselstegn rundt et felt-skille, eller hele objektet pakket i en kodefence). Gjenta oppgaven på nytt og avslutt garantert med en egen linje med kun teksten "%s", etterfulgt av ett gyldig, rått JSON-objekt uten kodefence.\n' \
      "$prompt" "$RESULT_MARKER" "$RESULT_MARKER")
    printf '\n[%s] --- RETRY (#104): forrige AGENT-RESULT var ugyldig/manglet ---\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$log_file"
    set +e
    run_pi_supervised "$log_file" "$retry_prompt" append
    exit_code=$?
    set -e
    guard_reason="$PI_GUARD_REASON"
    if [[ -z "$guard_reason" ]]; then
      extraction_failed=false
      set +e
      result_json=$(extract_result_json "$log_file") || extraction_failed=true
      set -e
      if [[ "$extraction_failed" == false ]]; then
        log "Event $id: retry ga gyldig AGENT-RESULT."
      fi
    fi
  fi

  finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  if [[ -n "$guard_reason" ]]; then
    printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "GUARD: $guard_reason" >>"$log_file"
    intent="action"
    reply="$guard_reason"
    delegate=""
  elif [[ -n "$result_json" ]]; then
    intent=$(jq -r '.intent // empty' <<<"$result_json")
    reply=$(jq -r '.reply // empty' <<<"$result_json")
    delegate=$(jq -c '.delegate // empty' <<<"$result_json")
  else
    intent=""; reply=""; delegate=""
  fi

  jq -cn \
    --arg id "$id" \
    --arg status "$([[ $exit_code -eq 0 ]] && echo ok || echo error)" \
    --argjson exit_code "$exit_code" \
    --arg log "logs/$id.log" \
    --arg intent "$intent" \
    --arg reply "$reply" \
    --arg delegate "$delegate" \
    --arg started_at "$started" \
    --arg finished_at "$finished" \
    --argjson extraction_failed "$extraction_failed" \
    '{id: $id, status: $status, exit_code: $exit_code, log: $log, started_at: $started_at, finished_at: $finished_at}
       + (if $intent != "" then {intent: $intent} else {} end)
       + (if $reply  != "" then {reply:  $reply}  else {} end)
       + (if $delegate != "" then {delegate: ($delegate | fromjson)} else {} end)
       + (if $extraction_failed == true then {extraction_failed: true} else {} end)' \
    >>"$RESULT_FILE"
  log "Event $id ferdig (exit $exit_code, intent=${intent:-?}, logg: $log_file)"
  knowledge_push_pending
}

watch_loop() {
  mkdir -p "$LOG_DIR"
  touch "$TRIGGER_FILE" "$RESULT_FILE"

  local processed=0 total line
  if [[ -f "$STATE_FILE" ]]; then
    processed=$(<"$STATE_FILE")
    [[ "$processed" =~ ^[0-9]+$ ]] || processed=0
  fi

  log "Watch-modus: poller $TRIGGER_FILE hvert ${POLL_INTERVAL}s (allerede behandlet: $processed linjer)"
  while true; do
    total=$(wc -l <"$TRIGGER_FILE")
    while (( total > processed )); do
      line=$(sed -n "$((processed + 1))p" "$TRIGGER_FILE")
      processed=$((processed + 1))
      printf '%s' "$processed" >"$STATE_FILE"

      line="${line%$'\r'}"
      [[ -n "${line//[[:space:]]/}" ]] || continue
      if ! jq -e . >/dev/null 2>&1 <<<"$line"; then
        log "Hopper over linje $processed: ugyldig JSON"
        continue
      fi
      process_event "$line"
    done
    if synthesis_due; then
      run_synthesis
    fi
    sleep "$POLL_INTERVAL"
  done
}

sync_knowledge

cmd="${1:-watch}"
case "$cmd" in
  watch)
    watch_loop
    ;;
  oneshot)
    shift || true
    prompt="${*:-${AGENT_PROMPT:-}}"
    if [[ -z "$prompt" ]]; then
      prompt=$(cat)
    fi
    exec pi "${pi_args[@]}" "$prompt"
    ;;
  pi)
    shift
    exec pi "$@"
    ;;
  *)
    exec "$@"
    ;;
esac
