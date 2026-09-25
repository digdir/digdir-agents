# local-cc-coding-agent — senior utførende kodeagent (container)

Senior-utgaven av den utførende kodeagenten: Claude Code CLI på pipelinens
mest kapable modell (Opus via llm-gatewayen med subscription-OAuth — tokenet
er aldri inne i containeren). Samme filkontrakt som de andre agentene
(`triggers/inbox.jsonl` inn, `triggers/results.jsonl` +
`triggers/logs/<id>.log` ut), og samme container-arkitektur som
[`local-cc-jr-developer`](../local-cc-jr-developer/): hvert event behandles
av en **engangs-container** med eget workspace og egen Claude-sesjon per
topic, orkestrert av runneren på hosten (`scripts/agent-runner.ps1`).
Arbeidsfordelingen styres av proxy-agentens `DELEGATE_AGENTS`-beskrivelser:
senior tar **komplekse, uklare eller arkitektur-/sikkerhetstunge** oppgaver;
junior tar godt definerte, avgrensede, lav-risiko oppgaver.

## Oppstart

```powershell
Copy-Item agents\local-cc-coding-agent\.env.example agents\local-cc-coding-agent\.env
# fyll inn GH_TOKEN (agentens EGET PAT fra bot-kontoen — aldri jr-ens) og git-identitet

.\scripts\agent-runner.ps1 -AgentName local-cc-coding-agent
```

Runneren bygger imaget ved behov og poller innboksen. Instruksene agenten
kjører med ligger i [CLAUDE.md](CLAUDE.md) — de bakes inn i imaget (endrer
du dem, bygg på nytt: `docker build -t local-cc-coding-agent:latest -f
docker/Dockerfile .` fra denne katalogen).

Identitet og isolasjon er som for junior-agenten (se dens README for
detaljene): eget fine-grained PAT fra bot-kontoen kun for denne agenten,
workspace per topic under `workspaces/` (gitignorert), aldri mount av
monorepo/deploy-klone/`workspaces_repos/`, `cap_drop: ALL`,
`no-new-privileges`, ingen Docker-socket. Leveransen er alltid branch + PR
med et menneske som review-gate.

> Historikk: tidligere kjørte denne agenten interaktivt på hosten
> (operatørens Claude Code og gh-identitet, delte kloner i
> `workspaces_repos/`). Container-utgaven erstatter det oppsettet;
> interaktiv kjøring er fortsatt mulig manuelt, men da med operatørens
> identitet — bruk det kun til feilsøking.
