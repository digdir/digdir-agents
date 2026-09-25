---
type: index
title: Dokumentasjon og læringspunkter — digdir-ai-agents
description: Inngangsport til repo-spesifikk kunnskap. Følger OKF-konvensjonene (markdown + YAML-frontmatter, sti = identitet, kryss-lenker = kunnskapsgraf).
timestamp: 2026-07-21T00:00:00Z
---

# doc/ — repo-spesifikk kunnskap

Denne katalogen er repoets kunnskapsbase etter
[OKF-konvensjonene](plans/kunnskap-og-laering.md#okf-konvensjoner):
én fil per konsept, YAML-frontmatter med minst `type`, vanlige
markdown-lenker som kryssreferanser, og [log.md](log.md) som
kronologisk endringslogg.

## Innhold

- [plans/kunnskap-og-laering.md](plans/kunnskap-og-laering.md) — plan for
  kunnskaps- og læringsprosessen i agent-pipelinen (OKF, kunnskapsrepo,
  læringsloop)
- [plans/agent-delegering.md](plans/agent-delegering.md) — delegering mellom
  agenter via broen, og utførende kodeagent (local-cc-coding-agent)
- [pr-prosess.md](pr-prosess.md) — auto-merge på trygge stier,
  CODEOWNERS/menneske-review på sensitive (issue #54)

## Overordnet kunnskap

Kunnskap som ikke er spesifikk for dette repoet hører hjemme i agentens
sentrale kunnskapsbase — et privat repo som konfigureres per bot-instans
via `KB_REPO`/`KB_GH_TOKEN` i proxy-agentens `.env` (se
[planen](plans/kunnskap-og-laering.md)).
