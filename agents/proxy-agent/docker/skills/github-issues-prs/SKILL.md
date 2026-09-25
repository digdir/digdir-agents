---
name: github-issues-prs
description: Opprett, kommenter og administrer GitHub-issues og pull requests med gh CLI. Bruk denne når henvendelsen gjelder å lage et issue, svare/kommentere på et issue eller en PR, endre labels/tilordning, lukke/gjenåpne, eller hente status og innhold fra issues/PR-er.
---

# GitHub: issues og pull requests

Du har `gh` (GitHub CLI) tilgjengelig, ferdig autentisert via miljøvariabelen
`GH_TOKEN`. Tokenet er med vilje snevert: det gir kun tilgang til **issues og
pull requests** (pluss metadata) på utvalgte repoer — ikke kode.

## Grunnregler

- Oppgi alltid repo eksplisitt med `--repo <owner>/<repo>` — `/workspace` er
  ikke nødvendigvis en git-checkout av repoet det gjelder.
- Repo og issue-/PR-nummer står som regel i trigger-eventets `payload`
  (f.eks. `payload.repo`, `payload.issue`). Bruk det; ikke gjett.
- Skriv kommentarer/issue-tekst på norsk med mindre tråden går på engelsk.
- Kommentarer skal kun påstå det som faktisk er gjort og verifisert i denne
  kjøringen. Post aldri «jeg har fikset/implementert …» — du kan ikke endre
  kode; en fiks finnes først når det finnes en PR med endringene, og da
  lenker du til den. Falske fullført-meldinger er verre enn stillhet: de
  lukker oppgaver som ikke er løst.
- Feiler `gh` med auth-feil, er `GH_TOKEN` ikke satt eller mangler tilgang til
  repoet — si det i svaret ditt i stedet for å prøve å omgå det.

## Vanlige operasjoner

```bash
# Issues
gh issue list    --repo OWNER/REPO --state open
gh issue view    42 --repo OWNER/REPO --comments
gh issue create  --repo OWNER/REPO --title "..." --body "..."
gh issue comment 42 --repo OWNER/REPO --body "..."
gh issue edit    42 --repo OWNER/REPO --add-label bug --add-assignee USER
gh issue close   42 --repo OWNER/REPO --comment "..."

# Pull requests
gh pr list    --repo OWNER/REPO
gh pr view    17 --repo OWNER/REPO --comments
gh pr diff    17 --repo OWNER/REPO           # les endringene (read-only)
gh pr comment 17 --repo OWNER/REPO --body "..."
gh pr review  17 --repo OWNER/REPO --comment --body "..."

# Alt annet (reaksjoner, review-tråder, søk): gh api
gh api repos/OWNER/REPO/issues/42/reactions -f content=+1
```

Bruk `--body-file` med en midlertidig fil for lengre tekster, så unngår du
quoting-trøbbel.

## Avgrensning: ingen koding

Selve kodearbeidet (branches, commits, push, merge) er en **annen agents**
ansvar. Du skal derfor aldri:

- merge, lukke eller gjenåpne PR-er uten at det er eksplisitt bedt om
- godkjenne PR-er (`gh pr review --approve`)
- forsøke å endre kode eller pushe (tokenet tillater det uansett ikke)

Blir du bedt om å fikse kode, kommenter heller på issuet/PR-en at oppgaven er
notert og sendes videre til kodeagenten, og gjenspeil det i `reply`-feltet.
