# nvt-bridge — bro mellom filkontrakten og levende nvt-instanser

Deterministisk supervisor **uten LLM** som kobler pipelinens filkontrakt til
[nvt-agent](https://github.com/mirkoSekulic/nvt-agent)-instanser: den poller
`agents/nvt-fat-developer/triggers/inbox.jsonl`, mapper hvert event til
topicets levende nvt-instans, og injiserer oppgaven som en prompt i sesjonen.

Rollen tilsvarer `scripts/agent-runner.ps1` for engangs-container-agentene,
men mot nvt i stedet for `docker run`. Design: [`doc/plans/nvt-agent-integrasjon.md`](../../doc/plans/nvt-agent-integrasjon.md).
Sporet er issue #95; dette er M1 (#97).

> **Status: kjernen er implementert og testet, og adapteren er kalibrert mot
> M0-funnene — men ikke kjørt ende-til-ende mot en ekte instans.** Alt som
> antar noe om nvt (init-flyten, containernavn, `agentdctl`-format,
> klar-mønstre) ligger isolert i [`src/nvt/docker.ts`](src/nvt/docker.ts).
> Kjernelogikken testes mot en fake-implementasjon og er uavhengig av hvordan
> den fila ender opp. Hva som fortsatt er uverifisert står i kommentaren øverst
> i fila.

## Hva den gjør

1. **Poller** `inbox.jsonl` og finner ubehandlede events — *id uten linje i
   `results.jsonl`*, samme dedupe-regel som resten av pipelinen.
2. **Avleder topic** = `payload.origin.event_id` uten delta-suffiks (`-d1`,
   `-d2`, …), ellers eventets egen id. Topicet er opphavstråden, så
   oppfølgingsevents havner i samme instans og samme samtale.
3. **Mapper topic → instans** i `state/topics.json`. Finnes ingen instans:
   `agent-init --user non-root` + `agent-up`. Finnes den: gjenbruk den levende
   sesjonen.
4. **Venter til sesjonen er klar** — `session-launched`-markøren finnes,
   tmux-sesjonen svarer, og panelet viser ikke en onboarding-dialog. Se
   «Klar-sjekken» under.
5. **Injiserer prompten**:
   `docker exec <instans> agentdctl prompt --source host --external`.
   `--external` er ikke valgfritt — delegerte prompts er upålitelig input, og
   flagget gir nvt sin «untrusted input»-preamble.
6. **Venter** på `agentdctl signal done`, og verifiserer at agenten faktisk
   skrev resultatlinja.
7. **Rydder**: `agent-down` for topics som har vært stille lenger enn TTL.
   Workspacet beholdes, så instansen kan gjenskapes med samme arbeidskopi.

Serielt innen et topic, parallelt på tvers (maks N). Serialiseringen er ikke
bare ytelse: alle events i et topic deler samme levende tmux-sesjon og samme
git-arbeidskopi.

### Fallback: aldri en fabrikert suksess

Agenten skriver resultatlinja selv. Broen skriver **kun** `status:"error"`:

| Situasjon | Hva broen gjør |
| --- | --- |
| Resultatlinje kom | Ingenting — agentens linje står |
| `signal done` uten resultatlinje innen nådefristen | `status:"error"` med forklaring |
| Verken resultatlinje eller `signal done` innen timeout | `status:"error"` med forklaring |
| Intern feil (instans nede, `make`/`docker` feilet) | `status:"error"` med feilmeldingen |
| Broen ble startet på nytt mens eventet var under arbeid | `status:"error"` — prompten sendes **ikke** inn igjen |

En `status:"ok"` kan altså bare komme fra agenten. At *hvert* dispatchet event
ender med en resultatlinje er en invariant: uten den ville eventet være evig
ubehandlet, og polleren dispatche det på nytt i ring (samme grunn som
agent-entrypointene har en EXIT-trap). Derfor er også bridge-loggen best
effort — den skal aldri kunne stå i veien for resultatlinja.

Kjent restrisiko: kommer agentens linje i akkurat samme øyeblikk som
nådefristen løper ut, kan begge linjene bli skrevet. Integrations tar den
første og hopper over den andre, så utfallet kan bli «agenten lyktes, men
brukeren fikk feilmeldingen». Vinduet er smalt (broen sjekker på nytt rett før
den skriver), men det er ikke null.

### Omstart midt i en oppgave

Injiserte events markeres i `state/topics.json` (`in_flight_event_id`) før
prompten sendes. Starter broen på nytt før eventet er kvittert ut, sendes
prompten **ikke** inn igjen — en andre prompt inn i en levende sesjon som står
midt i arbeidet ville vært verre enn et ærlig «ukjent utfall». Broen venter
nådefristen på at agenten skriver linja selv, og melder ellers feil med peker
til instansen. Ved `SIGTERM`/`SIGINT` avslutter broen først når events under
arbeid har fått skrevet resultatlinja si.

## Kalibrert mot M0-funnene

M0 (issue #96) kjørte hele kjeden manuelt og traff fem feller. Alle er
adressert i adapteren; her er hva du som drifter må vite.

### 1. Agenten kjører som non-root — ellers dør sesjonen

claude nekter `--dangerously-skip-permissions` som root, og tmux-sesjonen dør
innen 5 sekunder (tre forsøk, så exit). Broen kjører derfor
`agent-init --user non-root`, som gir `AGENT_RUN_USER=1000:1000`.

Merk at dette **ikke** går via `make agent-init`: make-målet kaller
`scripts/agent-init.sh --name --type --autonomy` og forwarder ikke `--user`.
Broen kaller scriptet direkte. `agent-up`/`agent-down` går fortsatt gjennom
make.

### 2. Volum-hygiene ved bytte root → non-root

Named-volumene til en instans som *allerede* har kjørt som root beholder
root-eierskapet, og bootstrap feiler med `Permission denied` selv etter at
configen er rettet. Volumene må slettes:

```bash
make agent-down NAME=<navn>
docker volume rm agent-<navn>_agent-home agent-<navn>_docker-data
```

Dette gjelder bare instanser opprettet før kalibreringen. Workspacet ligger i
nvt-sjekkouten, ikke i volumene, men **alt som bare fantes i agentens
`$HOME`** (CLI-state, cacher) forsvinner. Har du ikke-pushet arbeid i en slik
instans, hent det ut via code-server først.

### 3. Stier uid 1000 kan traversere

Workspacet bind-mountes på samme absolutte sti inne i containeren, og agenten
er `1000:1000`. Ligger `NVT_ROOT` under `/root` (mode 0700), feiler bootstrap
inne i containeren med en kryptisk `Permission denied`. Broen sjekker derfor
hele stien til `NVT_ROOT` og triggers-katalogen ved oppstart — sistnevnte også
for *skrive*tilgang, siden agenten appender resultatlinja selv. Symlenker løses
først, så en lenke ikke kan skjule at målets foreldre er stengt. Bruk f.eks.
`/srv/nvt-agent`.

**Hva sjekken faktisk kan svare på:** mode-bitene broen leser må være hostens.
Det stemmer når broen kjører som node-prosess på hosten (`npm start`) — da
**nekter den å starte** med en melding om hvilket ledd som stopper uid 1000.
Kjører broen selv i container, er bare `NVT_ROOT` og triggers-katalogen
bind-mountet; mellomleddene er mount-point-foreldre Docker har laget, med helt
andre rettigheter enn hostens. Da logges funnet som en **advarsel** i stedet, og
stien må kontrolleres på hosten. Samme gjelder macOS, der Docker Desktop mapper
eierskap i virtiofs-laget. `NVT_BRIDGE_SKIP_PATH_CHECK=1` slår sjekken av helt.

### 4. Commit-identitet må være eksplisitt i `agent.yaml`

`static_token`/broker-token-providere kan ikke rapportere commit-identitet, så
`identity.mode: provider` (som står i nvt-malens kommenterte eksempel) gir en
agent uten identitet — og `git commit` feiler *etter* at jobben er gjort.

Broen genererer derfor `.agents/<navn>/agent.yaml` selv, med
`identity.mode: explicit` og navn/e-post fra `NVT_GIT_IDENTITY_NAME` og
`NVT_GIT_IDENTITY_EMAIL`. Bot-navnet ligger bevisst i env, ikke i repoet.
Configen skrives **én gang**, før `agent-init`; en eksisterende config røres
aldri (den kan være håndredigert), men identiteten verifiseres ved hver
oppstart, og `mode: provider` stopper broen med en forklaring.

### 5. Klar-sjekken: onboardingen spiser første prompt

claude-onboardingen (velkomstskjerm, trust-dialog) fanger tastetrykk. En prompt
som injiseres da forsvinner **uten spor** — ingen feil, ingen `signal done`,
bare en timeout en time senere. `agentd` venter selv på
`session-launched`-markøren og tmux-sesjonen, men vet ikke om dialogene.

Onboardingen er en **sesjonsstart**-tilstand, så gaten har to nivåer:

| Tilstand | Krav før prompten sendes |
| --- | --- |
| Fersk sesjon (klar-prompten er ikke sett i denne container-inkarnasjonen) | Markøren finnes, tmux svarer, og panelet (`tmux capture-pane`) viser klar-prompt. Står en onboarding-dialog der, sendes ett Enter per runde (maks `NVT_BRIDGE_MAX_ONBOARDING_ENTER`, default 3). |
| Bekreftet sesjon | Markøren finnes og tmux-sesjonen lever. Panelet leses ikke. |

Enter sendes altså aldri i blinde — bare når en dialog faktisk er tegnet i en
sesjon som ikke har vært i bruk.

To detaljer som er verdt å kjenne:

- **Panelet leses bare på en fersk sesjon**, fordi det ellers inneholder
  transkriptet — inkludert den delegerte oppgaveteksten, som er upålitelig
  input. Lot vi den styre gaten, kunne en avsender som skrev
  «Do you trust the files in this folder?» i oppgaven fått broen til å tro at en
  dialog sto der, og dermed blokkert topicet. Mønstrene er i tillegg
  linjeankret, slik at et ekko av oppgaveteksten (som står bak en `>`-prompt)
  ikke matcher.
- **Klar-tilstanden gjelder én container-inkarnasjon** (`docker inspect` sin
  `Id` + `StartedAt`). Restartes containeren utenfor broen — `docker restart`,
  host-reboot med `restart: unless-stopped` — er det en ny sesjon med ny
  onboarding, og gaten blir streng igjen selv om broen aldri kjørte `agent-up`.

Blir sesjonen ikke klar innen `NVT_BRIDGE_READY_TIMEOUT_SECONDS`, kaster
sjekken, prompten sendes ikke, og eventet får en `status:"error"`-linje med
peker til code-server. Feilmeldingen gjengir **ikke** panelinnholdet — den går
videre til Slack, og en tmux-skjerm kan inneholde hva som helst.

Mønstergjenkjenning mot et TUI er heuristikk. Bytter claude ordlyd, kan
mønstrene settes med `NVT_READY_PATTERN` / `NVT_ONBOARDING_PATTERN` (regex,
case-insensitive) uten kodeendring. Feiler klar-mønsteret å matche en sesjon som
*er* klar, blir utfallet en ærlig feilmelding og en uendret innboks — ikke en
tapt oppgave.

## Oppstart

```bash
cp .env.example .env
# fyll inn NVT_ROOT (nvt-sjekkouten). Se kommentarene i .env.example.
npm start
```

Tørrkjøring uten nvt — logger hva som ville blitt kjørt, og skriver
`status:"error"`-linjer (ingen fabrikert suksess, heller ikke her):

```bash
NVT_BRIDGE_DRY_RUN=1 npm start
```

Tester og typecheck:

```bash
npm test        # node --test, ingen byggesteg
npm run typecheck
```

### Container (docker-outside-of-docker)

`docker-compose.yml` kjører broen i container med **hostens Docker-socket**
mountet, slik at oppsettet blir likt på Windows/WSL2 og Mac. Det er et bevisst
unntak forbeholdt tiltrodd infrastruktur: broen er deterministisk kode uten
LLM, og *agentcontainerne* får aldri socketen (nvt-instansen har sin egen,
isolerte dind).

**Den viktigste fella:** compose-bind-mounts løses av *hostens* daemon, så
stier broen sender videre må være host-gyldige. Derfor mountes nvt-sjekkouten
og agentens `triggers/` inn i broen på **samme absolutte sti som på hosten**,
styrt av `NVT_ROOT` og `PIPELINE_ROOT`. Adapteren gjør ingen sti-oversetting —
det er et bevisst valg, ikke en glipp. Fallback er å kjøre broen som en vanlig
node-prosess under WSL2; koden er den samme.

## Kode

| Fil | Rolle |
| --- | --- |
| `src/triggers.ts` | Filkontrakten: lesing, dedupe-regelen, resultatlinjer |
| `src/topic.ts` | Topic-avledning og instansnavn |
| `src/scheduler.ts` | Serielt per topic, parallelt på tvers (maks N) |
| `src/state.ts` | `state/topics.json` — topic → instans |
| `src/prompt.ts` | Prompten som injiseres, og fallback-forklaringene |
| `src/bridge.ts` | Orkestreringen og fallback-invariantene |
| `src/nvt/driver.ts` | Interfacet mot nvt |
| `src/nvt/fake.ts` | Fake-implementasjon for tester (håndhever klar-sjekk før prompt) |
| `src/nvt/dryrun.ts` | Tørrkjøring |
| `src/nvt/docker.ts` | **Ekte adapter — kalibrert mot M0-funnene** |
| `src/nvt/ready.ts` | Klar-tilstand: klassifisering av tmux-panelet |
| `src/nvt/paths.ts` | Sti-validering mot uid 1000 (fail-fast ved oppstart) |
| `src/nvt/agentConfig.ts` | `agent.yaml` med eksplisitt commit-identitet |

Node ≥ 22.6, ingen runtime-avhengigheter, ingen byggesteg (native
type-stripping), `node --test` — samme stack som `integrations/`.

## Ikke med i M1

- **Driver-leasen** (headless ↔ hands-on ↔ ekstern) er M3 (#99). I M1 er det
  ingen håndheving, så et menneske som jobber i code-server samtidig som broen
  injiserer prompts i samme topic kan kollidere i arbeidskopien.
- **Admin-konsollen** er M2 (#98). `state/topics.json` og `events.jsonl` er
  lesbare i mellomtiden.
- **Rendring av `AGENTS.local.md`** inn i instansen hører til
  `agent-init`-oppsettet og kalibreres mot M0. Malen ligger i
  [`agents/nvt-fat-developer/AGENTS.local.md.tmpl`](../../agents/nvt-fat-developer/AGENTS.local.md.tmpl).
- **Ruta i `AGENT_ROUTES`** er driftskonfig i deploy-klonens `.env` og tas ved
  utrulling — `integrations/src/` er urørt.
