import http from 'node:http'
import fs from 'node:fs'
import { fileURLToPath } from 'node:url'

// Last .env hvis den finnes (innebygd i Node 21+)
try { process.loadEnvFile() } catch {}

const PORT = Number(process.env.PORT ?? 8787)
const HOST = process.env.HOST ?? '127.0.0.1'
const ROUTES_FILE = process.env.ROUTES_FILE ?? fileURLToPath(new URL('./routes.json', import.meta.url))

// ---------------------------------------------------------------------------
// Konfig: routes.json beskriver konsumenter (fake API-nøkler) og regler.
// Upstream-adresser og EKTE nøkler ligger kun i .env, etter konvensjonen
// UPSTREAM_<NAVN>_URL / UPSTREAM_<NAVN>_KEY (navn med store bokstaver, «-» → «_»).
// ---------------------------------------------------------------------------
let config
try {
  config = JSON.parse(fs.readFileSync(ROUTES_FILE, 'utf8'))
} catch (err) {
  console.error(`Klarte ikke lese ${ROUTES_FILE}: ${err.message}`)
  console.error('Kopier routes.example.json til routes.json og tilpass.')
  process.exit(1)
}

const envName = (name) => name.toUpperCase().replaceAll('-', '_')

// Slå opp alle upstreams reglene refererer, og feil ved oppstart (ikke ved
// første kall) hvis en URL mangler i miljøet.
const upstreams = new Map()
for (const [consumerName, consumer] of Object.entries(config.consumers ?? {})) {
  for (const rule of consumer.rules ?? []) {
    const name = rule.upstream
    if (!name) {
      console.error(`Konsument "${consumerName}" har en regel uten "upstream".`)
      process.exit(1)
    }
    if (upstreams.has(name)) continue
    const url = (process.env[`UPSTREAM_${envName(name)}_URL`] ?? '').replace(/\/+$/, '')
    const apiKey = process.env[`UPSTREAM_${envName(name)}_KEY`] ?? ''
    if (!url) {
      console.error(`UPSTREAM_${envName(name)}_URL mangler i .env (kreves av upstream "${name}").`)
      process.exit(1)
    }
    upstreams.set(name, { name, url, apiKey })
  }
}

// Fake nøkkel → konsument. Nøklene er ruting-etiketter og en grov sperre på
// localhost — ikke en sikkerhetsgrense. Ukjent nøkkel avvises (fail closed).
const byKey = new Map()
for (const [name, consumer] of Object.entries(config.consumers ?? {})) {
  for (const key of consumer.keys ?? []) {
    if (byKey.has(key)) {
      console.error(`Nøkkelen for "${name}" er allerede i bruk av "${byKey.get(key).name}".`)
      process.exit(1)
    }
    byKey.set(key, { name, rules: consumer.rules ?? [] })
  }
}

if (byKey.size === 0) {
  console.error('routes.json har ingen konsumenter med nøkler — ingenting å rute.')
  process.exit(1)
}

// Headere som ikke skal videresendes i noen retning
const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization',
  'te', 'trailer', 'transfer-encoding', 'upgrade', 'host',
])

function bearerToken(req) {
  const auth = req.headers['authorization']
  if (typeof auth === 'string' && auth.toLowerCase().startsWith('bearer ')) return auth.slice(7).trim()
  const apiKey = req.headers['x-api-key']
  if (typeof apiKey === 'string' && apiKey !== '') return apiKey.trim()
  return ''
}

// Første regel som passer vinner: pathSuffix og whenModel er valgfrie
// betingelser; en regel uten betingelser er konsumentens default.
function pickRule(rules, pathname, model) {
  for (const rule of rules) {
    if (rule.pathSuffix && !pathname.endsWith(rule.pathSuffix)) continue
    if (rule.whenModel && rule.whenModel !== model) continue
    return rule
  }
  return undefined
}

function deny(res, status, message) {
  res.writeHead(status, { 'content-type': 'application/json' })
  res.end(JSON.stringify({ error: { message, type: 'gateway_error' } }))
}

