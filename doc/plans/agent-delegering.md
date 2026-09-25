---
type: plan
title: Delegering mellom agenter og utførende kodeagent
description: Valgfri, konfigurerbar delegering av oppgaver mellom agenter via broen (integrations), med en interaktiv Claude Code-sesjon som første utførende kodeagent.
timestamp: 2026-07-21T00:00:00Z
---

# Delegering mellom agenter

## Mål

Pipelinen skal kunne **utbedre seg selv autonomt**: proxy-agenten (analytikeren)
oppdager og beskriver, en utførende kodeagent implementerer — og mennesket er
review-gaten. Nøkkelinnsikten: filkontrakten (`triggers/inbox.jsonl` inn,
`results.jsonl` + `logs/<id>.log` ut) er allerede agent-grensesnittet. Alt som
kan lese og skrive jsonl er en agent — en container, en interaktiv Claude
Code-sesjon på hosten, eller noe tredje senere.

## Konsept: nav-og-eiker

Agentene skriver **aldri** i hverandres kataloger. All ruting går gjennom
broen (integrations), som allerede eier alle svar-ruter:

```text
Slack/GitHub ──> integrations ──> proxy-agent (analytiker)
                     │                  │ resultat: intent "delegate"
                     │                  │ {agent, prompt, payload}
                     │◄─────────────────┘
                     │ ruting (allowlist: AGENT_ROUTES, hoppgrense)
                     ▼
     agents/local-cc-coding-agent/triggers/inbox.jsonl
                     │
        ┌────────────┴─────────────┐
        │ i dag: Claude Code       │ senere: container med
        │ interaktivt (CLAUDE.md)  │ kodeagent (samme kontrakt)
        └────────────┬─────────────┘
                     ▼ results.jsonl ──> integrations ──> svar i opprinnelig tråd/issue
```

- **Delegering er et resultat**: `intent: "delegate"` + `delegate`-objekt
  (`agent`, `prompt` — komplett og selvstendig, `payload` — f.eks. issue-URL).
- **Broen remapper svar-konteksten**: målagentens endelige svar postes i den
  opprinnelige Slack-tråden / GitHub-issuet; opphavet får en interim-melding
  og beholder «working»-reaksjonen til det endelige svaret kommer.
- **Valgfritt og konfigurerbart**: mål må være allowlistet i `AGENT_ROUTES`
  (integrations) og annonsert i `DELEGATE_AGENTS` (proxy-agenten). Tomt = av.
- **Hoppgrense** (`AGENT_MAX_DELEGATION_HOPS`, default 2) hindrer at agenter
  kaster oppgaver frem og tilbake i evig loop.

## Arbeidsdeling og tilgang

| | proxy-agent (analytiker) | kodeagent (utførende) |
|---|---|---|
| `workspaces_repos/` | `/repos` **read-only** (lese dokumentasjon/kode for analyse) | skrivbar (jobber i klonene) |
| Leveranse | løsningsbeskrivelse, issue, delegering | branch + PR — aldri push til main |
| GitHub-tilgang | `GH_TOKEN` (kun issues/PR-er) | egne rettigheter (lokal CC: brukerens; container: eget snevert token) |

GitHub-issuet forblir kontrakten for *innholdet*: delegerings-eventet er tynt
(peker + kort prompt), spesifikasjonen er menneskelesbar i issuet, og
resultatet er alltid en PR et menneske reviewer.

## Milepæler

### M1 — Delegering i kontrakten og ruting i broen ✅
- integrations: `AGENT_ROUTES` (navn slås opp som `<AGENT_AGENTS_DIR>/<navn>/triggers`),
  resultat-watcher for alle konfigurerte agenter (offset per agent),
  `handleDelegation` med remapping av svar-kontekst, interim-melding,
  avvisning av ukjente mål og hoppgrense. Verifisert med kontraktstest
  (happy path, ukjent mål, hoppgrense).
- proxy-agent: `DELEGATE_AGENTS` gjør «delegate» tilgjengelig i
  klassifiseringen; entrypointet sender `delegate`-objektet videre til
  resultatlinja. `/repos` (anker-folder `workspaces_repos/`) mountet read-only.

### M2 — local-cc-coding-agent (interaktiv Claude Code) ✅
- `agents/local-cc-coding-agent/` med `CLAUDE.md` som operativ instruks:
  poll innboksen, utfør i `workspaces_repos/<provider>/<org>/<repo>`,
  alltid egen branch + PR, append resultatlinje + logg. Starter med
  `cd agents/local-cc-coding-agent && claude` — ingen wrapper nødvendig.

### M3 — Løsningsforslag → delegering, ende til ende ✅
- Skill i proxy-agenten (`solution-proposal`) som strukturerer analysen som
  GitHub-issue (bakgrunn, foreslått løsning, steg, berørte filer,
  akseptansekriterier) og delegerer utførelsen med issue-URL i payload.
- Verifiseres autonomt: Slack-melding → analyse → issue → delegering →
  kodeagenten lager PR → svar i tråden.

### Læringsløkke for delegering (issue #43) ✅
- Broen sender et `delegation-outcome`-event til opphavsagenten når det
  delegerte svaret er levert (`AGENT_DELEGATION_DEBRIEF`, default på).
  Debrief-eventet har ingen pending svar-rute (resultatet konsumeres stille)
  og teller ikke som delegeringshopp — kan aldri starte en loop.
- Proxy-agenten reflekterer ved debrief (knowledge-base-skillen) og skriver
  0–2 prosess-læringer (`scope: "process"`, `source: "agent"`) til
  kunnskapsrepoets innboks; kodeagenten har et retro-steg som avleverer
  tilsvarende læringer før resultatlinja skrives.

### Junior-kodeagent: local-cc-jr-developer (issue #53) ✅
- `agents/local-cc-jr-developer/`: samme runtime (Claude Code CLI,
  interaktivt) og samme filkontrakt som local-cc-coding-agent, men mot en
  lokal kodemodell via LM Studios Anthropic-kompatible `/v1/messages`-API —
  bare env-variabler (`ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`/
  `ANTHROPIC_MODEL`), ingen proxy. Oppstart er én kommando:
  `scripts/junior-agent.ps1`.
- Instruksen krever eksplisitt «meld tilbake i stedet for å gjette» ved
  uklare eller for store oppgaver, og solution-proposal-skillen fikk
  valgkriterier: godt definert/avgrenset/lav risiko → junior; komplekst/
  uklart/arkitektur/sikkerhet → senior (local-cc-coding-agent).
- Mellomsteg på veien mot M4; M4 står ved lag som fremtidig mål.

### M4 — Containerisert kodeagent
- `agents/<code-agent>/` i Docker med skrivbar `workspaces_repos`-mount og
  eget fine-grained token (Contents R/W på avgrensede repoer — tredje
  bevisste unntak). Erstatter/utfyller local-cc uten endringer i kontrakten.

## Sikkerhet

- Agentene deler aldri kataloger; broen er eneste ruter og har allerede alle
  tokens den trenger.
- Delegerte events merkes `source: "agent"` med `payload.origin`-kjede —
  sporbart hvem som ba om hva.
- Kodeagentens leveranse er alltid branch + PR; main er beskyttet av
  menneskelig review. Lokal CC kjører med brukerens rettigheter og er
  «trusted dev-mode»; containervarianten får eget, snevert token.
- Selvforbedrings-scope starter med dette repoet; utvidelse til andre repoer
  er et bevisst konfigvalg senere.
