---
type: plan
title: nvt-agent som «fat-dev»-backend — integrasjon, admin-konsoll og driver-lease
description: Integrere Mirkos nvt-agent (isolerte agentmiljøer med code-server, agentd og broker) som neste generasjons utførende kodeagent i pipelinen, med bro mot filkontrakten, en separat admin-konsoll på tvers av orkestrator og agenter, og en lease-mekanisme for å veksle mellom headless-, hands-on- og ekstern modus på samme work-in-progress.
timestamp: 2026-07-24T00:00:00Z
---

# nvt-agent-integrasjon («fat-dev»)

## Hva nvt-agent er (kartlagt 2026-07-24)

[nvt-agent](https://github.com/mirkoSekulic/nvt-agent) (lokal klone:
`C:\data\github\olebhansen\nvt-agent`) kjører kodeagenter (Codex/Claude Code)
i isolerte, reproduserbare miljøer — lokalt via Docker Compose, i produksjon
via en Kubernetes-operator. Ett agentmiljø består av:

- **Runtime-container** med workspace, en **langlevende interaktiv CLI-sesjon
  i tmux** (claude/codex), **code-server** (VS Code i nettleser, port 4090,
  rutet via Traefik som `http://<navn>.agent.localhost:4090`) og egen
  Docker-daemon (dind).
- **agentd** — container-lokal daemon på Unix-socket med JSONL-protokoll:
  `prompt` (settes i kø og pastes inn i tmux-sesjonen; `external: true` gir
  automatisk «untrusted input»-preamble), `status`, `health`,
  `event.publish`. Alle hendelser logges append-only til `events.jsonl`;
  `agentdctl subscribe` tailer den, `agentdctl signal done` publiserer
  `plugin.agent.signal.done`.
- **Plugins** (kjørbare, små kontrakter): `checkout-repos`,
  `git-credentials`/`git-host-credentials`, `github-watcher` (poller PR-er og
  prompter agenten ved kommentarer/checks), `event-webhook` (videresender
  agentd-hendelser til et HTTP-endepunkt), broker-auth-plugins.
- **Broker** — eneste langlevende credential-eier. Providers: GitHub App
  (korte, repo-scopede installasjonstokens), statiske tokens, Claude/Codex
  OAuth med refresh. Per-agent **grants** (default deny, snevrere enn
  providerens tak), append-only **audit-logg**. I *mediated mode* ser agenten
  bare placeholders; ekte credentials injiseres i egress-laget (egressd).
  *Direct mode* (lokal utvikling) gir filbaserte credentials.
- **Producer/operator** (k8s-sporet): `github-comments`-produceren poller
  issue-kommentarer etter `/nvtagent pr create` og oppretter AgentRuns via
  AgentSchedule; gateway ruter nettlesersesjoner. Lokalt erstattes dette av
  `make agent-init/agent-up` og Traefik.

**Viktigste egenskaper for oss:** sesjonskontinuitet er en *levende prosess*
(ingen `--resume`-plumbing — oppfølging = ny prompt inn i samme tmux-sesjon);
mennesket kan hoppe inn i nøyaktig samme miljø via code-server; credential-
modellen (broker + grants + audit) er et hakk bedre enn PAT-er i `.env`; og
`external: true`-preamblen sammenfaller med vårt prinsipp om at events er
upålitelig input.

## Mål

1. **`nvt-fat-developer`** som ny utførende kodeagent bak samme filkontrakt
   som i dag (`triggers/inbox.jsonl` inn, `results.jsonl` + logg ut) — uten
   endringer i integrations utover én rute i `AGENT_ROUTES`.
2. **Samtale går fortsatt gjennom orkestratoren**: agenten poster aldri selv
   i Slack/GitHub-tråder — svar leveres som resultatlinjer. Git/PR-arbeid
   gjør agenten selv, men med broker-utstedte, repo-scopede tokens i stedet
   for PAT i `.env`.
3. **Admin-konsoll som egen app**: tverrgående visning per topic
   (Slack/GitHub-tråd ↔ delegeringer ↔ agentkjøringer ↔ PR-er), dyplenker
   inn i code-server, live status fra `events.jsonl`.
