# digdir-agents

Monorepo for en event-drevet agent-pipeline: én **integrations**-app som lytter
på Slack/GitHub, og et voksende sett **agenter** som utfører arbeidet isolert i
Docker — uten at agentene noensinne ser Slack-/GitHub-tokens.

```
Slack / GitHub
      │  (Socket Mode / notifications-polling — ingen inngående webhooks)
┌─────▼─────────┐
│ integrations/ │  eneste komponent med Slack-/GitHub-tokens
└─────┬─────────┘
      │ append                                      ┌──────────────────────┐
      ├──────────> agents/proxy-agent/triggers/ <───┤ agents/proxy-agent/  │
      │            inbox.jsonl                      │ (Pi i Docker)        │
      │ poll                                        └──────────────────────┘
      └──────────< triggers/results.jsonl + logs/<id>.log
```

## Komponenter

| Katalog | Rolle |
|---|---|
| [`integrations/`](integrations/) | Lytter på GitHub (notifications-polling) og Slack (Socket Mode), oversetter events til JSON-linjer i en agents `triggers/inbox.jsonl`, og poster svar fra `results.jsonl` tilbake dit de kom fra. |
| [`agents/proxy-agent/`](agents/proxy-agent/) | Pi-agent isolert i Docker. Poller sin egen `triggers/inbox.jsonl`, kjører agenten per event og skriver svar til `triggers/results.jsonl`. |
| [`agents/local-cc-coding-agent/`](agents/local-cc-coding-agent/) | Utførende kodeagent (senior): Claude Code kjørt interaktivt fra agent-katalogen (instrukser i `CLAUDE.md`). Mottar delegerte oppgaver via samme filkontrakt og leverer branch + PR. |
| [`agents/local-cc-jr-developer/`](agents/local-cc-jr-developer/) | Junior-kodeagent: samme runtime og filkontrakt, men mot en lokal modell via LM Studios Anthropic-kompatible API (`scripts/junior-agent.ps1`). Tar godt definerte, avgrensede lav-risiko-oppgaver og melder tilbake i stedet for å gjette. |

Kontrakten mellom integrations og agent er bevisst minimal: **to jsonl-filer i
agentens `triggers/`-katalog**. Formatet er beskrevet i
[`agents/proxy-agent/README.md`](agents/proxy-agent/README.md) (eventformat) og
[`integrations/README.md`](integrations/README.md) (resultatkontrakt med
`intent`/`reply`).

## Kom i gang

Hele pipelinen kjører i Docker via compose-fila på rotnivå (krever Docker
Compose v2.20+):

```powershell
# 1. Konfig per komponent
Copy-Item integrations\.env.example integrations\.env              # Slack-/GitHub-tokens
Copy-Item agents\proxy-agent\.env.example agents\proxy-agent\.env  # LLM-endepunkt/nøkkel

# 2. Dra opp alt
docker compose up -d --build
docker compose logs -f
```

Rot-compose-fila bare inkluderer komponentenes egne compose-filer, så hver
komponent kan fortsatt dras opp alene med `docker compose up` i sin katalog.
Integrations kan også kjøres rett på hosten under utvikling (Node >= 23:
`cd integrations && npm install && npm start`).

### Utvikling i forgrunnen

Default restart-policy er `unless-stopped`, slik at klyngen overlever omstart
av Docker/maskinen. Til utvikling — logger rett i konsollen, Ctrl+C stopper
alt, og ingenting starter igjen av seg selv — kjør:

```powershell
.\scripts\dev.ps1        # docker compose up --build uten restart-policy
```

Policyen styres av miljøvariabelen `RESTART_POLICY` i compose-filene
(default `unless-stopped`; dev-skriptet setter `no`).

## Drift og selvoppgradering

