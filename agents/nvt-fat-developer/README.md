# nvt-fat-developer — utførende kodeagent i nvt-instans («fat-dev»)

Neste generasjons utførende kodeagent: i stedet for en engangs-container per
event kjører agenten i et **isolert nvt-agentmiljø per topic**, med en levende
CLI-sesjon i tmux, code-server (VS Code i nettleser) å hoppe inn i, og
git-token utstedt av nvt-brokeren. Samme filkontrakt som de andre agentene
(`triggers/inbox.jsonl` inn, `triggers/results.jsonl` +
`triggers/logs/<id>.log` ut), så integrations trenger bare én ny rute i
`AGENT_ROUTES`.

Design og premisser: [`doc/plans/nvt-agent-integrasjon.md`](../../doc/plans/nvt-agent-integrasjon.md).
Sporet er issue #95; denne katalogen og broen er M1 (#97).

> **Status: M1-kjerne kalibrert mot M0-funnene, ikke tatt i bruk ennå.** Broen
> kjører nå `agent-init --user non-root`, venter på at CLI-sesjonen er klar før
> første prompt, validerer at stiene kan traverseres av uid 1000, og genererer
> instansens `agent.yaml` med eksplisitt commit-identitet. Ende-til-ende mot en
> ekte instans er fortsatt ikke kjørt — se
> [`apps/nvt-bridge/README.md`](../../apps/nvt-bridge/README.md) § «Kalibrert
> mot M0-funnene», særlig volum-hygienen ved bytte fra root til non-root. Ruta
> i `AGENT_ROUTES` er driftskonfig og settes ved utrulling, ikke her.

## Arkitektur

- **Broen på hosten** ([`apps/nvt-bridge/`](../../apps/nvt-bridge/)): dum
  supervisor uten LLM. Poller `triggers/inbox.jsonl`, finner ubehandlede
  events (id uten linje i `results.jsonl`), grupperer på topic
  (`payload.origin.event_id` uten delta-suffiks) og mapper topic → nvt-instans
  i `state/topics.json`. Serielt innen et topic, parallelt på tvers (maks N).
  Broen eier Docker-tilgangen mot hostens daemon; **agentcontaineren får den
  aldri** (nvt-instansen har sin egen, isolerte dind).
- **Instans per topic:** første event i et topic gir `agent-init` +
  `agent-up`; oppfølgingsevents injiseres i den **samme levende sesjonen**
  (`agentdctl prompt --source host --external`) — oppfølging er altså samme
  samtale, ikke en `--resume`-rekonstruksjon. Inaktive topics tas ned med
  `agent-down` etter TTL; workspacet beholdes, så instansen kan gjenskapes.
- **Agenten skriver resultatlinja selv.** `triggers/` mountes inn i instansen
  som `/triggers`. Etter resultatlinja sender agenten `agentdctl signal done`.
  Kommer signalet uten resultatlinje innen fristen, skriver broen en
  `status:"error"`-linje med forklaring — **aldri** en fabrikert suksess.
- **Samtalen går via orkestratoren:** agenten poster aldri selv i
  Slack/GitHub. Svar er resultatlinjer; integrations poster dem i
  opphavstråden. Git og PR-er gjør agenten selv.
- **Egen tilgang, samme identitet:** pipelinens bot-konto med et **eget**
  fine-grained PAT for denne agenten, lagt i nvt-brokeren som
  `static_token`-provider — ikke i agentens `.env`. Da får vi brokerens
  grant-innsnevring per instans og audit-loggen, og instansen kan senere
  flyttes til mediated mode uten å endres. Tokenverdien bor i `.broker/env` på
  hosten. Bot-kontoen skal **ikke** stå i `GITHUB_ALLOWED_USERS`.
- **Mennesket kan hoppe inn** i samme miljø via code-server
  (`http://<instans>.agent.localhost:4090`) eller `tmux attach -t agent` mens
  sesjonen lever. Kontrollert veksling mellom headless og hands-on er
  driver-leasen i M3 (#99) — i M1 er det ingen håndheving, så samtidig
  innhopp og headless-kjøring i samme topic kan kollidere i arbeidskopien.

## Filer

| Fil | Rolle |
| --- | --- |
| `AGENTS.local.md.tmpl` | Instruks-malen som rendres inn i instansens `AGENTS.local.md`. Agentens protokoll. |
| `.env.example` | Instans-config (broker-provider, LLM-backend). Ingen tokens — de bor i `.broker/`. Commit-identiteten settes i broens `.env`. |
| `triggers/` | Filkontrakten. Gitignorert bortsett fra `.gitkeep`. |

### Instruks-malen

`AGENTS.local.md.tmpl` er en tilpasset utgave av
[`local-cc-coding-agent/CLAUDE.md`](../local-cc-coding-agent/CLAUDE.md) —
«kjenn din begrensning», branch+PR-kontrakten (`agent/<navn>`,
`gh pr create --base`, `Closes #<nr>`, aldri merge), retro/KB-steget og
sikkerhetsreglene er gjenbrukt ordrett der de kan. To ting skiller:

1. **Ingen innboks-polling** — oppgaver kommer som prompts i den levende
   sesjonen, én om gangen.
2. **`agentdctl signal done` etter resultatlinja** — signalet er for
   bro/konsoll (status, lease), resultatlinja er kontrakten mot integrations.

Plassholderne (`{{AGENT_NAME}}`, `{{TOPIC}}`, `{{INSTANCE}}`,
`{{TRIGGERS_DIR}}`, `{{WORKSPACE_DIR}}`, `{{KNOWLEDGE_DIR}}`) er dokumentert i
kommentaren øverst i malen.

> Rendringen inn i instansen er **ikke wiret opp ennå**: den hører til
> `agent-init`-oppsettet, som kalibreres mot M0-funnene. Malen er med her slik
> at teksten kan reviewes og versjoneres nå.

## Oppstart

```powershell
# 1) nvt-oppsettet på WSL2 (M0, issue #96): broker med fat-dev-provider,
#    grant mot arbeidsrepoene, gateway-konsument i routes.json.

# 2) Broen
Copy-Item apps\nvt-bridge\.env.example apps\nvt-bridge\.env
# fyll inn NVT_ROOT (en sti uid 1000 kan traversere, f.eks. /srv/nvt-agent),
# NVT_GIT_IDENTITY_NAME/EMAIL og NVT_BROKER_PROVIDER — broen nekter å starte
# uten. Se apps/nvt-bridge/README.md
cd apps\nvt-bridge; npm start

# 3) Ruta (driftskonfig, i deploy-klonens .env — ikke i dette repoet)
#    AGENT_ROUTES=...,nvt-fat-developer
```

## Krav

- Docker Desktop + nvt-sjekkout på WSL2-siden (Make/bash-flyten er
  Linux-first, filene på Linux-filsystemet for ytelse).
- llm-gatewayen på hosten (`127.0.0.1:8787`) med en egen konsument for
  fat-dev. Modell-allowlisten håndheves der, fail closed.
- Node ≥ 22.6 for broen.

Oppgaver delegeres hit av andre agenter (proxy-agenten med
`DELEGATE_AGENTS`) via broen — integrations må ha `nvt-fat-developer` i
`AGENT_ROUTES`. Svar postes automatisk tilbake i den opprinnelige
Slack-tråden / GitHub-issuet. Leveransen er alltid branch + PR med et
menneske som review-gate.