4. **Driver-lease per work-in-progress**: veksle kontrollert mellom
   *headless* (pipelinen driver), *hands-on* (Ole i code-server/tmux i samme
   miljø) og *ekstern* (sr-dev/Claude Code på egen sjekkout av samme branch).

## Arkitektur

```text
Slack/GitHub ──> integrations ──────────────┐ (uendret nav)
                    │  AGENT_ROUTES         │
                    ▼                       │
   agents/nvt-fat-developer/triggers/       │
        inbox.jsonl   results.jsonl ◄───────┤
             │              ▲               │
             ▼              │ (agenten      │
        ┌─────────┐         │  skriver      ▼
        │nvt-bridge│        │  selv)   svar til tråd/issue
        │ (host)   │        │
        └────┬─────┘        │
             │ topic → instans; docker exec agentdctl prompt
             ▼
   nvt-agentmiljø per topic (compose-prosjekt agent-<topic>)
   ├─ claude i tmux  ├─ agentd + events.jsonl  ├─ code-server :4090
   ├─ /triggers (mount av agentens triggers/)  └─ broker-token (grant)
             │
             ▼ git push / gh pr create (broker-utstedt token)
          GitHub
```

### nvt-bridge (ny, deterministisk — ingen LLM)

Rollen tilsvarer jr-runneren i issue #90, men mot nvt i stedet for
`docker run`: en liten supervisor på hosten (Node/TS i monorepoet, samme
stack som integrations) som

1. poller `agents/nvt-fat-developer/triggers/inbox.jsonl` og finner
   ubehandlede events (id uten linje i `results.jsonl` — samme regel som i
   dag),
2. avleder **topic** = `payload.origin.event_id` uten delta-suffiks, og
   mapper topic → nvt-instans i en egen state-fil
   (`state/topics.json`): finnes ingen, kjør `agent-init`/`agent-up` med
   topic-slug som navn; finnes den, gjenbruk den levende instansen,
3. leverer prompten inn i sesjonen:
   `docker exec agent-<topic>-agent-1 agentdctl prompt --source host --external "<prompt + kontrakt>"`,
4. håndhever **driver-leasen** (se under): events til et topic der leasen
   ikke er `headless` blir liggende i innboksen til leasen frigis,
5. rydder: TTL-basert `agent-down` for inaktive topics (workspace
   beholdes; instansen kan gjenskapes).

Bridgen er tiltrodd kode med Docker-tilgang — som integrations og
self-update, og i tråd med prinsippet om at *agenter* aldri får
Docker-socket. Serielt per topic, parallelt på tvers (maks N instanser).

**Miljøuavhengighet (besluttet retning):** bridgen skal kunne kjøre i
container (docker-outside-of-docker), så oppsettet blir likt på Oles
Windows/WSL2 og Mirkos Mac:

- Bridge-imaget er node-slim + docker-cli/compose-cli, med **hostens
  Docker-socket mountet** (`/var/run/docker.sock`). Det er et bevisst
  unntak forbeholdt tiltrodd infrastruktur — bridgen er deterministisk
  kode uten LLM, og agentcontainere får aldri socketen.
- Kjent felle: compose-bind-mounts løses av *hostens* daemon, så stier i
  `.agents/<navn>/env` må være host-gyldige. Løsning: nvt-sjekkouten og
  monorepoets `agents/nvt-fat-developer/` mountes inn i bridgen på samme
  absolutte sti som på hosten (én `NVT_ROOT`-/`PIPELINE_ROOT`-env styrer
  begge), slik at genererte compose-filer virker uendret. M0/M1
  verifiserer dette på WSL2; fallback er å kjøre bridgen som vanlig
  node-prosess under WSL2 — koden er den samme.
- nvt selv kjøres fra WSL2-siden (Make/bash-flyten er Linux-first, og
  Docker Desktop deler daemonen), med filene på Linux-filsystemet for
  ytelse.

### Resultatkontrakten består — agenten skriver selv