Runtime-oppgradering «i fart» gjøres av
[`scripts/self-update.ps1`](scripts/self-update.ps1), som kjører **på hosten,
utenfor det den oppgraderer** — en container kan ikke trygt stoppe og
gjenoppbygge seg selv, og agentene skal ikke ha Docker-tilgang (tilgang til
docker-socketen tilsvarer root på hosten). Utløseren er merge til
deploy-branchen: når agent-pipelinen selv har fått en PR merget, plukker
skriptet det opp — menneskelig review forblir gaten.

Skriptet følger A/B-prinsippet, men uten parallellkjøring (innboksene er
single-consumer — to samtidige klynger dobbeltbehandler events):

1. Kjørende images beholdes som `:rollback`-tag.
2. `git pull --ff-only` (nekter ved skitten arbeidskopi), og nye images
   **bygges mens den gamle klyngen fortsatt kjører** — byggefeil er den
   vanligste «brick»-årsaken og gir her null nedetid.
3. Bytte (`docker compose up -d`) og helsesjekk: containerne må kjøre stabilt
   uten restarts, og komponentenes klar-meldinger må dukke opp i loggene.
4. Ved feil rulles kode og images tilbake, og forrige versjon startes igjen.

Restart er billig i denne arkitekturen: kø og state ligger i jsonl-filer og
navngitte volumer, så events i innboksene overlever byttet.

Skriptet er også drifts-inngangen: er koden allerede oppdatert men klyngen
nede (første oppstart, etter reboot), startes den — én kommando dekker
«start alt og hold det oppdatert».

```powershell
pwsh scripts\self-update.ps1                     # én sjekk/oppgradering nå
pwsh scripts\self-update.ps1 -WatchSeconds 300   # følg deploy-branchen, poll hvert 5. min
```

For ubemannet drift kan skriptet registreres som gjentakende oppgave i Task
Scheduler i stedet for `-WatchSeconds`.

### Dedikert deploy-klone

Watcheren gjør `git pull` i klonen den kjører fra, og guardene (kun
deploy-branchen, kun ren arbeidskopi) stopper den så snart du utvikler i
samme klone. Kjør den derfor fra en egen klone som watcheren eier alene:

```powershell
git clone https://github.com/digdir/digdir-agents.git C:\data\deploy\digdir-ai-agents
cd C:\data\deploy\digdir-ai-agents
git checkout main
Copy-Item <dev-klone>\integrations\.env integrations\           # .env er gitignorert
Copy-Item <dev-klone>\agents\proxy-agent\.env agents\proxy-agent\
pwsh scripts\self-update.ps1 -WatchSeconds 300
```

Merk:

- **Kjør bare én klynge om gangen.** Compose-prosjektnavnet
  (`digdir-ai-agents`) er felles, så `up` fra én klone tar over containerne
  fra den andre (mountene peker da på den klonens kataloger). Det hindrer
  dobbeltkonsumering av innboksene — men vær bevisst på hvilken klone som
  «eier» klyngen.
- **Kø-katalogene følger klonen.** Kjører integrations fra deploy-klonen,
  havner delegerte oppgaver i *deploy-klonens*
  `agents/local-cc-coding-agent/triggers/` — den interaktive
  kodeagent-sesjonen må da startes derfra også.
- Navngitte volumer (`integrations-state`, `pi-home`) deles via
  prosjektnavnet, så pending replies og results-offsets overlever bytte av
  klone.

## PR-prosess: auto-merge på trygge stier

Alle endringer leveres som PR, men ikke alle trenger et menneske: PR-er som
kun rører trygge stier kan agentene merge selv — de kjører en
reviewer-subagent, poster reviewen som PR-kommentar, og setter labelen
`auto-merge`; en GitHub Action merger med den efemere `GITHUB_TOKEN` når
required checks er grønne (agent-tokenene har fortsatt ingen kode-tilgang).
Sensitive stier — agent-instrukser (`agents/*/CLAUDE.md`), skills,
Docker-filer, `integrations/src/`, `scripts/` og `.github/` — har menneskelig
eier i [`.github/CODEOWNERS`](.github/CODEOWNERS), og branch protection
(«Require review from Code Owners») blokkerer merge til code owner har
godkjent, uansett label. Husk at merge til deploy-branchen er auto-deploy
via self-update-watcheren — derfor er lista konservativ. Hele prosessen,
inkludert branch protection-oppsettet for repo-admin, er beskrevet i
[`doc/pr-prosess.md`](doc/pr-prosess.md).

