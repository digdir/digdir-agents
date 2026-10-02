# jr-sandbox — jr-agenten på agentctl/sandbox (UTKAST)

**Status: F0-utkast — ikke i drift.** Dette er agent-definisjonen for
piloten i [doc/plans/jr-paa-agentctl.md](../../doc/plans/jr-paa-agentctl.md):
jr som agentctl-Agent (varig sandbox, én Session per topic), drevet av
[apps/nvt-bridge](../../apps/nvt-bridge) med `NVT_BRIDGE_DRIVER=agentctl`.
Dagens jr (`agents/local-cc-jr-developer/` + `scripts/agent-runner.ps1`)
kjører uberørt videre til F3-cutover.

Filene er skrevet mot skjemaet og konvensjonene slik de ser ut i
altinn-studio (`agents/minimal`, `agents/Dockerfile`, `HARNESSES.md`) — før
`agentctl/` og `sandbox/` er flyttet inn i dette repoet.

## Slik er det tenkt å henge sammen

```
integrations ──append──> agents/jr-sandbox/triggers/inbox.jsonl
nvt-bridge (driver: agentctl) ──poll──┘
  │  agentctl apply --wait                (én Agent: jr-sandbox)
  │  agentctl create session/<topic>      (én Session per topic)
  │  agentctl prompt session/<topic> …    (event-prompt m/ resultatkontrakt)
  └─ poller session-state til turen er ferdig
agenten ──append──> /triggers/results.jsonl  (bind-mount av triggers/)
integrations ──poll──> results.jsonl → svar til Slack/GitHub
```

## Kalibreringspunkter (F1 — når agentctl/sandbox er flyttet inn)

Alt under er antakelser som må verifiseres mot ekte `agentctl`; de bor i
`apps/nvt-bridge/src/nvt/agentctl.ts` (driveren) og her:

1. **CLI-flatene**: `apply --wait`-idempotens, `create session/<navn>` uten
   attach, promptlevering (posisjonsargument vs. stdin), `get sessions -o
   json`-feltnavn (navn/state), `archive session/<navn>`, `--agent`-flagget.
2. **Session-state-verdiene**: driveren antar `Working` under arbeid og noe
   annet (`WaitingForInput`/`Idle`) når turen er ferdig.
3. **Imaget**: bruker (`agent`), systemd-init, tmux og harness-pin må matche
   det agentd forventer (modellert på altinn-studios `agents/Dockerfile`).
4. **Bind-mount av `./triggers`** fra host inn i microsandbox-VM-en, med
   skriverettigheter for sandbox-brukeren.
5. **Modell-allowlist**: kan mediert claudeCode-auth håndheve «aldri
   fable-nivå», eller er `defaults.model: sonnet` bare et default?
6. **Prompt-dialekten**: broen bruker `dialect: "agentctl"` (ingen
   `agentdctl signal done`); ferdig-deteksjon er session-state, og
   resultatlinja er fortsatt eneste suksessbevis.

## Oppsett (F1, på verten)

```sh
agentctl claude login                      # mediert harness-auth
cp .env.sample .env                        # git-identitet + GITHUB_TOKEN
agentctl apply --wait                      # fra denne katalogen
```

Bridge-konfig (i apps/nvt-bridge/.env): `NVT_BRIDGE_DRIVER=agentctl`,
`NVT_BRIDGE_TRIGGERS_DIR=../../agents/jr-sandbox/triggers`,
`AGENTCTL_AGENT_DIR=../../agents/jr-sandbox`, `NVT_INSTANCE_PREFIX=jr`.

GitHub-tokenet skal være et **fine-grained PAT** avgrenset til repoene jr
jobber i (Contents + Pull requests R/W) — ikke dagens klassiske PAT.
