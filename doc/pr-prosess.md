---
type: process
title: PR-prosess — auto-merge på trygge stier, menneske-review på sensitive
description: Plattform-håndhevet skille mellom trygge endringer (reviewer-subagent + auto-merge) og sensitive endringer (CODEOWNERS krever menneskelig godkjenning). Én agent-identitet; merge-gaten er branch protection/CODEOWNERS, ikke token-scoping (se avvik fra opprinnelig akseptansekriterium).
timestamp: 2026-07-22T00:00:00Z
---

# PR-prosess: auto-merge på trygge stier

## Hvorfor

Agent-pipelinen skal kunne utbedre seg selv uten at hvert eneste
dokumentasjons- eller småfiks-PR venter på et menneske — men endringer som
styrer *agentenes egen oppførsel* eller *sikkerhetsmodellen* skal alltid ha
menneskelig godkjenning. Bakgrunn (issue #54): i PR #47 blokkerte
kodeagentens tillatelses-klassifiserer redigering av dens egen instruksfil —
riktig instinkt, men skillet bør håndheves av plattformen, ikke av den
enkelte agentens dømmekraft.

**Viktig premiss:** merge til deploy-branchen er auto-deploy innen minutter
([scripts/self-update.ps1](../scripts/self-update.ps1) med `-WatchSeconds`).
Auto-merge på trygge stier er dermed auto-deploy uten menneske i løkka —
helsesjekk + rollback fanger krasj, men ikke uønsket oppførsel. Listen over
sensitive stier er satt med det i mente.

## Mekanikken — én identitet, fire brikker

1. **[`.github/CODEOWNERS`](../.github/CODEOWNERS)** legger menneskelig eier
   på sensitive stier: agent-instrukser (`agents/*/CLAUDE.md`), skills,
   entrypoints, `integrations/src/`, Docker-filer, `scripts/` og `.github/`
   selv.
2. **Branch protection på deploy-branchen** (settes av repo-admin, se under):
   required approvals **0** + **Require review from Code Owners** + required
   status check (CI). Kombinasjonen gir nøyaktig skillet:
   - PR uten CODEOWNERS-treff → kan merges uten noen godkjenning.
   - PR som rører en sensitiv sti → blokkert til code owner har godkjent.
3. **CI som required check**
   ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml), minimal:
   typecheck i `integrations/`) — «grønn CI før merge» er plattform-håndhevet,
   ikke konvensjon.
4. **Auto-merge via GitHub Action**
   ([`.github/workflows/auto-merge.yml`](../.github/workflows/auto-merge.yml)):
   når en PR får labelen `auto-merge`, slår workflowen på GitHubs native
   auto-merge med den efemere `GITHUB_TOKEN`. Selve mergen skjer alltid med
   dette efemere tokenet, aldri med agent-tokenet direkte — men agent-tokenet
   har i praksis også Contents-scope (se «Avvik fra opprinnelig
   akseptansekriterium» under). Labelen kan uansett ikke omgå branch
   protection: rører PR-en en sensitiv sti, venter mergen på code owner
   uansett, uavhengig av hvilket token som gjør mergen.

## Prosessen for agentene

Før en agent setter `auto-merge`-labelen på sin egen PR skal den:

1. Kjøre en **reviewer-subagent** på diffen (ferske øyne, ikke samme
   kontekst som skrev koden).
2. Poste reviewens funn og konklusjon som **kommentar på PR-en** — det er
   audit-sporet for at review faktisk er gjort.
3. Først da sette labelen: `gh pr edit <nr> --add-label auto-merge`.

Rører PR-en en sensitiv sti er labelen virkningsløs (og bør utelates) —
pek i stedet på PR-en i svaret til brukeren, som før: mennesket er
review-gaten.

## Oppsett (repo-admin, gjøres i GitHub UI/API) — status: aktivt

Dette kan ikke leveres som filer i repoet. Oppsettet er nå aktivt på
**`main`** (utviklingsbranchen; frem til v2-koden tok over `main` var det
speilet på både `v2.0` og `main`, og v1 er arkivert på `v1.0`):

1. **Branch protection / ruleset på `main`:**
   - Require a pull request before merging, **required approvals: 0**
   - **Require review from Code Owners: enabled**
   - Required status checks: **`integrations`** (jobben i CI-workflowen)
2. **Repo-innstilling:** «Allow auto-merge» er slått på (kreves av
   `gh pr merge --auto`).
3. **Label:** `auto-merge` finnes i repoet.
4. **Interaksjonslås:** repoet er satt til `collaborators_only` —
   kun collaborators kan kommentere på eller åpne issues og PR-er. Dette er
   uavhengig av branch protection/CODEOWNERS, men del av samme
   sikkerhetsoppsett: det begrenser hvem som i utgangspunktet kan skape
   innhold agentene reagerer på.

## Avvik fra opprinnelig akseptansekriterium

Det opprinnelige akseptansekriteriet «ingen agent-tokens har fått
Contents-tilgang» er **ikke** oppfylt slik det ble formulert: kodeagentene
bruker i dag ett **klassisk PAT med `repo`-scope** (inkludert Contents), ikke
et fine-grained PAT begrenset til PR/Issues. Årsak: et fine-grained PAT kan
strukturelt ikke nå org-repoer for en outside collaborator (GitHub tillater
ikke fine-grained access til org-repoer for kontoer uten org-medlemskap) —
alternativet var enten organisasjonsmedlemskap for bot-kontoen (større
blast radius) eller klassisk PAT.

Merge-gaten er derfor **branch protection + CODEOWNERS**, ikke
token-scoping: agent-tokenet kan teknisk sett skrive Contents, men kan ikke
omgå required review from Code Owners på sensitive stier, og kan ikke sette
`auto-merge`-labelen forbi branch protection på ikke-sensitive stier heller
(labelen trigger kun workflowen — selve mergen skjer med den efemere
`GITHUB_TOKEN`, se punkt 4 i mekanikken ovenfor). Restrisikoen er dermed at
et kompromittert agent-token kan pushe/force-pushe direkte til andre
branches enn de beskyttede — ikke at det kan omgå review-prosessen på
`main`.

**Fremtidig hardening:** en fork-basert flyt (agenten jobber i en fork,
åpner PR mot upstream) ville fjernet behovet for at agent-tokenet har
Contents-tilgang til hoved-repoet i det hele tatt. Ikke implementert ennå —
notert som neste steg for å faktisk lukke det opprinnelige
akseptansekriteriet.

## Grenser og videre

- `integrations/src/` er sensitiv i sin helhet i første omgang, siden CI kun
  er typecheck; kan snevres til `config.ts` når testdekning finnes.
- Merges gjort av `GITHUB_TOKEN` trigger ikke andre workflows — irrelevant
  her, siden deploy er polling-basert (self-update-watcheren), ikke en
  Action.
- Fork-basert flyt for agent-PR-er (se avviket ovenfor) er fremtidig
  hardening, ikke implementert.
