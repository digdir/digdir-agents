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

**F0 — forberedelse (kan gjøres nå, uavhengig av Martins innflytting):**

- M1 bridge-E2E mot den levende m0-instansen i WSL (#97 sitt siste
  akseptansepunkt). Det validerer bridge-kjernen porten skal stå på.
- Skriv utkast til `agents/local-cc-jr-developer/agent.yaml` (uten å ta den
  i bruk): instructions fra dagens CLAUDE.md, `harnesses: claudeCode/
  mediated/defaults.model: sonnet`, `secrets: GITHUB_TOKEN` med
  `allowedHosts: [github.com, api.github.com, uploads.github.com]`,
  `network: {mode: mediated}`.
- Avklaringsliste til Martin (under).

**F1 — når `agentctl/` + `sandbox/` er inne:**

- Installer agentctl på verten (install-skript finnes; Windows krever
  `HypervisorPlatform`-featuren). Første kjøring i **WSL**, der m0 allerede
  har kjørt — Windows-sporet tas etterpå.
- `AgentctlDriver` i nvt-bridge (implementerer `NvtDriver`-interfacet:
  ensureInstance → `agentctl apply`/`create session`, sendPrompt →
  prompt-injeksjon, waitDone → session-status). Gjenbruk klar-sjekk-
  erfaringene fra M0 (onboarding-dialoger, markørfiler).

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

1. Kan mediert claudeCode-auth begrense modellvalg (policy: aldri
   fable-nivå for kodeagenter — holder `defaults.model: sonnet`, eller
   finnes en allowlist i medieringen)?
2. Støtter harness-konfigen custom modell-endepunkt (f.eks. vår llm-gateway
   / LM Studio), for et senere lokal-modell-jr-spor?
3. Status for Windows-vert (libkrunfw-forken har Windows-støtte; er
   agentctl-på-Windows kjørt i praksis, eller bør piloten bli i WSL)?
4. Ressursdimensjonering for en liten kodeagent (minimal-varianten bruker
   4 CPU / 8 GiB / 64 GiB — jr trenger trolig mindre).
5. Headless drift: `agentctl apply`/sessions uten TUI — noe bridgen må vite
   om exit-koder/JSON-output (`-o json` finnes på deler av CLI-et).

## Risiko / ikke-mål

- **To supervisorer i overgangen** (agent-runner for sr, bridge for jr) er
  villet — sr er fallback om piloten feiler.
- Piloten endrer ikke eventformatet; en rollback er å peke `AGENT_ROUTES`
  tilbake og re-aktivere tasken.
- k8s/cloud-agents, OpenShell og multi-player-orkestrering er utenfor
  scope (parkert i ki-lab-tråden).