## Legge til en ny agent

Pipelinen utvides ved å legge nye agenter under `agents/<navn>/`. En agent er
hva som helst som oppfyller kø-kontrakten:

1. **Opprett `agents/<navn>/`** med en `triggers/`-katalog. Kopier gjerne
   `agents/proxy-agent/` som mal (Dockerfile + entrypoint som poller køen), men
   runtime er valgfri — det eneste kravet er filkontrakten.
2. **Les events** fra `triggers/inbox.jsonl` (én JSON-linje per event, feltet
   `prompt` er det viktigste; husk å persistere lest posisjon slik at restart
   ikke reprosesserer gamle events).
3. **Skriv resultater** til `triggers/results.jsonl` med samme `id`, og gjerne
   `intent` (`action` / `feedback` / `ack`) + `reply` slik at integrations vet
   hvordan svaret skal leveres. Full logg per event legges i
   `triggers/logs/<id>.log`.
4. **Pek integrations på agenten** via `AGENT_TRIGGERS_DIR` i
   `integrations/.env` (f.eks. `../agents/<navn>/triggers`).

Agentene holdes uavhengige av hverandre: hver agent eier sin egen
`triggers/`-katalog, sitt eget image og sin egen `.env` — de skriver aldri i
hverandres kataloger.

## Delegering mellom agenter

En agent kan sende en oppgave videre til en annen agent ved å svare med
`intent: "delegate"` + et `delegate`-objekt i resultatlinja. **Broen gjør
rutingen**: integrations appender oppgaven som nytt event i målagentens
innboks (allowlist i `AGENT_ROUTES`), følger alle agenters `results.jsonl`,
og sørger for at det endelige svaret postes tilbake i den opprinnelige
Slack-tråden / GitHub-issuet. En hoppgrense (`AGENT_MAX_DELEGATION_HOPS`)
hindrer at to agenter kaster en oppgave frem og tilbake. Typisk flyt:
proxy-agenten analyserer og beskriver løsningen, og delegerer utførelsen til
kodeagenten — som leverer branch + PR med mennesket som review-gate. Se
[`integrations/README.md`](integrations/README.md) for kontrakten og planen i
[`doc/plans/agent-delegering.md`](doc/plans/agent-delegering.md).

## Dokumentasjon og kunnskap

Repo-spesifikke læringspunkter og dokumentasjon ligger i [`doc/`](doc/)
(OKF-konvensjoner: markdown + frontmatter, `index.md` som inngangsport).
Overordnet kunnskap på tvers av repoer hører hjemme i agentens sentrale
kunnskapsbase: et privat, instans-spesifikt repo som klones til
anker-folderen `workspaces_knowledge/` (gitignorert) og mountes inn i
proxy-agenten som `/knowledge`. Se planen i
[`doc/plans/kunnskap-og-laering.md`](doc/plans/kunnskap-og-laering.md).

## Sikkerhetsmodell

- Bare `integrations/` har Slack-/GitHub-tokens; agentene ser kun jsonl-filene,
  sitt eget `workspace/` og sin egen kunnskapsbase. To bevisste, snevre unntak
  hos proxy-agent: `GH_TOKEN` (kun issues/PR-er, ingen kode-tilgang) for
  github-skillen, og `KB_GH_TOKEN` (Contents kun på det private
  kunnskapsrepoet) for kunnskapsbasen — se
  [`agents/proxy-agent/README.md`](agents/proxy-agent/README.md).
- Agent-containerne kjører som ikke-root med `cap_drop: ALL` og
  `no-new-privileges`, og trenger bare nettverk mot LLM-endepunktet.
- Ingen inngående webhooks: integrations bruker polling (GitHub) og websocket
  (Slack), så alt kan kjøre fra en laptop bak brannmur.
