# jr-sandbox — jr-agenten på agentctl/sandbox

**Status: kalibrert mot kilden, venter på F1-E2E.** Dette er
agent-definisjonen for piloten i
[doc/plans/jr-paa-agentctl.md](../../doc/plans/jr-paa-agentctl.md): jr som
agentctl-Agent (varig sandbox, én Session per topic), drevet av
[apps/nvt-bridge](../../apps/nvt-bridge) med `NVT_BRIDGE_DRIVER=agentctl`.
Dagens jr (`agents/local-cc-jr-developer/` + `scripts/agent-runner.ps1`)
kjører uberørt videre til F3-cutover.

Filene er kalibrert mot koden i dette repoet (`agentctl/` @
v0.1.0-preview.10): manifest-skjemaet i `agentctl/src/manifest.rs`,
CLI-flatene i `agentctl/src/bin/agentctl/main.rs`, session-statene i
`agentctl/src/sessions/mod.rs`, og image-mønsteret i
`agentctl/examples/minimal/`.

## Slik henger det sammen

```
integrations ──append──> agents/jr-sandbox/triggers/inbox.jsonl
nvt-bridge (driver: agentctl) ──poll──┘
  │  agentctl apply --wait                  (én Agent: jr-sandbox)
  │  agentctl create session/<topic> …      (ensure + venter til harness-klar)
  │  agentctl prompt session/<topic> --file (event-prompt m/ resultatkontrakt)
  └─ poller `get sessions -o json` til staten roer seg (working → waitingForInput)
agenten ──append──> /triggers/results.jsonl  (bind-mount av triggers/)
integrations ──poll──> results.jsonl → svar til Slack/GitHub
```

TTL: broen `archive`-r stille topics; neste event `unarchive`-r og
gjenopptar samtalen. Pre-seedet `claude-state.json` (mønster fra
examples/minimal) fjerner onboarding-/trust-dialogene som spiste prompts i
M0 — docker-driverens tmux-Enter-heuristikk trengs ikke her.

## Verifisert mot kilden (tidligere kalibreringspunkter)

- `create` er ensure-semantikk og venter til harnesset er klart; `apply` er
  deklarativ og idempotent. `--agent` tar agent-navnet.
- Prompt leveres med `--prompt`, `--file` eller stdin — driveren bruker
  `--file` (lang, upålitelig tekst; unngår argumentgrenser).
- `get sessions -o json` gir Session-objekter med `name` og `status.state`;
  statene er `starting`/`working`/`waitingForInput`/`idle`/`archiving`/
  `archived`/`failed` (camelCase, liten forbokstav).
- Manifestet krever `home:`; bind-mounts bruker `readOnly`; secrets bruker
  `environment`/`placeholder`/`allowedHosts`; `GIT_USER_NAME` og
  `GIT_USER_EMAIL` må deklareres sammen. Skjemaet er `deny_unknown_fields`.
- Image-fasit: tmux + `agent`-bruker (passordløs sudo) + pinnet Claude Code;
  intet systemd/ssh for en headless agent.

## Gjenstår å verifisere live (F1-E2E)

1. Bind-mount av `./triggers` fra host inn i microsandbox-VM-en — spesielt
   på Windows/WSL — med skriverettigheter for sandbox-brukeren.
2. Modell-politikk: `defaults.model: sonnet` er et *default*, ikke en
   allowlist (`--model` ved create overstyrer). Håndhevet «aldri
   fable-nivå» må i så fall avtales med innflytterteamet (spørsmål i
   plan-dokumentet står).
3. Første ende-til-ende-kjøring: apply → create → prompt → resultatlinje,
   og unarchive-gjenopptak etter TTL.

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