Agentens `triggers/`-katalog mountes inn i nvt-instansen som `/triggers`
(én mount-linje, samme mønster som integrations-komposen). Prompten fra
bridgen bærer event-id og kontrakten, og `AGENTS.local.md` (genereres av
vår `agent-init`-mal) inneholder en tilpasset utgave av dagens
CLAUDE.md-protokoll:

- «Kjenn din begrensning»-regelen, branch+PR-kontrakten (`agent/<navn>`,
  `gh pr create --base <base>`, `Closes #<nr>`, aldri merge), retro/
  KB-læringssteget og resultatlinje-formatet gjenbrukes ordrett der det kan.
- Forskjellen fra i dag: **ingen innboks-polling** («du får oppgaver som
  prompts i denne sesjonen») og *i tillegg* `agentdctl signal done` etter
  resultatlinja — signalet er for bridge/konsoll (status/lease), mens
  resultatlinja er kontrakten mot integrations.

Dermed er integrations uendret: den ser en helt vanlig agent. Fallback om
agenten glemmer resultatlinja: bridgen tailer `events.jsonl`; ved
`plugin.agent.signal.done` uten tilhørende resultatlinje innen kort tid
skriver bridgen en `status:"error"`-linje med forklaring (aldri en
fabrikert suksess).

### Identitet og tilgang: samme bot-konto, dedikert token via brokeren

**Besluttet (2026-07-24):** vi beholder pipelinens bot-konto som identitet,
med **dedikerte fine-grained PAT-er per agent** — samme modell som
jr-tokenet fra issue #90 (Contents RW + Pull requests RW, kun
arbeidsrepoene).

- Tokenet legges i brokeren som **`static_token`-provider** (ikke i
  agentens `.env`): da får vi likevel brokerens grant-innsnevring per
  instans og audit-loggen, og agentcontaineren kan senere flyttes til
  mediated mode uten å endres. Tokenverdien bor i `.broker/env` på hosten
  (gitignorert, 0600).
- **GitHub App er utsatt til M4** som valgfri oppgradering (kortlevde
  installasjonstokens i stedet for statisk PAT) — broker-configen er
  byttbar per provider, så dette er en ren config-endring senere.
- PR-er attribueres til bot-kontoen, som ikke skal stå i
  `GITHUB_ALLOWED_USERS`, og hvis hendelser self-filtreres av polleren som
  i dag. Botnavnet holdes utenfor repoet — kun env/`.broker/`-config.
- **Modell: llm-gatewayen med subscription-OAuth** (samme oppsett som jr-/
  sr-agentene kjører på i dag, se `apps/llm-gateway/README.md`):
  runtime-miljøet settes med `ANTHROPIC_BASE_URL` mot gatewayen på hosten
  (`http://host.docker.internal:8787`; merk at agentcontaineren bruker
  `network_mode: service:docker`, så host-oppslaget må verifiseres i M0) og
  `ANTHROPIC_AUTH_TOKEN=<konsument-nøkkelen>`. fat-dev får sin **egen
  konsument med egen fake-nøkkel** i gatewayens `routes.json`, slik at
  trafikken kan skilles i loggen og nøkkelen revokeres alene.
  **Modell-allowlisten håndheves i gatewayen**: konsumentens regler
  bestemmer hvilke modeller som slipper gjennom (`whenModel`), og en
  forespørsel uten treff avvises (fail closed) — agenten kan altså ikke
  velge en dyrere eller uønsket modell selv.
  Det ekte OAuth-tokenet bor **kun i gatewayens `.env`** og er aldri inne i
  agentcontaineren — samme credential non-possession som brokeren gir for
  git-tokenet. LM Studio (eller en annen Anthropic-kompatibel backend) er
  et alternativ: ett bytte av upstream i `routes.json`, uten å røre
  agentens env.
- `--dangerously-skip-permissions` settes av nvt selv ved
  `autonomy: trusted-local` — akseptabelt av samme grunn som i issue #90:
  containeren er isolasjonsgrensen. Aldri på host.

### Hva vi *ikke* tar i bruk (nå)

- **`github-comments`-produceren**: integrations er produceren vår; den
  eier allowlist, ruting og svar-ruter.
