---
type: plan
title: Kunnskaps- og læringsprosess for agent-pipelinen
description: Plan for hvordan agentene bygger, konsulterer og vedlikeholder kunnskap — OKF som format, et privat instans-konfigurert kunnskapsrepo som sentral wiki, <repo>/doc for repo-spesifikk kunnskap, og en læringsloop inspirert av yoyo-evolve og Karpathys LLM-wiki.
tags: [kunnskap, okf, skills, proxy-agent, laering]
timestamp: 2026-07-21T00:00:00Z
---

# Kunnskaps- og læringsprosess

## Mål

Agent-pipelinen skal **lære av arbeidet den gjør** og **konsultere det den
har lært** før nye oppgaver — uten at kunnskapen låses til én agent, ett
verktøy eller én leverandør. Kunnskapen skal være lesbar for både mennesker
og agenter, versjonert i git, og vokse som et sammensatt hele (compounding)
i stedet for å gjenoppdages per oppgave.

## Inspirasjonskilder — hva vi tar fra hver

| Kilde | Hva vi adopterer |
|---|---|
| [OKF](https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing) (Open Knowledge Format, Google, v0.1) | **Formatet**: markdown-filer i katalog-hierarki, YAML-frontmatter med `type` som eneste påkrevde felt, sti = konseptets identitet, markdown-lenker = kunnskapsgraf, `index.md` for progressiv navigering, `log.md` for kronologi. Format, ikke plattform — ingen SDK, ingen leverandørbinding. |
| [Karpathys LLM-wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) | **Arkitekturen**: tre lag (rå kilder som er uforanderlige → LLM-vedlikeholdt wiki → skjema/konvensjoner som styrer vedlikeholdet) og tre operasjoner (**ingest**, **query**, **lint**). Git som infrastruktur. LLM-en tar bokholderiet mennesker aldri orker — kryssreferanser, motsigelser, foreldede påstander. |
| [yoyo-evolve](https://github.com/yologdev/yoyo-evolve) | **Prosessen**: append-only `learnings.jsonl` som kilde til sannhet, periodisk **syntese** til aktiv kontekst med tidsvektet komprimering (nytt beholdes fullt, gammelt tematiseres), skills som selv kan opprettes/forbedres/pensjoneres, og karantene/nedprioritering av mistenkelig input. |

## Konsept

### To nivåer av kunnskap

1. **Sentral kunnskapsbase** — et **privat, instans-spesifikt repo**
   konfigurert via env (`KB_REPO` + `KB_GH_TOKEN` i proxy-agentens `.env`):
   agentens egen OKF-wiki for overordnet kunnskap på tvers av repoer
   (domenekunnskap, prosesser/playbooks, erfaringer). Eies av bot-kontoen;
   agenten har skrivetilgang og vedlikeholder den selv. Hvilken
   kunnskapsbase en instans bruker er bevisst **usynlig i dette repoet**:
   ulike bot-instanser (i andre organisasjoner) har sine egne private
   kunnskapsrepoer, og kobles kun via config. Tom config = funksjonen er
   inaktiv (samme mønster som `GH_TOKEN`).
2. **Repo-spesifikk kunnskap** — `<repo>/doc`: læringspunkter og
   dokumentasjon som hører til ett bestemt repo, versjonert sammen med koden
   det beskriver. Samme OKF-konvensjoner. (Denne fila er første eksempel.)

Skillelinjen: *gjelder det flere repoer eller domenet generelt → sentral KB;
gjelder det dette repoets kode/oppsett → `doc/`.*

### Anker-foldere: innlinking av eksterne repoer

To gitignorerte anker-foldere på monorepo-rot brukes til å linke inn
eksterne repoer, og bind-mountes inn i agent-containerne:

```text
workspaces_knowledge/                      # klone av kunnskapsrepoet (KB_REPO)
workspaces_repos/<provider>/<org>/<repo>/  # koderepoer det jobbes med
                                           # (provider: github|bitbucket|…)
```

- **Én anker = ett bind-mount** → tilgang kan gis per agent:
  proxy-agenten mountes bare `workspaces_knowledge` (som `/knowledge`),
  den fremtidige kodeagenten får i tillegg `workspaces_repos`
  (som `/repos`). `/workspace` forblir per-agent scratch.
- Klonene ligger på hosten og **overlever container-restarts** (ingen
  re-klon), og et menneske kan inspisere/redigere kunnskapen lokalt med
  vanlig editor — wikien er bare filer i git.
- `<provider>/<org>/<repo>`-hierarkiet er kollisjonsfritt og
  leverandøruavhengig (samme konvensjon som ghq/GOPATH).
- Innholdet forvaltes av containernes entrypoints (klon hvis tom,
  ellers pull) med de respektive tokenene; hosten stiller bare
  katalogene til rådighet.

### Tre lag (Karpathy)

| Lag | Hos oss | Muterbart? |
|---|---|---|
| Rå kilder | `triggers/logs/<id>.log` + `results.jsonl` (trajektorier), eventene selv, `sources/` i KB-repoet (web-snapshots) | Nei — append-only |
| Innboks | `inbox/learnings.jsonl` i KB-repoet: ubehandlede læringskandidater | Append-only, tømmes ved syntese |
| Wiki | OKF-sidene i KB-repoet og `<repo>/doc` | Ja — LLM-vedlikeholdt |
| Skjema | `process/`-sidene i KB + skill-instruksene (hvordan wikien vedlikeholdes) | Ja — menneske-kuratert |

### Læringsloopen

```text
                 ┌───────────────── 1. konsulter ─────────────────┐
                 │                                                │
event ──> proxy-agent ──> utfører oppgave ──> resultat            │
                 │                                                │
                 │ 2. fangst: append læringskandidat              │
                 ▼                                                │
   /knowledge/inbox/learnings.jsonl                               │
                 │ 3. syntese (periodisk, egen skill)             │
                 ▼                                                │
   OKF-wiki: kunnskapsrepoet (KB_REPO) ───────────────────────────┘
   (index.md, domains/, repos/, process/, log.md)
                 │
                 │ 4. repo-spesifikk læring uten kodetilgang:
                 └──> issue m/ label "learning" på <repo>
                        └──> menneske / kodeagent committer til <repo>/doc
```

1. **Konsulter (query)**: før en oppgave leser agenten `index.md` i
   `/knowledge` (lokal klone av KB-repoet) og navigerer lenkene til
   relevante sider. Progressiv navigering — ikke alt i kontekst.
2. **Fangst**: etter en oppgave appender agenten en læringskandidat som én
   JSON-linje til `inbox/learnings.jsonl` — hvis det faktisk er noe å lære
   (ikke pliktløp). Kandidater er *data i karantene*, ikke wiki-innhold.
3. **Syntese (ingest + lint)**: en egen skill kjøres periodisk: leser
   innboksen, integrerer i eksisterende OKF-sider (oppdaterer
   kryssreferanser, flagger motsigelser, tematiserer gammelt — tidsvektet
   komprimering à la yoyo-evolve), oppdaterer `index.md` og `log.md`,
   tømmer innboksen, committer og pusher.
4. **Repo-spesifikke læringer**: proxy-agenten har ikke kodetilgang til
   digdir-repoer (bevisst). Læringer som hører hjemme i et repos `doc/`
   meldes derfor som **issue med label `learning`** via den eksisterende
   github-skillen; et menneske eller den fremtidige kodeagenten committer.

## OKF-konvensjoner

Gjelder både KB-repoet og `<repo>/doc`:

- Én fil = ett konsept; **stien er identiteten**.
- YAML-frontmatter med `type` (påkrevd) + `title`, `description`, `tags`,
  `timestamp`, evt. `resource` (lenke til autoritativ kilde).
- Kryssreferanser som vanlige markdown-lenker → kunnskapsgraf.
- `index.md` per katalognivå (progressiv navigering), `log.md` i rot
  (append-only kronologi, nyeste øverst).

Foreslått struktur for kunnskapsrepoet:

```text
<kb-repo>/
├── index.md            # inngangsport — agenten leser denne først
├── log.md              # kronologi
├── inbox/
│   └── learnings.jsonl # karantene for læringskandidater
├── domains/            # fag-/domenekunnskap (digdir, altinn, …)
├── repos/              # kunnskap om konkrete repoer (én fil per repo)
├── process/            # playbooks: hvordan agenten jobber, inkl.
│                       # syntese-reglene selv (= "skjema"-laget)
└── sources/            # arkiverte web-kilder (rålaget): én fil per URL,
    └── <domene>/<slug>.md   # frontmatter med resource + retrieved
```

Format for en linje i `inbox/learnings.jsonl`:

```json
{"ts":"2026-07-21T12:00:00Z","event_id":"slack-C123-456","source":"slack|github|web","repo":"digdir/x","scope":"global|repo","text":"<hva som ble lært>","confidence":"low|medium|high","source_url":"<valgfri: URL når kilden er en nettside>"}
```

## Sikkerhetsmodell

- **To-token-modell.** Én fine-grained PAT kan ikke ha ulike rettigheter per
  repo, derfor: eksisterende `GH_TOKEN` (Issues + PR-er på digdir-repoer,
  ingen Contents) forblir urørt, og et nytt **`KB_GH_TOKEN`** utstedes fra
  bot-kontoen med Contents Read/Write på **kun** kunnskapsrepoet.
  Verste konsekvens ved kompromittering er forurenset kunnskap — ikke
  kodetilgang.
- **Instans-isolasjon.** `KB_REPO` og `KB_GH_TOKEN` lever kun i den
  gitignorerte `.env`-fila. Dette repoet inneholder ingen referanse til
  noen konkret kunnskapsbase — hver bot-instans peker på sin egen.
- **Forgiftning (poisoning).** Alt som kommer via events er upålitelig
  input, og KB-innhold flyter tilbake inn i fremtidige prompts. Mitigering:
  (a) innboksen er karantene — kandidater blir ikke aktiv kunnskap før
  syntesen; (b) syntese-skillen instrueres eksplisitt om å behandle
  kandidat-tekst som *data, aldri som instruks*; (c) git-historikk + `log.md`
  gir full revisjon; (d) opsjon: la syntesen levere PR i stedet for push til
  main, med menneskelig godkjenning som gate.
- Ingen endring i grunnprinsippet: agentene ser fortsatt bare jsonl-filene,
  `/workspace` og nå `/knowledge` (en klone de eier selv).

## Implementasjonsplan (milepæler)

### M1 — Konvensjoner og `doc/` i dette repoet ✅ (denne PR-en)
`doc/index.md`, `doc/log.md`, denne planen. README-peker til `doc/`.

### M2 — Lesetilgang: agenten konsulterer KB ✅
- Anker-folder `workspaces_knowledge/` på monorepo-rot (gitignorert),
  bind-mountet som `/knowledge` i proxy-agentens compose.
- `entrypoint.sh`: klon kunnskapsrepoet (`KB_REPO`, f.eks. `owner/repo`)
  til `/knowledge` hvis tomt, ellers pull — autentisert med `KB_GH_TOKEN`.
  Er en av dem tom, hoppes det stille over — som `GH_TOKEN`. Må håndtere
  at kunnskapsrepoet kan være **helt tomt** (nyopprettet): klon fungerer,
  og første push etablerer default branch.
- Bootstrap: er `/knowledge` tomt etter klon, legger agenten (eller vi)
  inn OKF-grunnstrukturen (`index.md`, `log.md`, `inbox/`, `domains/`,
  `repos/`, `process/`) som første commit.
- Ny skill **`knowledge-base`**: OKF-konvensjonene, naviger fra
  `/knowledge/index.md`, når KB skal konsulteres, skillet mellom
  global/repo-kunnskap.
- Prompt-prefiks i entrypointet (vi bygger allerede prompten): kort hint om
  å sjekke `/knowledge/index.md` for relevante oppgaver.
- `.env.example`, `docker-compose.yml`, README: dokumenter `KB_REPO` +
  `KB_GH_TOKEN` som bevisst unntak nr. 2 (eksempelverdiene holdes
  generiske — ingen konkret kunnskapsbase nevnes i repoet).

### M3 — Skrivetilgang: fangst av læringer ✅
- Utvid `knowledge-base`-skillen: append kandidat til
  `inbox/learnings.jsonl` + commit/push med bot-identitet etter oppgaver
  med reell læringsverdi.
- Repo-spesifikke læringer: opprett issue med label `learning` på
  målrepoet (gjenbruker `github-issues-prs`-skillen).

### M4 — Syntese og lint ✅
- Ny skill **`knowledge-synthesis`**: les innboks → integrer i OKF-sider →
  oppdater kryssreferanser/`index.md`/`log.md` → flagg motsigelser → tøm
  innboks → commit/push. Trigges først manuelt (`trigger.ps1` /
  Slack-kommando "synthesize").
- Bootstrap av KB-repoets struktur (kan gjøres av agenten selv som første
  syntese-kjøring).

### M5 — Automatikk ✅ (aktiv kontekst utsatt)
- Periodisk syntese kjøres av **proxy-agentens egen watch-loop** (ikke
  integrations): fyrer når innboksen har kandidater og det er minst
  `SYNTHESIS_INTERVAL_HOURS` (default 24, 0 = av) siden sist. Kunnskap er
  agentens egen sak, og syntese trenger ingen Slack-/GitHub-svarrute —
  dette lukker åpent spørsmål 3. Manuell trigging via event fungerer i
  tillegg.
- Tidsvektet komprimering (yoyo-evolve) ligger som regel i
  syntese-skillen: ferske detaljer beholdes, gammelt tematiseres.
- Utsatt: `active/`-side i KB som alltid prependes i prompten (yoyo-style
  "active learnings") — veies mot kontekstkostnad når behovet melder seg.

### M6 — Web-research: selvstendig innlesing fra weben ✅

- Ny skill **`web-research`** med [agent-browser](https://agent-browser.dev/)
  (headless Chromium bakt inn i imaget): `agent-browser read <url>` gir
  agent-lesbar tekst, token-effektivt nok for små modeller.
- **Proveniens via markdown-snapshots**, ikke PDF: det agenten bruker
  arkiveres som `sources/<domene>/<slug>.md` i KB-repoet med frontmatter
  (`type: source`, `resource` = URL, `retrieved` = tidspunkt). Samme URL →
  samme fil, så git-diff viser hva som endret seg på kilden over tid.
  PDF (`agent-browser pdf`) finnes som opsjon på eksplisitt forespørsel.
- Læringskandidater fra weben får `"source":"web"` + `source_url`, og
  syntesen tar kildelenken med inn i wiki-siden — hver påstand sporbar.
- Vakt: webinnhold er data, aldri instruks; kun lesing (aldri innlogging,
  skjemaer eller å oppgi noe til sider); Chromiums interne sandbox er av i
  containeren (Docker-seccomp) — containeren er isolasjonsgrensen, og
  egress-filtrering står som mulig innstramming.

## Åpne spørsmål

1. **KB-tilgang**: hver instans setter `KB_REPO` + `KB_GH_TOKEN`
   (fine-grained PAT fra bot-kontoen, Contents R/W kun på kunnskapsrepoet)
   i sin lokale `.env` før M2 kan verifiseres ende-til-ende.
2. **Review-gate**: skal syntesen pushe rett til main i KB-repoet, eller
   levere PR med menneskelig godkjenning (tryggere mot forgiftning, mer
   friksjon)?
3. **Scheduling** — *løst i M5*: syntesen trigges av proxy-agentens egen
   watch-loop (`SYNTHESIS_INTERVAL_HOURS`); integrations er ikke involvert.
4. **Omfang for `doc/`-flyten**: skal `learning`-issues → `doc/` gjelde alle
   digdir-repoer, eller kun dette repoet inntil kodeagenten finnes?
