# local-cc-jr-developer — junior utførende kodeagent (container)

Junior-utgaven av den utførende kodeagenten: Claude Code CLI på en rimelig
modell (Sonnet/Haiku via llm-gatewayen med subscription-OAuth, eller en
lokal modell via LM Studio — se `.env.example`). Samme filkontrakt som de
andre agentene (`triggers/inbox.jsonl` inn, `triggers/results.jsonl` +
`triggers/logs/<id>.log` ut) og samme container-arkitektur som
senior-utgaven [`local-cc-coding-agent`](../local-cc-coding-agent/):
hvert event behandles av en **engangs-container** med eget workspace og
egen Claude-sesjon per topic.

## Arkitektur

- **Runner på hosten** (`scripts/agent-runner.ps1 -AgentName
  local-cc-jr-developer`): dum supervisor uten LLM.
  Poller `triggers/inbox.jsonl`, finner ubehandlede events (id uten linje i
  `results.jsonl`), grupperer på topic (opphavs-tråden/issuet:
  `payload.origin.event_id` uten delta-suffiks) og kjører `docker run --rm`
  per event — seriellt innen et topic, parallelt på tvers (maks N).
  Docker-tilgangen ligger hos runneren; containeren har aldri Docker-socket.
- **Container** (`docker/`): node-slim + Claude Code CLI + git + gh +
  ripgrep, non-root, `cap_drop: ALL`, `no-new-privileges`. Entrypointet
  behandler ÉN oppgave: leser eventet, kjører `claude -p` headless
  (stream-json) og skriver resultatlinje + logg til `/triggers`.
  `--dangerously-skip-permissions` er akseptabelt her — containeren er
  isolasjonsgrensen (samme argument som Chromium-sandboxen i proxy-imaget).
- **Workspace per topic:** hvert topic får `workspaces/<topic>/`
  (gitignorert) mountet som `/workspace`; agenten kloner arbeidsrepoet dit
  selv med sitt eget token. Monorepoet, deploy-klonen og felles
  `workspaces_repos/` mountes aldri inn — agenten kan strukturelt ikke røre
  dem.
- **Samtalekontinuitet per topic:** Claude Code-sesjonsstate ligger i
  topic-workspacet (`CLAUDE_CONFIG_DIR`). Første event i et topic starter ny
  sesjon; oppfølgingsevents kjører `claude -p --resume <session-id>`
  (session-id fra forrige kjørings output). Nye topics starter ferskt.
- **Egen tilgang, samme identitet:** agenten bruker pipelinens bot-konto med
  et **eget fine-grained PAT** kun for denne agenten (Contents RW + Pull
  requests RW, begrenset til arbeidsrepoene) — aldri operatørens PAT, aldri
  gjenbruk av proxyens tokens. Branch protection gjelder boten fullt ut, og
  bot-kontoen skal **ikke** stå i `GITHUB_ALLOWED_USERS`. Kontonavn og token
  ligger kun i `.env` (gitignorert).

## Oppstart

```powershell
Copy-Item agents\local-cc-jr-developer\.env.example agents\local-cc-jr-developer\.env
# fyll inn GH_TOKEN (agentens eget PAT) og git-identitet i .env

.\scripts\agent-runner.ps1 -AgentName local-cc-jr-developer
# bygger imaget ved behov og poller innboksen; kjør headless via
# scripts\install-agent-tasks.ps1 (Scheduled Task) i stedet for en konsoll
```

Instruksene agenten kjører med ligger i [CLAUDE.md](CLAUDE.md) — de bakes
inn i imaget og lastes i workspacet ved hver kjøring (endrer du CLAUDE.md,
bygg imaget på nytt: `docker build -t local-cc-jr-developer:latest -f
docker/Dockerfile .` fra denne katalogen). Viktigste forskjell fra
senior-agenten: junior-agenten skal ta **godt definerte, avgrensede,
lav-risiko** oppgaver — og er eksplisitt instruert til å melde tilbake i
stedet for å gjette når en oppgave er uklar eller større enn antatt.

## Krav

- Docker Desktop på hosten (runneren orkestrerer containerne).
- LLM-backend per `.env`: llm-gatewayen på `127.0.0.1:8787` (variant A,
  default) — eller LM Studio på `127.0.0.1:1234` med modellen lastet og
  **romslig kontekstvindu** (variant B; Claude Code er kontekst-tungt, så
  minst 25k tokens, helst mer).

Oppgaver delegeres hit av andre agenter (proxy-agenten med
`DELEGATE_AGENTS`) via broen — integrations må ha `local-cc-jr-developer` i
`AGENT_ROUTES`. Svar postes automatisk tilbake i den opprinnelige
Slack-tråden / GitHub-issuet. Leveransen er alltid branch + PR med et
menneske som review-gate.
