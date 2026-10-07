---
type: plan
title: Pilot — jr-agenten over på agentctl/sandbox
description: Port av local-cc-jr-developer fra one-shot-docker/agent-runner.ps1 til agentctl-agent drevet av nvt-bridge. Forberedelse; ingen runtime-endringer før fasene under.
---

# Pilot: jr-agenten på agentctl + sandbox

Oppfølging av ki-lab-beslutningen om å flytte agent-laget fra
`altinn-studio/src/experimental` inn hit som `agentctl/` og `sandbox/`
(se README «Planlagt innflytting»). Jr er valgt som pilot fordi den er den
enkleste kodeagenten: avgrensede lav-risiko-oppgaver, samme filkontrakt som
resten av pipelinen, og minst å tape om piloten skranter.

## Hvorfor (gevinstene, i prioritert rekkefølge)

1. **Mediert GitHub-token.** I dag har jr et klassisk PAT med `repo`-scope
   *inne i containeren* — det formelt fravekne akseptansekriteriet fra #54.
   agentctl-sandboxen får bare en inert placeholder; hosten substituerer
   tokenet kun i trafikk mot `allowedHosts` (github.com m.fl.). Dette lukker
   avviket uten fork-modellen som var notert som M4-hardening.
2. **Mediert harness-auth.** `harnesses: [{type: claudeCode, auth: mediated}]`
   erstatter dagens setup-token-i-llm-gateway-snarvei (PR #102): tokenet bor
   på hosten, aldri i sandboxen. llm-gateway beholdes for pi/router/
   embeddings — jr slutter bare å være konsument der.
3. **Supervisor i stedet for Scheduled Task.** `digdir-runner-jr`-tasken dør
   jevnlig og stille (gjenganger i driftsloggen). agentd/agentctl eier
   livssyklusen; bridgen melder ærlige `status:"error"` ved alt annet.
4. **Verifikasjonskvalitet.** Full sandbox-frihet (egne cluster, nettverk
   etter manifest) lar agenten teste reelt — Martins observasjon fra
   altinn-studio-kjøringene.

## Dagens jr vs. målbildet

| | I dag | Målbilde |
|---|---|---|
| Kjøring | `docker run --rm` per event (`scripts/agent-runner.ps1`, Scheduled Task) | Levende agentctl-Agent; én Session per topic, prompt injiseres per event |
| Supervisor | Scheduled Task (dør stille) | nvt-bridge (poller inbox, driver-abstrahert) + agentd |
| Instrukser | `CLAUDE.md` bakt inn i image | `instructions:`-kilder i `agent.yaml` (samme innhold, flyttet) |
| Modell | sonnet via llm-gateway (OAuth-token i gateway-.env) | sonnet via mediert Claude-auth på host |
| GitHub | klassisk PAT i container-env | mediert secret, inert placeholder i sandbox |
| Filkontrakt | `triggers/inbox.jsonl` → `results.jsonl` | **uendret** — bridgen snakker filkontrakten |

Nøkkelen er at `apps/nvt-bridge` allerede er bygget for dette:
kjernelogikken (dedupe, topic-avledning, serialisering per topic, ærlig
fallback-linje) er testet mot `FakeNvtDriver`, og `src/nvt/driver.ts` sier
eksplisitt at en ny backend er «en ny implementasjon av interfacet, ikke en
endring i kjernen». Porten er altså i hovedsak én ny driver + én
agent-definisjon.

## Faser

**F0 — forberedelse (GJORT, 2026-10):**

- ~~Agent-definisjon~~: `agents/jr-sandbox/` (agent.yaml, Dockerfile på
  examples/minimal-mønsteret, instructions fra jr-CLAUDE.md) — kalibrert
  mot `agentctl/src/manifest.rs` etter at koden flyttet inn.
- ~~Driver~~: `AgentctlDriver` i nvt-bridge bak `NvtDriver`-interfacet,
  kalibrert mot CLI-kilden: `apply --wait` (deklarativ), `create session`
  (ensure + venter til harness-klar), prompt via `--file`, done-deteksjon
  ved å polle `get sessions -o json` (`working` → `waitingForInput`/`idle`,
  dobbel-poll), TTL via `archive`/`unarchive`. M0-onboarding-problemet
  bortfaller: imaget pre-seeder `~/.claude/.claude.json`.
- Gjenstår fra F0: M1 bridge-E2E mot den levende m0-instansen i WSL (#97
  sitt siste akseptansepunkt) — validerer bridge-kjernen uavhengig av
  driver.

**F1 — kalibrering live (neste):**

- Installer agentctl på verten: `agentctl/install.ps1` (Windows krever
  `HypervisorPlatform`-featuren) eller `make user-install`. Første kjøring
  i **WSL**, der m0 allerede har kjørt — Windows-sporet tas etterpå.
- F1-E2E: `agentctl apply --wait` i agents/jr-sandbox → create → prompt →
  resultatlinje i `/triggers/results.jsonl` → unarchive-gjenopptak etter
  TTL. Særlig: bind-mount av triggers/ inn i microsandbox-VM-en på
  Windows/WSL (se «Gjenstår å verifisere live» i agents/jr-sandbox/README).

**F2 — skyggekjøring:**

- Jr-sandbox-agenten får kopi av jr-events (egen triggers-katalog, egen
  bridge-instans) og kjører parallelt med dagens jr. Resultatlinjene
  sammenlignes manuelt; ingen svar postes fra skyggen.

**F3 — cutover:**

- `AGENT_ROUTES` i integrations peker jr-navnet til den bridge-drevne
  katalogen; `digdir-runner-jr`-tasken pensjoneres; jr-PAT-et revokeres
  (sr beholder sitt til sr porteres).

**Bevisst uendret i piloten:** sr (venter på pilot-erfaring), pi, router,
filkontrakten og integrations-koden (kun konfig i F3).

## Avklaringer med Martin/teamet

1. **Står igjen:** kan mediert claudeCode-auth HÅNDHEVE en modell-allowlist
   (policy: aldri fable-nivå for kodeagenter)? Kilden viser at
   `defaults.model` bare er et default — `create --model` overstyrer.
2. **Står igjen:** støtter harness-konfigen custom modell-endepunkt (f.eks.
   vår llm-gateway / LM Studio), for et senere lokal-modell-jr-spor?
3. ~~Windows-vert~~: `agentctl/install.ps1` finnes og README dokumenterer
   `HypervisorPlatform`-kravet; piloten starter likevel i WSL (der m0
   kjørte), Windows-sporet tas etterpå.
4. Ressursdimensjonering: utkastet bruker 2 CPU / 4 GiB / 32 GiB (minimal-
   eksempelet: 4/8/64) — justeres etter F1-erfaring.
5. ~~Headless drift~~: bekreftet i kilden — `get sessions -o json`,
   `create` uten attach, `prompt --file`/`--wait`, exit-koder via clap.

## Risiko / ikke-mål

- **To supervisorer i overgangen** (agent-runner for sr, bridge for jr) er
  villet — sr er fallback om piloten feiler.
- Piloten endrer ikke eventformatet; en rollback er å peke `AGENT_ROUTES`
  tilbake og re-aktivere tasken.
- k8s/cloud-agents, OpenShell og multi-player-orkestrering er utenfor
  scope (parkert i ki-lab-tråden).
