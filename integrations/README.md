# integrations

A small local bot that listens for GitHub and Slack events and reacts to them.
This first iteration uses a **pull / websocket** pattern only — it never exposes
an inbound webhook endpoint, so it can run entirely from your laptop.

## What it does

**GitHub** (polls the notifications API for the token's user):
- When the bot is **assigned** to an issue or PR → the assignment is a **work
  order**: it stays in place (the bot account owns the issue end-to-end) and
  the issue is handed to the agent queue, same as a mention.
- When the bot is **@-mentioned** in a comment or body → it reacts with 🚀
  (`rocket`, configurable) on the triggering comment (or the issue/PR itself).
- **Hard allowlist (fail-closed):** only logins listed in
  `GITHUB_ALLOWED_USERS` can trigger agent actions. The actor behind every
  relevant notification is looked up before anything else — for assigns the
  actor on the last `assigned` event (issue events API), for mentions the
  author of the exact content that becomes the prompt (the triggering
  comment, or the issue/PR body when there is none). Events from anyone else
  are dropped with a WARN log (actor, repo, issue, reason) and the thread is
  marked read — **no reaction, no queue, no router call**, so the bot never
  signals that it saw the event. An empty/unset list drops *all* GitHub work
  orders (a startup warning says so), and a failed actor lookup also drops
  the event (fail-closed).
- Self-triggered events (where the bot itself is the actor) are recognized
  the same way and skipped quietly (debug-logged) — they are noise, not an
  attack. The bot's own account should not be on the allowlist.
- **Residual risk** (gated elsewhere, by design): the allowlist gates
  *initiation*, not *content*. An allowed user can still point the agent at
  text written by a stranger (an issue body, a linked page), and prompt
  injection may live there. Defence in depth stays as before: delegated
  prompts and event text are treated as untrusted data, agent tokens carry no
  Contents access, and a human review gate covers sensitive paths.

**Slack** (connects over Socket Mode — a websocket, no public URL needed):
- On start it sets its presence to **active** (green dot); on shutdown it sets
  itself **away**.
- In a **1:1 DM** → it reacts with 🤷 (`shrug`, configurable) to **every**
  message (they are all directed at the bot).
- In **channels, private groups, group DMs and threads** → it only reacts when
  the bot is **@-mentioned**. Other messages are ignored.
- It also picks up **emoji reactions** (`reaction_added`) — but only on the
  bot's **own messages** or in **threads the bot participates in**; reactions
  elsewhere in channels it happens to be a member of are ignored. When the
  queue bridge is on, the reaction and the message it sits on are handed to
  the agent, which interprets what the reaction means (e.g. a 👍 as positive
  feedback). The bot ignores its own reactions to avoid loops.

**Agent queue bridge** (optional — connects to the `proxy-agent` LLM worker):
- When enabled, each event the bot reacts to is also handed off to
  [`proxy-agent`](../agents/proxy-agent) by appending one JSON line to its
  `triggers/inbox.jsonl`. `proxy-agent` runs the LLM agent in an isolated Docker
  container and writes its answer to `triggers/results.jsonl` + `logs/<id>.log`.
- The bot polls `results.jsonl` and delivers each answer back where it came
  from, based on how the agent **classified** the input (`intent`):
  - **`action`** / **`feedback`** → post the agent's `reply` as a **threaded
    reply** in Slack / an **issue/PR comment** on GitHub.
  - **`ack`** (a pure "noted"/thanks) → post nothing, just add an
    acknowledgement reaction (Slack ✅ `white_check_mark`, GitHub 👍 `+1`).
  - Results without an `intent` (older proxy-agent) fall back to posting the log.
- While the agent works, the bot shows a **"working" reaction** (Slack
  🤔 `thinking_face`, GitHub 👀 `eyes`) on the triggering message and **removes
  it once the result is delivered**. (When the bridge is off, it instead leaves
  a persistent acknowledgement reaction — 🤷 / 🚀.)

The bot and proxy-agent agree on a small **result contract** in `results.jsonl`
(all classification fields optional, so it stays backward compatible):

```jsonc
{ "id": "…", "status": "ok",
  "intent": "action" | "feedback" | "ack" | "delegate",  // how the agent read the input
  "reply": "clean answer text to post back", // separate from the raw log
  "delegate": { "agent": "target-agent-name", "prompt": "…", "payload": {…} }, // required with "delegate" — see below

  "log": "logs/<id>.log", … }
```

This is the "receiver" role described in proxy-agent's own README: integrations is
the only side that holds Slack/GitHub tokens; proxy-agent only ever touches the
shared `triggers/` files. Enable it with `AGENT_QUEUE_ENABLED=true` and point
`AGENT_TRIGGERS_DIR` at proxy-agent's `triggers/` directory (both apps run on the
same machine).