const server = http.createServer(async (req, res) => {
  const pathname = (req.url ?? '/').split('?')[0]

  // Helsesjekk uten auth — brukes av compose/konsoll. Aldri hemmeligheter her.
  if (req.method === 'GET' && pathname === '/healthz') {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify({
      ok: true,
      consumers: [...new Set([...byKey.values()].map((c) => c.name))],
      upstreams: [...upstreams.keys()],
    }))
    return
  }

  try {
    const consumer = byKey.get(bearerToken(req))
    if (!consumer) {
      console.warn(`${req.method} ${pathname} -> 401 (ukjent nøkkel)`)
      deny(res, 401, 'Ukjent API-nøkkel. Gatewayen ruter kun kjente konsumenter (se routes.json).')
      return
    }

    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    let body = chunks.length ? Buffer.concat(chunks) : undefined

    // Plukk ut model-feltet fra JSON-bodyer, både for regelvalg og overstyring
    let json
    const contentType = req.headers['content-type'] ?? ''
    if (body && contentType.includes('application/json')) {
      try { json = JSON.parse(body.toString('utf8')) } catch { /* ikke gyldig JSON – urørt videre */ }
    }
    const incomingModel = json && typeof json === 'object' ? json.model : undefined

    const rule = pickRule(consumer.rules, pathname, incomingModel)
    if (!rule) {
      console.warn(`${consumer.name}: ${req.method} ${pathname} (model: ${incomingModel ?? '-'}) -> 404 (ingen regel)`)
      deny(res, 404, `Ingen regel for ${pathname} hos konsumenten "${consumer.name}".`)
      return
    }
    const upstream = upstreams.get(rule.upstream)

    let modelNote = ''
    if (rule.model && json && typeof json === 'object' && 'model' in json) {
      modelNote = ` (model: ${json.model} -> ${rule.model})`
      json.model = rule.model
      body = Buffer.from(JSON.stringify(json))
    }

    const headers = {}
    for (const [key, value] of Object.entries(req.headers)) {
      if (!HOP_BY_HOP.has(key) && key !== 'content-length' && key !== 'x-api-key') headers[key] = value
    }
    // Den fake nøkkelen skal aldri videre; upstream får sin ekte nøkkel — eller
    // ingen auth i det hele tatt (lokale servere som LM Studio).
    if (upstream.apiKey) headers['authorization'] = `Bearer ${upstream.apiKey}`
    else delete headers['authorization']

    // appendHeaders (f.eks. anthropic-beta for subscription-OAuth): flettes inn
    // i klientens eksisterende verdi i stedet for å erstatte den — klienten
    // eier sin egen feature-liste. Kun for ikke-hemmelige verdier.
    for (const [name, value] of Object.entries(rule.appendHeaders ?? {})) {
      const key = name.toLowerCase()
      const existing = headers[key]
      if (existing === undefined || existing === '') headers[key] = value
      else if (!String(existing).split(',').map((s) => s.trim()).includes(value)) {
        headers[key] = `${existing},${value}`
      }
    }

    const started = Date.now()
    const upstreamRes = await fetch(upstream.url + req.url, {
      method: req.method,
      headers,
      body,
    })

    // fetch dekomprimerer responsen, så content-encoding/length stemmer ikke lenger
    const resHeaders = {}
    for (const [key, value] of upstreamRes.headers) {
      if (!HOP_BY_HOP.has(key) && key !== 'content-encoding' && key !== 'content-length') {
        resHeaders[key] = value
      }
    }
    res.writeHead(upstreamRes.status, resHeaders)

    if (upstreamRes.body) {
      for await (const chunk of upstreamRes.body) res.write(chunk)
    }
    res.end()

    console.log(`${consumer.name}: ${req.method} ${pathname} -> ${upstream.name} ${upstreamRes.status}${modelNote} (${Date.now() - started}ms)`)
  } catch (err) {
    console.error(`${req.method} ${pathname} -> feil:`, err.cause ?? err)
    if (!res.headersSent) {
      res.writeHead(502, { 'content-type': 'application/json' })
    }
    res.end(JSON.stringify({ error: { message: `Gateway-feil: ${err.message}`, type: 'gateway_error' } }))
  }
})

server.listen(PORT, HOST, () => {
  console.log(`llm-gateway lytter på http://${HOST}:${PORT} (ruter: ${ROUTES_FILE})`)
  for (const [name, consumer] of Object.entries(config.consumers ?? {})) {
    const targets = [...new Set((consumer.rules ?? []).map((r) => r.upstream))].join(', ')
    console.log(`  - ${name} -> ${targets}`)
  }
})