- **`github-watcher`-pluginen for samtale**: PR-/issue-kommentarer når
  agenten via integrations → delta-event → bridge → prompt, så samtalen
  forblir i orkestratoren. (Watcheren kan senere, som opt-in, prompte
  agenten direkte ved *CI-check*-overganger — det er maskinsignal, ikke
  samtale. Av i M1.)
- **Operator/gateway/k8s**: lokalt compose-spor først. K8s-sporet er
  naturlig neste steg når dette skal av laptopen.
- **Mediated egress**: direct mode i M1 (enklere), mediated som
  hardening i M4 — da slutter selv scopede tokens å eksistere inne i
  agentcontaineren.

## Admin-konsoll (egen app: `apps/agent-console/`)

Dagens nvt-«GUI» er én dashboard-tabell i gatewayen (k8s) — ingenting å
gjenbruke lokalt utover URL-mønsteret. Vi bygger en liten lokal webapp,
adskilt fra integrations:

- **Backend uten samtale-tokens**: leser (read-only) alle agenters
  `triggers/`-filer, bridgens state (`topics.json`, leases) og nvt-instansers
  `events.jsonl`; snakker med bridgen over et lokalt HTTP-API for handlinger.
  Slack/GitHub-posting forblir i integrations — konsollen får aldri de
  tokene.
- **Topic-tverrsnitt** (kjernevisningen): per topic vises opphavstråden
  (Slack/GitHub-referanse), delegeringskjeden (inbox/results på tvers av
  agenter, inkl. proxy-agentens og debrief-events), nvt-instansens
  live-status (siste events, køede prompts) og PR-lenker — det «på tvers»-
  bildet som i dag krever fire terminaler.
- **Dyplenker**: «Åpne i VS Code» → `http://<topic>.agent.localhost:4090`.
- **Handlinger (via bridge, fase 2)**: bytt lease-modus, send
  operatør-prompt inn i en sesjon (merkes som `source: operator`), stopp/
  gjenskap instans. Konsoll-initierte prompts går via bridgen — konsollen
  har heller ikke Docker-tilgang.
- Teknisk: Node/TS + enkel SPA/SSE for live-tail; lytter kun på
  `127.0.0.1`.

## Driver-lease: headless ↔ hands-on ↔ ekstern

Ett topics work-in-progress = nvt-instansens workspace + agent-branchen.
Kun én «driver» om gangen, håndhevet av bridgen via en lease-fil per topic
(`state/leases/<topic>.json`):

```json
{"mode":"headless|handson|external","holder":"bridge|ole|sr-dev",
 "acquired_at":"<UTC>","note":"<valgfri>"}
```

- **headless** (default): bridgen injiserer prompts; agenten jobber
  autonomt. Full sesjonslogg i `events.jsonl`.
- **hands-on (nvt-modus)**: Ole tar leasen (konsollen eller CLI). Bridgen
  slutter å injisere — nye events til topicet blir liggende i innboksen
  (opphavstråden beholder «working»-reaksjonen, ingenting går tapt). Ole
  åpner code-server i *samme* miljø: kan redigere filer, bruke terminalen,
  og til og med snakke direkte med agentens levende CLI i tmux-sesjonen
  (`tmux attach -t agent`) — pipelinens historikk og Oles innspill er da
  samme samtale.
- **ekstern (sr-dev-modus)**: arbeidet skal fortsette utenfor instansen
  (Oles egen Claude Code, eller sr-agenten, på egen sjekkout). Git er
  synkroniseringspunktet — derfor en **sjekkpunkt-protokoll** i
  leasebyttet: før frigivelse ber bridgen agenten committe og pushe WIP til
  agent-branchen (`agentdctl prompt` med sjekkpunkt-instruks, vent på
  `signal done`); ved gjenopptak i headless er første prompt «pull og les
  deg opp på ny historikk på branchen». Samme protokoll andre veien: den
  eksterne driveren pusher før leasen gis tilbake.
- Leasen er en koordineringsmekanisme, ikke en sikkerhetsgrense: den hindrer
  at to drivere skriver i samme arbeidskopi samtidig og gjør bytter
  eksplisitte og loggede (lease-endringer publiseres som events → synlige i
  konsollen).