**Delegation between agents** (optional — `AGENT_ROUTES`):
- An agent can hand a task off to another agent by answering with
  `intent: "delegate"` and a `delegate` object in its result line:

  ```jsonc
  { "id": "…", "status": "ok", "intent": "delegate",
    "reply": "short interim notice posted to the origin",
    "delegate": { "agent": "local-cc-coding-agent",
                  "prompt": "complete, self-contained task description",
                  "payload": { "issue": "https://github.com/…" } } }
  ```

- integrations (the hub — agents never write to each other's directories)
  appends the task as a new event in the target agent's `inbox.jsonl`
  (`source: "agent"`, `type: "delegation"`, with `payload.origin` linking back
  to the delegating agent and event), and watches **every** configured agent's
  `results.jsonl`.
- The pending reply is remapped, so the target agent's eventual answer is
  posted back to the **original** Slack thread / GitHub issue. The origin gets
  the interim `reply` right away and keeps its "working" reaction until the
  final answer arrives.
- Targets must be allowlisted in `AGENT_ROUTES` (comma-separated agent names,
  resolved as `<AGENT_AGENTS_DIR>/<name>/triggers`). Unknown targets are
  reported back to the origin instead of executed. `AGENT_MAX_DELEGATION_HOPS`
  (default 2) caps chains so two agents cannot ping-pong a task forever.
- **Debrief** (`AGENT_DELEGATION_DEBRIEF`, default true): once the delegated
  answer has been delivered, the delegating agent gets a `delegation-outcome`
  event in its inbox (target agent, status, the delivered reply) so it can
  reflect on how the delegation went. The debrief has **no** pending reply
  route — the agent's result on it is consumed without posting anything to
  Slack/GitHub — and it does not count as a delegation hop, so it can never
  start a delegation loop.

**First-line router** (optional — `ROUTER_BASE_URL`):
- Before an external event is appended to the agent's inbox, a small local
  model annotates it — routing/classification is one structured LLM call, not
  agentic work, so it lives here in integrations, which is the only component
  that sees the whole event stream from both channels:
  - **`classification`** — `action` / `feedback` / `ack` / `delegate`, from
    one chat call with structured output (`ROUTER_MODEL`).
  - **`related_activities`** — references (never text) to similar open
    activities: every Slack thread / GitHub issue the bot has seen becomes an
    entry in a small embedding index (`ROUTER_EMBEDDING_MODEL`, embedding of
    the title/first message), and new events are matched against it with
    cosine similarity (`ROUTER_MATCH_THRESHOLD`, top `ROUTER_MAX_RELATED`).
    Lets the agent spot "this is the same topic as issue #42" across
    channels instead of creating duplicates. The index is persisted in
    `AGENT_STATE_DIR` (the `integrations-state` volume in Docker), so it
    survives restarts.
- The router **only annotates**. It never drops or reroutes an event, and the
  first milestone does not change who receives it — the agent decides what
  the annotations mean. Delegation/debrief events (`source: "agent"`) are
  internal traffic and are not routed.
- **Failure = fallback, never blockage**: endpoint down, timeout
  (`ROUTER_TIMEOUT_MS`) or invalid model output logs a warning and queues the
  event unannotated. Model output is validated against a fixed schema before
  it is allowed onto the event — the event text is untrusted input, and the
  router's output is treated as data, not instructions. The router holds no
  tokens and has no tools.
- Empty `ROUTER_BASE_URL` (the default) disables all of this; events are
  queued exactly as before.

- **Short-circuit** (`ROUTER_SHORT_CIRCUIT`, default `"ack"`): when the router
  classifies an event as `"ack"`, integrations delivers it directly through the
  connector (adds ack reaction, clears working reaction) and skips appending to
  `inbox.jsonl` — proxy-agent never wakes. Empty env var disables short-circuit;
  only `"ack"` or empty is accepted (other values are normalized to empty). This
  is purely an optimization: if a classified event still gets queued (router
  failed / no classification), the agent's own classification step will still
  see it and act accordingly.

**Slack-DM review notifier** (optional — `SLACK_REVIEW_NOTIFY_USER`, issue #115):
- Independently of the agent queue bridge, polls the repos listed in
  `GITHUB_REVIEW_NOTIFY_REPOS` (GraphQL) for open, non-draft pull requests
  with `reviewDecision == REVIEW_REQUIRED` ("awaiting human review") and DMs
  one summary (title, author, link) per PR to the Slack user in
  `SLACK_REVIEW_NOTIFY_USER`.
- A PR carrying the `auto-merge` label is skipped **unless** it also touches
  a path listed in the repo's `CODEOWNERS` — such a PR merges on its own once
  CI passes (see [`doc/pr-prosess.md`](../doc/pr-prosess.md)), so a DM would
  be noise; a CODEOWNERS-path PR is blocked on human review regardless of the
  label, so it still gets notified.
- Dedupe is persisted (`review-notified.json` in `AGENT_STATE_DIR`): one
  notification per PR number, ever.
- Notify failures (Slack API errors) are logged as WARN and never stop the
  poll loop; the PR is retried on the next cycle since it is not marked
  notified until the DM succeeds.
- Empty `SLACK_REVIEW_NOTIFY_USER` (the default) disables the notifier
  entirely. Requires both `GITHUB_ENABLED` and `SLACK_ENABLED`.

## Requirements

- **Node ≥ 23** (runs the TypeScript directly via type stripping — no build step),
  or **Bun** (bonus). Node 24 is what this was developed on.

## Setup

```bash
npm install          # or: bun install
cp .env.example .env # then fill in credentials
```

### GitHub credentials

Create tokens **on the bot's own user account**. The bot acts as that user: it
only sees assignments/mentions delivered to that account's notifications.

The notifications API does **not** work with fine-grained PATs, so credentials
are split into two roles:

- **`GITHUB_TOKEN`** — issue/PR actions (react, comment). A **fine-grained** PAT
  with Read+Write on *Issues* and *Pull requests* works, as does a classic `repo`
  token.
- **`GITHUB_TOKEN_CLASSIC_NOTIFICATIONS`** — the notifications API. Must be a
  **classic** PAT with the `notifications` scope.

Single-token shortcut: if you have one **classic** PAT with **both**
`notifications` and `repo` scopes, it works for everything — put it in either
variable and leave the other blank. The two tokens fall back to each other.
(This is often the simplest route for organization repos.)

### Slack credentials

In your Slack app config (https://api.slack.com/apps):
1. **Socket Mode** → enable it. This creates an **App-Level Token** (`xapp-…`)
   with the `connections:write` scope → `SLACK_APP_TOKEN`.
2. **OAuth & Permissions** → add bot scopes `reactions:write`, `reactions:read`
   (to observe reactions), `users:write` (to set active/away presence) and
   `chat:write` (to post the agent's answer back into the thread), plus the
   history scopes for the surfaces you care about (`channels:history`,
   `groups:history`, `im:history`, `mpim:history`). Install to the workspace →
   copy the **Bot User OAuth Token** (`xoxb-…`) → `SLACK_BOT_TOKEN`.
3. **Event Subscriptions** → subscribe to the matching bot events
   (`message.channels`, `message.groups`, `message.im`, `message.mpim`) and
   `reaction_added` (so the bot receives reactions on messages).
4. Invite the bot to the channels it should watch (`/invite @your-bot`).

If your workspace has no `:shrug:` emoji, set `SLACK_REACTION=man-shrugging`
(or `woman-shrugging`).

## Run

```bash
npm start            # Node
npm run start:bun    # Bun
npm run typecheck    # tsc --noEmit
```

Set `LOG_LEVEL=debug` for verbose output. Stop with Ctrl-C (graceful shutdown).

### Run in Docker

```bash
docker compose up -d --build   # from this directory
docker compose logs -f
```

The container reads credentials from the same `.env` file, but compose
overrides `AGENT_TRIGGERS_DIR`/`AGENT_STATE_DIR` with container paths: the
queue is a bind mount of `../agents/proxy-agent/triggers` (shared with the
proxy-agent container) and state lives in a named volume. To run the whole
pipeline (this app + agents) in one go, use the compose file at the repo root
instead.

Each connector is independent: enable one or both via `GITHUB_ENABLED` /
`SLACK_ENABLED`. A failure in one connector is logged and does not stop the other.

## Configuration

All configuration is via `.env` — see [`.env.example`](.env.example) for the full
list and defaults (`GITHUB_TOKEN`, `GITHUB_REACTION`, `GITHUB_POLL_INTERVAL`,
`GITHUB_API_URL`, `SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN`, `SLACK_REACTION`, …).

## How the pull/websocket pattern works here

- **GitHub:** repeatedly calls `GET /notifications` (unread only), honoring the
  server's `X-Poll-Interval` as a lower bound and using `If-Modified-Since` for
  cheap 304s. Handled threads are marked read so they don't reappear.
- **Slack:** opens a websocket via Socket Mode and receives events over it; each
  event is acknowledged immediately. No incoming HTTP endpoint is exposed.
