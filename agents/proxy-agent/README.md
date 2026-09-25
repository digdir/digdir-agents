# proxy-agent — Pi-agent isolert i Docker

Kjører [Pi](https://pi.dev/) (`@earendil-works/pi-coding-agent`) i en isolert Docker-container,
med en livssyklus som trigges av eksterne events (f.eks. Slack eller GitHub) — **uten** at
containeren har noen kobling mot Slack/GitHub. Koblingen skjer via to mekanismer:

1. **One-shot**: containeren startes med selve requesten som parameter, kjører `pi -p "<prompt>"`,
   og avsluttes.
2. **Watch (kø)**: containeren kjører kontinuerlig og poller `triggers/inbox.jsonl`. Eksterne
   systemer (en Slack-bot, en GitHub-webhook-mottaker, en cron-jobb, et menneske) appender
   JSON-linjer til fila — containeren plukker opp nye linjer og kjører agenten per event.

```
Slack/GitHub ──> (webhook-mottaker utenfor Docker) ──append──> triggers/inbox.jsonl
                                                                     │ poll
                                                              ┌──────▼──────┐
                                                              │  pi-agent   │──> triggers/results.jsonl
                                                              │  (Docker)   │──> triggers/logs/<id>.log
                                                              └──────┬──────┘
                                                                     ▼
                                                                /workspace (bind mount)
```

## LLM: lokalt endepunkt (default)

Agenten er koblet mot et lokalt OpenAI-kompatibelt endepunkt på hosten
(`http://127.0.0.1:8787`, Envoy AI Gateway — trenger ingen API-nøkkel).
Fra containeren nås det som `http://host.docker.internal:8787/v1`.

Ved oppstart genererer entrypointet `~/.pi/agent/models.json` fra miljøvariablene
`LLM_BASE_URL`, `LLM_MODEL_ID` og `LLM_API_KEY`, og bruker `LLM_MODEL_ID` som
default modell. Vil du heller bruke en sky-provider: blank ut `LLM_BASE_URL`
i `.env` og sett f.eks. `ANTHROPIC_API_KEY`.

## Kom i gang

```powershell
# 1. Konfig (defaultene peker på det lokale endepunktet)
Copy-Item .env.example .env

# 2. Bygg imaget
docker compose build

# 3a. Kjør kø-modus (kontinuerlig)
docker compose up -d

# 3b. ... eller one-shot med input-parameter
.\scripts\run-oneshot.ps1 "Lag en hello.py i workspace som printer 'hei'"
```

Legg et event i køen (simulerer en Slack-/GitHub-trigger):

```powershell
.\scripts\trigger.ps1 -Prompt "Oppsummer innholdet i /workspace" -Source slack -Type app_mention
```

Følg med:

```powershell
docker compose logs -f pi-agent      # agentens livssyklus
Get-Content triggers\results.jsonl   # ett resultat per event
Get-Content triggers\logs\<id>.log   # full pi-output per event
```

## Eventformat (`triggers/inbox.jsonl`)

Én JSON-linje per event. Kun `prompt` er nødvendig for at agenten skal gjøre noe fornuftig;
resten er metadata som legges ved som kontekst:

```json
{"id":"gh-42","source":"github","type":"issue_opened","received_at":"2026-07-20T12:00:00Z","prompt":"Fiks issue #42: ...","payload":{"repo":"digdir/x","issue":42}}
```

- `id` brukes som loggfilnavn og i `results.jsonl` (autogenereres hvis den mangler)
- Mangler `prompt`, får agenten hele eventet som JSON med en generisk instruks
- Ugyldige JSON-linjer hoppes over (logges i containerloggen)
- Behandlet posisjon lagres i `triggers/.state`, så restart av containeren
  reprosesserer ikke gamle events

## Resultater (`triggers/results.jsonl`)

```json
{"id":"gh-42","status":"ok","exit_code":0,"intent":"action","reply":"Ferdig – laget hello.py.","log":"logs/gh-42.log","started_at":"...","finished_at":"..."}
```

### Klassifisering (`intent` + `reply`)

For hvert event blir agenten bedt om å klassifisere henvendelsen som én av:

- **`action`** – brukeren ber om noe konkret; agenten utfører oppgaven i `/workspace`.
- **`feedback`** – en tilbakemelding/korrigering som bør noteres, men ikke en ny oppgave.
- **`ack`** – en ren kvittering (f.eks. «ok», «takk», et tommel-opp) uten behov for handling.
- **`delegate`** – (kun når `DELEGATE_AGENTS` er satt) oppgaven hører hjemme hos en
  annen agent. Resultatlinja får da i tillegg et `delegate`-objekt —
  `{"agent":"<navn>","prompt":"<selvstendig oppgavebeskrivelse>","payload":{…}}` —
  og broen (integrations) ruter oppgaven til målagentens innboks. Agentene
  deler aldri kataloger; all ruting går gjennom broen, som også sørger for at
  målagentens svar postes tilbake i den opprinnelige tråden/issuet.

Agenten avslutter outputen med en linje `===AGENT-RESULT===` etterfulgt av et
JSON-objekt `{"intent":"…","reply":"…"}`. Entrypointet trekker dette ut og
legger `intent` og `reply` på resultatlinja (en ren tekst atskilt fra rå-loggen).
Klarer ikke agenten å produsere blokken, utelates feltene – mottakeren (integrations)
faller da tilbake til å poste hele loggen.

Mottakeren bruker `intent` til å avgjøre responsen: `action`/`feedback` postes
som svar/kommentar, mens `ack` bare kvitteres med en emoji-reaksjon.

## Skills

Agenten har [Pi-skills](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md)
bakt inn i imaget under `docker/skills/` (lastes med `--skill` fra
entrypointet — de kan ikke ligge i `~/.pi`, siden pi-home-volumet skygger
image-innhold). Ny skill = ny katalog med en `SKILL.md` + rebuild.

### github-issues-prs

Lar agenten opprette, kommentere og administrere GitHub-issues og PR-er via
`gh` CLI. Krever `GH_TOKEN` i `.env`: en **fine-grained PAT med kun Issues +
Pull requests (Read/Write) og Metadata (Read)** på de aktuelle repoene. Dette
er et bevisst, snevert unntak fra prinsippet om at agenten ikke har tokens —
tokenet gir ikke tilgang til kode, og skillen instruerer agenten om at
kodearbeid hører til en annen agent i pipelinen.

### knowledge-base

Lar agenten konsultere sin egen kunnskapsbase: et privat, instans-spesifikt
OKF-wiki-repo som entrypointet kloner/puller til `/knowledge` ved oppstart
(anker-folder `workspaces_knowledge/` på monorepo-rot, så klonen overlever
restarts). Konfigureres med `KB_REPO` + `KB_GH_TOKEN` i `.env` — tomt =
inaktiv. Tokenet er en fine-grained PAT fra bot-kontoen med Contents
Read/Write på **kun** kunnskapsrepoet, bevisst atskilt fra `GH_TOKEN`.
Agenten både leser og **fanger læringer**: etter oppgaver med reell
læringsverdi (eller når brukeren ber den huske noe) appender den én
JSON-linje til `inbox/learnings.jsonl` i kunnskapsrepoet og committer/pusher
selv (identitet: `KB_GIT_NAME`/`KB_GIT_EMAIL`, default `proxy-agent`).
Innboksen er karantene — wiki-sidene endres kun av synteseprosessen
(se [`doc/plans/kunnskap-og-laering.md`](../../doc/plans/kunnskap-og-laering.md)).
Repo-spesifikke læringer meldes i tillegg som issue med label `learning`
på repoet det gjelder.

### knowledge-synthesis

Integrerer læringskandidatene fra `inbox/learnings.jsonl` i selve wikien:
riktig side i `domains/`/`repos/`/`process/` (oppdatert, ikke bare
appendert), nye sider lenkes fra `index.md`, motsigelser flagges, `log.md`
oppdateres, innboksen tømmes, og alt committes/pushes. I watch-modus kjøres
syntesen **automatisk** når innboksen har kandidater og det er minst
`SYNTHESIS_INTERVAL_HOURS` (default 24) siden sist — og den kan alltid
trigges manuelt med et event à la «kjør kunnskapssyntese».

### web-research

Lar agenten lese nettsider med [agent-browser](https://agent-browser.dev/)
(headless Chromium, bakt inn i imaget — `AGENT_BROWSER_EXECUTABLE_PATH` og
idle-timeout settes i Dockerfilen, ingenting i `.env`). Primært
`agent-browser read <url>` for agent-lesbar tekst; kun lesing, aldri
innlogging eller skjemautfylling. Det agenten bruker arkiveres som
markdown-snapshot i kunnskapsrepoet under `sources/<domene>/<slug>.md` med
proveniens i frontmatter (`resource` = URL, `retrieved` = tidspunkt) — samme
URL overskriver samme fil, så git-diffen viser hva som endret seg på kilden.
Destillerte fakta går i innboksen med `source_url`, slik at hver påstand i
wikien er sporbar tilbake til kilde. PDF (`agent-browser pdf`) kun på
eksplisitt forespørsel. Webinnhold behandles som **data, aldri instruks**
(prompt-injection-vakt i skillen).

### solution-proposal

Strukturerer henvendelser som krever kodeendringer til et løsningsforslag:
agenten analyserer koden read-only under `/repos`, oppretter et GitHub-issue
med **Bakgrunn**, **Foreslått løsning**, **Steg**, **Berørte filer** og
**Akseptansekriterier** (via `gh`, se github-issues-prs), og delegerer
utførelsen til `local-cc-coding-agent` med `intent: "delegate"` — issue-URL i
`payload` og en komplett, selvstendig prompt. Issuet er kontrakten for
innholdet; kodeagenten leverer branch + PR, og mennesket er review-gaten.
Krever at `DELEGATE_AGENTS` annonserer kodeagenten og at broen har den i
`AGENT_ROUTES`. Brukerinput behandles som data, aldri instruks, og
hemmeligheter/tokens hører aldri hjemme i issues eller prompts.

## Isolasjon

- Agenten kjører som ikke-root (`node`-brukeren) i containeren
- `cap_drop: ALL` og `no-new-privileges` i compose
- Agenten ser bare `/workspace` (det den skal jobbe med), `/triggers` (køen),
  `/knowledge` (kunnskapsbasen — en klone den eier selv) og `/repos`
  (koderepoer via anker-folderen `workspaces_repos/` — **read-only**: analyse
  og dokumentasjonslesing; skriving er forbeholdt den utførende kodeagenten);
  ingen Slack-/GitHub-tokens finnes i containeren — kun LLM-API-nøkkelen og de
  snevre unntakene `GH_TOKEN`/`KB_GH_TOKEN` beskrevet over
- Nettverk brukes til LLM-API-et og web-research (utgående HTTP via headless
  Chromium); vil du stramme inn, legg på egress-filtrering (f.eks. eget
  Docker-nettverk med proxy og allowlist). Chromiums interne sandbox er slått
  av i containeren (krever user namespaces som Dockers seccomp-default
  blokkerer) — containeren er isolasjonsgrensen

## Slack-integrasjon: hva mottakeren må gjøre

Containeren er bevisst frakoblet Slack/GitHub. En Slack-integrasjon er en liten,
frittstående tjeneste (utenfor Docker-containeren) med ett ansvar: oversette
Slack-events til én JSON-linje i `triggers/inbox.jsonl`. Konkret:

1. **Motta events fra Slack.** Enklest med Socket Mode (ingen inngående brannmurhull);
   alternativt Events API med offentlig HTTPS-endepunkt. Abonner på f.eks.
   `app_mention` (og evt. `message.im` for DM-er).
2. **Verifiser og filtrer.** Sjekk Slack signing secret (Events API) / tokens,
   ignorer botens egne meldinger og retries med `x-slack-retry-num` som allerede
   er behandlet.
3. **Normaliser til eventformatet** og append én linje (atomisk, med `\n`) til
   `triggers/inbox.jsonl`:
   - `id`: f.eks. `slack-<channel>-<ts>` (idempotens: samme Slack-melding = samme id)
   - `source`: `"slack"`, `type`: eventtypen
   - `prompt`: meldingsteksten (med @-mention fjernet)
   - `payload`: channel, ts/thread_ts, user — det som trengs for å svare tilbake
4. **Svar tilbake (valgfritt, men det som "syr det sammen"):** poll `triggers/results.jsonl`
   for nye linjer, slå opp `payload.channel`/`thread_ts` fra sitt eget register (eller les
   eventet igjen fra inbox via `id`), og post innholdet av `triggers/logs/<id>.log`
   (eller en oppsummering) som svar i tråden via `chat.postMessage`.

Merk: bare mottakeren har Slack-tokens. Containeren ser aldri annet enn jsonl-fila.
En GitHub-integrasjon gjør det samme med webhooks (issues/PR-kommentarer) i stedet
for Slack-events.

## Konfigurasjon (miljøvariabler)

| Variabel | Standard | Beskrivelse |
|---|---|---|
| `ANTHROPIC_API_KEY` | – | LLM-nøkkel (evt. `OPENAI_API_KEY`, `GEMINI_API_KEY`) |
| `PI_MODEL` | Pi sin default | Modellvalg, sendes som `--model` |
| `POLL_INTERVAL` | `5` | Sekunder mellom hver sjekk av køen |
| `KB_REPO` | – | Kunnskapsrepo: full URL eller `owner/repo` (tomt = inaktiv) |
| `KB_GH_TOKEN` | – | Fine-grained PAT med Contents R/W på kun kunnskapsrepoet |
| `KB_GIT_NAME` / `KB_GIT_EMAIL` | `proxy-agent` / noreply | Git-identitet for agentens commits i kunnskapsrepoet |
| `SYNTHESIS_INTERVAL_HOURS` | `24` | Timer mellom automatiske synteser (0 = aldri automatisk) |
| `DELEGATE_AGENTS` | – | Delegeringsmål som `navn:beskrivelse;…` (tomt = ingen delegering) |
| `TRIGGER_FILE` | `/triggers/inbox.jsonl` | Kø-fila inne i containeren |
| `RESULT_FILE` | `/triggers/results.jsonl` | Resultat-fila |