## Forhold til issue #90 (jr i container)

**Status 2026-07-24: #90 er gjennomført og under testing** (dedikert
jr-token er laget). Dette sporet er arvtakeren: nvt løser #90s tre
problemer (identitet, filsystem, sesjonsmodell) og mer. Når M1 her er
verifisert, pensjoneres jr-containeren eller re-provisjoneres som «fat-jr»
(nvt-instans med en billigere modell-rute i gatewayen, evt. LM Studio som
upstream). Token-modellen fra #90 (dedikert fine-grained
PAT på bot-kontoen) gjenbrukes som broker-provider her — men fat-dev bør få
sitt *eget* token, ikke dele jr-ens, så de kan skilles i audit og
revokeres hver for seg.

## M0-funn (gjennomført 2026-07-30, WSL2 Ubuntu-24.04)

Hele kjeden er bevist: prompt via `agentdctl` → claude (Sonnet via
llm-gatewayen) i levende tmux-sesjon → branch + commit + push + PR i
testrepoet (`nvt-m0-test`) opprettet av bot-identiteten med broker-utstedt
token — og en oppfølgings-prompt i samme sesjon fortsatte samme samtale
(PR-en fikk commit nr. 2 uten re-forklaring av kontekst). code-server
svarer bak Traefik. Verifisert: `FATDEV_GH_TOKEN` finnes ikke i
agentcontainerens env; i direct mode/file-bundle ligger en liten
credential-config (`~/.nvt-agent/git-credentials/config.yaml`, ~200 bytes)
som `git-credential-nvt`-helperen og `gh-auth`-wrapperen bruker —
tokenverdien selv bor kun i `.broker/env` på hosten. Modell-allowlisten i
gatewayen håndhever fail-closed (verifisert med avvist modell).

Feller M1-implementasjonen må kjenne (alle truffet i praksis):

1. **claude nekter `--dangerously-skip-permissions` som root** → agenten
   MÅ kjøres i nvt-ens non-root-modus (`agent-init --user non-root`).
   Symptom ellers: tmux-sesjonen dør innen 5 s, tre forsøk, container exit.
2. **nvt-sjekkouten kan ikke ligge under `/root`** på verten: workspace
   bind-mountes på samme absolutte sti i containeren, og uid 1000 kommer
   ikke gjennom `/root` (700). Bruk f.eks. `/srv/nvt-agent`. (Samme krav
   som bridge-planens «host-gyldige stier».)
3. **Bytte root→non-root på eksisterende agent krever at named-volumene
   slettes** (`agent-m0_agent-home`, `agent-m0_docker-data`) — de beholder
   root-eierskap og gir `Permission denied` ved bootstrap.
4. **`static_token`-provideren støtter ikke `identity.mode: provider`** —
   commit-identitet må settes eksplisitt i `agent.yaml`
   (`mode: explicit` + navn/e-post for bot-kontoen).
5. **claude-onboardingen spiser første prompt**: velkomstskjerm +
   trust-dialog må gjennom (to Enter i tmux) før agentd-prompts når
   claude; preseed-`settings.json` dekker det ikke i dag. Bridgen (M1) må
   enten preseede onboarding-state eller vente på klar-prompt før første
   injeksjon.
6. **Gatewayen nås fra agentcontaineren** via `host.docker.internal:8787`
   (200 OK) tross `network_mode: service:docker` — det åpne spørsmålet fra
   planen er avklart, ingen gateway-IP-triksing nødvendig.
7. Diverse: `gh` finnes i runtime-imaget og `gh-auth` (plugin-eksportert
   wrapper) gir PR-opprettelse uten token i miljøet; Windows-siden må ikke
   røres — alt kjøres fra WSL2-distroen med filene på Linux-filsystemet.

## Milepæler

