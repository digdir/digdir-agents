# llm-gateway — ett lokalt endepunkt for alle modellkall

Liten null-avhengighets-gateway (Node ≥21) som samler pipelinens LLM-konfig på
ETT sted. Alle konsumenter (integrations-routeren, proxy-agenten, …) peker på
`http://…:8787/v1` med en **fake API-nøkkel** som identifiserer konsumenten;
gatewayen velger backend per kall og legger på den ekte nøkkelen. Å bytte
modell-backend er dermed én endring i `routes.json` — ingen agent-`.env`-filer
eller container-restarts hos konsumentene.

Arvtakeren til `llm-proxy-proxy` (én upstream, én modell-overstyring); samme
passthrough-oppførsel (streaming/SSE pipes rett gjennom), men med ruting.

## Ruting

To trinn per forespørsel:

1. **Fake nøkkel → konsument** (`Authorization: Bearer <nøkkel>` eller
   `x-api-key`). Ukjent nøkkel avvises med 401 (fail closed).
2. **Første regel som passer vinner**, ovenfra: valgfrie betingelser
   `pathSuffix` (f.eks. `/embeddings`) og `whenModel` (eksakt match på
   innkommende `model`-felt). En regel uten betingelser er default. Regelen
   peker på en upstream og kan overstyre `model`-feltet — konsumentene kan
   dermed bruke stabile alias (f.eks. `router-chat`) som aldri endres.

`routes.json` (gitignorert, kopier fra `routes.example.json`) beskriver
konsumenter og regler. Upstream-adresser og ekte nøkler ligger KUN i `.env`
etter konvensjonen `UPSTREAM_<NAVN>_URL` / `UPSTREAM_<NAVN>_KEY`.

Eksempel — routeren i integrations sender chat til Aivar og embeddings til
lokal LM Studio over samme base-URL:

```json
"integrations-router": {
  "keys": ["integrations-router"],
  "rules": [
    { "pathSuffix": "/embeddings", "upstream": "lmstudio", "model": "text-embedding-qwen3-embedding-4b" },
    { "upstream": "aivar", "model": "aivar:gemma4" }
  ]
}
```

## Kjøring

Anbefalt: container (overlever omstart, samme mønster som resten av klyngen):

```powershell
Copy-Item .env.example .env       # fyll inn upstream-URL-er og ekte nøkler
Copy-Item routes.example.json routes.json
docker compose up -d --build
```

Alternativt rett på hosten (Node ≥21): `npm start`.

Helsesjekk uten auth: `GET /healthz` (kun navn — aldri nøkler).

## Claude Code-agenter med subscription-OAuth (jr-dev)

Claude Code snakker Anthropic-formatet (`/v1/messages`), som gatewayen sender
urørt videre — så en Claude Code-agent i container kan bruke gatewayen som
base-URL, med et **subscription-OAuth-token som aldri er inne i containeren**
(samme «credential non-possession»-idé som nvt-agents mediated mode, i
miniatyr — uten refresh-maskineri, siden `setup-token` er langlivet):

1. Kjør `claude setup-token` i et pålogget miljø (krever abonnement) og legg
   tokenet i `UPSTREAM_ANTHROPIC_KEY` i gatewayens `.env`.
2. Gi agenten en konsument-regel mot `anthropic` med
   `"appendHeaders": { "anthropic-beta": "oauth-2025-04-20" }` — OAuth-tokens
   krever denne beta-flaggingen, og klienten vet ikke selv at den kjører OAuth.
   `appendHeaders` *fletter* verdien inn i klientens eksisterende beta-liste
   (erstatter aldri — klienten eier sin egen feature-liste).
3. Agentens miljø: `ANTHROPIC_BASE_URL=http://host.docker.internal:8787` og
   `ANTHROPIC_AUTH_TOKEN=<konsument-nøkkelen>`. Ingen modell-overstyring —
   ekte modellnavn går rett gjennom.

Trafikken er ekte Claude Code-trafikk (kun auth-headeren byttes), så
Anthropics krav om Claude Code-systemprompt for subscription-tokens er
oppfylt av seg selv.

## Sikkerhet

- Ekte nøkler finnes kun i `.env` (gitignorert) og legges på server-side; de
  fake nøklene går aldri videre til upstream.
- De fake nøklene er ruting-etiketter og en grov sperre — ikke en
  sikkerhetsgrense. Gatewayen skal derfor kun være nåbar lokalt: den binder
  `127.0.0.1` (containere når den via `host.docker.internal`).
- Loggene inneholder konsument, path, modell og status — aldri nøkler eller
  meldingsinnhold.

## Verdt å vite

- Klientens path legges rett på upstream-URL-en (som i `llm-proxy-proxy`), så
  `/v1/models`, `/v1/embeddings` osv. fungerer. Har upstream et path-prefiks,
  ta det med i `UPSTREAM_<NAVN>_URL`.
- **Bytte av embedding-backend/-modell** endrer vektorrommet: routerens
  persisterte aktivitetsindeks (i integrations-state) må da nullstilles, ellers
  blir likhetssøkene stille dårlige.
- Gatewayen *oversetter ikke* mellom API-formater: Anthropic-formatet
  (`/v1/messages`) sendes urørt videre og krever en Anthropic-kompatibel
  upstream (api.anthropic.com — se OAuth-avsnittet — eller LM Studio).
  Skal Claude Code-agenter via et rent OpenAI-endepunkt (Aivar), trengs en
  oversettende proxy (f.eks. LiteLLM).
- Endringer i `routes.json`/`.env` leses ved oppstart — restart gatewayen
  (`docker compose restart`) for å ta dem i bruk.