**M0 — Sandkasse-bevis (manuelt, ingen repo-endringer).** Bygg nvt fra
WSL2 (`make runtime-build broker-build …`, `infra-up`), én claude-instans i
direct mode med `static_token`-grant (dedikert bot-PAT) mot et testrepo, og
llm-gatewayen som modell-backend (`ANTHROPIC_BASE_URL` mot gatewayen på
hosten, port 8787 — verifiser at gatewayen faktisk nås fra nvt-instansen,
som kjører med `network_mode: service:docker`). Verifiser for hånd:
prompt via `agentdctl` → agenten lager branch+PR med broker-token;
code-server-innhopp; `events.jsonl`-tailing.
*Akseptanse: PR opprettet av bot-identiteten uten token i containeren, mot
llm-gatewayen med subscription-OAuth (OAuth-tokenet kun i gatewayens
`.env`).*

**M1 — Bridge + agent bak filkontrakten.** Ny katalog
`agents/nvt-fat-developer/` (triggers/, README, instruks-mal for
`AGENTS.local.md`, `.env.example`), `nvt-bridge` (Node/TS, `apps/` eller
`integrations/`-søsken), rute i `AGENT_ROUTES`, mount av `triggers/` inn i
instansene. *Akseptanse: delegert issue → PR av bot-identitet →
resultatlinje → svar i opphavstråden; oppfølgingsevent i samme topic lander
i samme levende sesjon; to topics kjører parallelt i hver sin instans;
deploy-treet urørt.*

**M2 — Admin-konsoll v1 (read-only).** `apps/agent-console/` med
topic-tverrsnitt, live events, code-server-dyplenker. *Akseptanse: ett
topic kan følges fra Slack-melding via delegering til PR uten terminal.*

**M3 — Driver-lease.** Lease-filer + bridge-håndheving +
sjekkpunkt-protokoll + konsoll-toggle. *Akseptanse: bytte
headless→hands-on→headless midt i en oppgave uten tapte events eller
commit-kollisjoner; ekstern modus verifisert med sr-dev på samme branch.*

**M4 — Hardening/utvidelser.** GitHub App-provider (kortlevde tokens i
stedet for statisk PAT), mediated egress, TTL/opprydding, opt-in
github-watcher for CI-checks, evt. k8s-spor.

## Beslutninger (Ole, 2026-07-24)

1. **Identitet:** samme bot-konto, dedikerte fine-grained PAT-er per agent
   (som jr-tokenet fra #90); GitHub App utsatt til M4.
2. **Vertsmiljø:** Windows + WSL2 hos Ole, Mac hos Mirko —
   miljøuavhengighet er et krav; bridgen bygges for container-kjøring
   (docker-outside-of-docker) med host-node-prosess som fallback.
3. **Modell (endret 2026-07-27, [#96](https://github.com/digdir/digdir-ai-agents/issues/96)):**
   llm-gatewayen med subscription-OAuth først, som jr-/sr-agentene kjører i
   dag — fat-dev blir en egen konsument i gatewayens `routes.json` med egen
   fake-nøkkel og modell-allowlist, og OAuth-tokenet bor kun i gatewayens
   `.env`. LM Studio er nedgradert til *alternativ* backend (bytte av
   upstream i `routes.json`). Erstatter den opprinnelige beslutningen om LM
   Studio/«fat-jr» som første backend.
4. **#90:** allerede gjennomført og under testing; nvt-sporet er
   arvtakeren.

## Sikkerhet (invarianter som består)

- Kun integrations har Slack/GitHub-samtaletokens; konsoll og bridge får
  dem aldri. Agent-svar til mennesker går utelukkende via resultatlinjer.
- Agentcontainere får aldri Docker-socket til *hostens* daemon (nvt-dind er
  instansens egen, isolerte daemon). Bridge/konsoll-backend er tiltrodd,
  deterministisk kode.
- Delegerte prompts er upålitelig input — nvt forsterker dette med
  `external: true`-preamble i selve injeksjonen.
- Broker-nøkler (App-privatnøkkel) bor i `.broker/` på hosten (gitignorert,
  0600) og når aldri agentcontainere; grants er default-deny og auditerte.
- Branch protection + CODEOWNERS-gaten gjelder uendret; bot-identiteten
  skal ikke i `GITHUB_ALLOWED_USERS`, og merges skjer fortsatt kun via
  auto-merge-workflowens efemere token eller Oles review.
- Aldri hemmeligheter i learnings, logger, resultatlinjer, events eller
  konsollen.
