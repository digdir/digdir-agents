import { promises as fs } from "node:fs";
import path from "node:path";
import type { AgentQueueConfig, AgentRoute } from "../config.ts";
import { createLogger } from "../logger.ts";
import type { Router } from "../router/router.ts";
import type { SlackConnector } from "../slack/connector.ts";
import type { GithubPoller } from "../github/poller.ts";
import type { Delivery, QueueEvent, ReplyContext, ResultLine, ResultPosters } from "./types.ts";

type Connectors = { slack?: SlackConnector; github?: GithubPoller };

const log = createLogger("queue");

/**
 * Bridges integrations to the agents over their shared `triggers/` directories:
 *   - {@link submit} appends an event to the primary agent's `inbox.jsonl` and
 *     remembers where its eventual result should be posted (the pending map,
 *     persisted to disk so it survives restarts between submit and result).
 *   - {@link startResultWatcher} tails every agent's `results.jsonl`, reads the
 *     matching log, and hands the answer to the registered posters.
 *   - A result with `intent: "delegate"` is routed onwards: the task becomes a
 *     new event in the target agent's inbox, and the pending reply is remapped
 *     so the final answer still lands in the originating thread/issue.
 *   - Once a delegated answer has been delivered, the delegating agent gets a
 *     `delegation-outcome` debrief event ({@link sendDebrief}) so it can
 *     reflect on the process. The debrief has no pending reply route.
 */
export class AgentQueue {
  private readonly config: AgentQueueConfig;
  /** All queue-connected agents: primary first, then delegation routes. */
  private readonly agents: AgentRoute[];
  /** id -> where to post the result. The source of truth; disk mirrors it. */
  private pending = new Map<string, ReplyContext>();
  /** Per-agent read offset into its results.jsonl. */
  private offsets = new Map<string, number>();
  /** Serializes state writes so overlapping saves cannot corrupt the file. */
  private writeChain: Promise<void> = Promise.resolve();
  private readonly pendingFile: string;
  /** Optional first-line router; annotates external events before the append. */
  private readonly router: Router | null;
  /** Connectors wired up after construction so submit() can short-circuit acks directly. */
  private connectors: Connectors | null = null;

  constructor(config: AgentQueueConfig, router: Router | null = null) {
    this.config = config;
    this.router = router;
    this.pendingFile = path.join(config.stateDir, "pending.json");
    this.agents = [
      {
        name: config.primaryName,
        triggersDir: config.triggersDir,
        inboxFile: config.inboxFile,
        resultsFile: config.resultsFile,
      },
      ...config.routes,
    ];
  }

  /**
   * Wires the Slack and GitHub connectors into the queue so ack events can be
   * delivered directly (ack reaction + working-reaction cleanup) without going
   * through inbox.jsonl. Must be called after the connectors are constructed in
   * main(); called with null to drop them again.
   */
  setConnectors(connectors: Connectors | null): void {
    this.connectors = connectors;
  }

  /** The primary agent keeps the historic offset filename; routes get their own. */
  private offsetFileFor(agent: AgentRoute): string {
    const suffix = agent.name === this.config.primaryName ? "" : `-${agent.name}`;
    return path.join(this.config.stateDir, `results${suffix}.offset`);
  }

  /** Ensures directories exist and loads the persisted pending map. */
  async init(): Promise<void> {
    for (const agent of this.agents) {
      await fs.mkdir(agent.triggersDir, { recursive: true });
    }
    await fs.mkdir(this.config.stateDir, { recursive: true });
    try {
      const raw = await fs.readFile(this.pendingFile, "utf8");
      const obj = JSON.parse(raw) as Record<string, ReplyContext>;
      this.pending = new Map(Object.entries(obj));
      if (this.pending.size > 0) {
        log.info(`Loaded ${this.pending.size} pending reply/replies awaiting results.`);
      }
    } catch {
      // No pending file yet — start empty.
    }
    const routeNames = this.config.routes.map((r) => r.name).join(", ") || "(ingen)";
    log.info(`Queue bridge ready. Inbox: ${this.config.inboxFile}. Delegation routes: ${routeNames}`);
  }

  /**
   * Appends an event to the inbox and records where its result should go.
   * The append is a single write, so with a single writer (this process) it is
   * effectively atomic; proxy-agent only ever reads the file.
   */
  async submit(event: QueueEvent, reply: ReplyContext): Promise<void> {
    // First-line router (optional): annotate with classification and related
    // activities before the append. annotate() never throws — on any failure
    // or timeout the event goes through unannotated, exactly as before.
    const enriched = this.router ? await this.router.annotate(event) : event;

    // Short-circuit: if the router classified this as an ack, deliver it
    // directly via the connectors — add the ack reaction and clear the working
    // reaction, without waking proxy-agent or appending to inbox. Falls back to
    // normal queuing when:
    //   - shortCircuit is disabled (empty env var)
    //   - the router did not run / failed (no classification)
    //   - the event's classification is not exactly "ack"
    if (this.config.routerShortCircuit && enriched.classification === "ack" && this.connectors) {
      const delivered = await this.deliverAckDirectly(reply, enriched.id);
      if (delivered) return;
      // Failed delivery: continue to normal queue path below.
    }

    await fs.appendFile(this.config.inboxFile, JSON.stringify(enriched) + "\n", "utf8");
    this.pending.set(enriched.id, reply);
    await this.persistPending();
    log.info(`Queued ${enriched.source} event "${enriched.id}" for the agent.`);
  }

  /**
   * Delivers an ack directly via the connectors (ack reaction + working-reaction
   * cleanup) without touching inbox.jsonl. Returns true on success, false on
   * failure (so the caller can fall back to normal queuing). Logs each attempt
   * so the outcome is visible.
   */
  private async deliverAckDirectly(reply: ReplyContext, id: string): Promise<boolean> {
    const connectors = this.connectors;
    if (!connectors) return false;

    try {
      if (reply.kind === "slack" && connectors.slack) {
        await connectors.slack.deliver(reply, { kind: "ack" });
      } else if (reply.kind === "github" && connectors.github) {
        await connectors.github.deliver(reply, { kind: "ack" });
      } else {
        log.warn(`No connector for ack delivery of "${id}" (${reply.kind}).`);
        return false;
      }
    } catch (err) {
      log.error(`Failed to short-circuit ack for "${id}"; will fall back to normal queuing.`, err);
      return false;
    }

    // Best effort: if we delivered successfully, drop the pending entry — there
    // is no agent result coming for this event. If delivery failed (above), we
    // returned false and the caller will append to inbox.jsonl.
    try {
      this.pending.delete(id);
      await this.persistPending();
    } catch {
      // Persist failure is not fatal — pending survives in memory across cycles.
    }

    log.info(`Short-circuited ack for event "${id}" (${reply.kind}).`);
    return true;
  }

  /**
   * Polls every agent's `results.jsonl` for new lines and delivers each answer
   * through the matching poster. Tracks a byte offset per agent (persisted) so
   * results are processed exactly once, and only whole lines (ending in "\n")
   * are consumed.
   */
  async startResultWatcher(posters: ResultPosters, signal: AbortSignal): Promise<void> {
    for (const agent of this.agents) {
      const offset = await this.loadOffset(agent);
      this.offsets.set(agent.name, offset);
      log.info(`Watching results for "${agent.name}" from offset ${offset} in ${agent.resultsFile}.`);
    }

    while (!signal.aborted) {
      for (const agent of this.agents) {
        try {
          const next = await this.drainResults(agent, this.offsets.get(agent.name) ?? 0, posters);
          this.offsets.set(agent.name, next);
        } catch (err) {
          log.error(`Result poll cycle for "${agent.name}" failed; will retry.`, err);
        }
      }
      await sleep(this.config.resultsPollIntervalSeconds * 1000, signal);
    }
  }

  private async drainResults(agent: AgentRoute, offset: number, posters: ResultPosters): Promise<number> {
    let stat: Awaited<ReturnType<typeof fs.stat>>;
    try {
      stat = await fs.stat(agent.resultsFile);
    } catch {
      return offset; // No results file yet.
    }
    if (stat.size <= offset) {
      // File was truncated/rotated — restart from the beginning.
      return stat.size < offset ? 0 : offset;
    }

    const fh = await fs.open(agent.resultsFile, "r");
    let text: string;
    try {
      const buf = Buffer.alloc(stat.size - offset);
      await fh.read(buf, 0, buf.length, offset);
      text = buf.toString("utf8");
    } finally {
      await fh.close();
    }

    const lastNl = text.lastIndexOf("\n");
    if (lastNl === -1) return offset; // No complete line yet.

    const complete = text.slice(0, lastNl + 1);
    for (const line of complete.split("\n")) {
      if (line.trim() === "") continue;
      await this.handleResultLine(agent, line, posters);
    }
    const newOffset = offset + Buffer.byteLength(complete, "utf8");
    await this.saveOffset(agent, newOffset);
    return newOffset;
  }

  private async handleResultLine(agent: AgentRoute, line: string, posters: ResultPosters): Promise<void> {
    let result: ResultLine;
    try {
      result = JSON.parse(line) as ResultLine;
    } catch {
      log.warn(`Skipping malformed results line from "${agent.name}": ${line.slice(0, 200)}`);
      return;
    }

    const reply = this.pending.get(result.id);
    if (!reply) {
      log.debug(`No pending reply for result "${result.id}" — not ours or already handled.`);
      return;
    }

    if (result.status === "ok" && result.intent === "delegate") {
      await this.handleDelegation(agent, result, reply, posters);
      return;
    }

    const delivery = await this.composeDelivery(agent, result, reply.kind);
    const posted = await this.post(reply, delivery, posters, result.id);
    if (!posted) return; // Keep pending; the next cycle retries.
    log.info(`Delivered result "${result.id}" to ${reply.kind} as ${delivery.kind} (intent=${result.intent ?? "?"}).`);

    this.pending.delete(result.id);
    await this.persistPending();
    await this.sendDebrief(agent, result, reply, delivery);
  }

  /**
   * Routes a `delegate` result onwards: appends the task as a new event in the
   * target agent's inbox and remaps the pending reply to the new event id, so
   * the target's eventual result is posted to the original thread/issue. The
   * origin gets an interim notice, but keeps its "working" reaction — the task
   * is still in progress until the target agent answers.
   */
  private async handleDelegation(
    from: AgentRoute,
    result: ResultLine,
    reply: ReplyContext,
    posters: ResultPosters,
  ): Promise<void> {
    const targetName = (result.delegate?.agent ?? "").trim();
    const prompt = (result.delegate?.prompt ?? "").trim();
    const target = this.agents.find((a) => a.name === targetName && a.name !== from.name);
    const hops = (reply.hops ?? 0) + 1;

    let failure = "";
    if (!targetName || !prompt) {
      failure = `Agenten «${from.name}» ville delegere, men resultatet mangler delegate.agent/delegate.prompt.`;
    } else if (!target) {
      failure = `Ingen utførende agent «${targetName}» er konfigurert (AGENT_ROUTES) — kan ikke delegere.`;
    } else if (hops > this.config.maxDelegationHops) {
      failure = `Maks delegeringsdybde (${this.config.maxDelegationHops}) er nådd — stopper hos «${from.name}».`;
    }
    if (failure) {
      log.warn(`Delegation of "${result.id}" rejected: ${failure}`);
      const posted = await this.post(reply, { kind: "message", text: `⚠️ ${failure}` }, posters, result.id);
      if (!posted) return; // Keep pending; the next cycle retries.
      this.pending.delete(result.id);
      await this.persistPending();
      return;
    }

    const newId = `${result.id}-d${hops}`.replace(/[^a-zA-Z0-9._-]/g, "_");
    const event: QueueEvent = {
      id: newId,
      source: "agent",
      type: "delegation",
      received_at: new Date().toISOString(),
      prompt,
      payload: {
        ...(result.delegate?.payload ?? {}),
        origin: { agent: from.name, event_id: result.id, hops },
      },
    };
    await fs.appendFile(target!.inboxFile, JSON.stringify(event) + "\n", "utf8");

    this.pending.delete(result.id);
    this.pending.set(newId, { ...reply, hops, origin: { agent: from.name, eventId: result.id } });
    await this.persistPending();
    log.info(`Delegated "${result.id}" from "${from.name}" to "${target!.name}" as "${newId}".`);

    // Interim notice (best effort — the delegation itself is already done).
    const text = (result.reply ?? "").trim() || `🔁 Delegert til ${target!.name}.`;
    await this.post(reply, { kind: "message", text: truncate(text, this.config.maxReplyChars) }, posters, newId, {
      keepWorking: true,
    });
  }

  /**
   * Debriefs the delegating agent after a delegated answer has been delivered:
   * appends a `delegation-outcome` event to its inbox so it can reflect on the
   * process. The event deliberately gets no pending reply route — the agent's
   * result on it is consumed without posting anything to Slack/GitHub, and
   * (for the same reason) it can never start a new delegation, so no hop is
   * spent. Best effort: a failed append is logged and dropped, never retried.
   */
  private async sendDebrief(from: AgentRoute, result: ResultLine, reply: ReplyContext, delivery: Delivery): Promise<void> {
    const origin = reply.origin;
    if (!this.config.delegationDebrief || !origin) return;
    const originAgent = this.agents.find((a) => a.name === origin.agent);
    if (!originAgent) {
      log.warn(`No agent "${origin.agent}" configured for the debrief of "${result.id}" — skipping.`);
      return;
    }
    // Use the actual delivered text (which may include error-wrapped or fallback text).
    const deliveredText = delivery.kind === "message" ? delivery.text : (result.reply ?? "").trim();
    const summary = truncate(deliveredText, 2000);
    const event: QueueEvent = {
      id: `${origin.eventId}-outcome`.replace(/[^a-zA-Z0-9._-]/g, "_"),
      source: "agent",
      type: "delegation-outcome",
      received_at: new Date().toISOString(),
      prompt:
        `Debrief: oppgaven du delegerte til «${from.name}» (ditt event «${origin.eventId}») er ` +
        `levert til brukeren med status «${result.status}». Svaret var: «${summary}». ` +
        `Reflekter kort over delegeringsprosessen og svar med intent "ack" — svaret ditt postes ikke videre.`,
      payload: {
        delegated_to: from.name,
        status: result.status,
        reply: truncate(deliveredText, this.config.maxReplyChars),
        origin_event_id: origin.eventId,
      },
    };
    try {
      await fs.appendFile(originAgent.inboxFile, JSON.stringify(event) + "\n", "utf8");
      log.info(`Debriefed "${originAgent.name}" about "${result.id}" (delegation-outcome "${event.id}").`);
    } catch (err) {
      log.warn(`Could not append delegation-outcome for "${result.id}" to "${originAgent.name}".`, err);
    }
  }

  /**
   * Posts a delivery to the origin. Returns false when no poster is available
   * or posting failed (callers keep the reply pending and retry later).
   * `keepWorking` strips the working-reaction bookkeeping from the context so
   * the reaction survives an interim message.
   */
  private async post(
    reply: ReplyContext,
    delivery: Delivery,
    posters: ResultPosters,
    id: string,
    opts: { keepWorking?: boolean } = {},
  ): Promise<boolean> {
    let ctx = reply;
    if (opts.keepWorking) {
      ctx = { ...reply };
      if (ctx.kind === "slack") {
        delete ctx.messageTs;
        delete ctx.workingReaction;
      } else {
        delete ctx.reactionsUrl;
        delete ctx.reactionId;
      }
    }
    try {
      if (ctx.kind === "slack" && posters.slack) {
        await posters.slack(ctx, delivery);
      } else if (ctx.kind === "github" && posters.github) {
        await posters.github(ctx, delivery);
      } else {
        log.warn(`No poster registered for ${ctx.kind} — cannot deliver "${id}".`);
        return false;
      }
      return true;
    } catch (err) {
      log.error(`Failed to deliver "${id}" to ${ctx.kind}; will retry.`, err);
      return false;
    }
  }

  /** Decides how to deliver a result: a posted message, or a silent ack. */
  private async composeDelivery(agent: AgentRoute, result: ResultLine, replyKind: "slack" | "github"): Promise<Delivery> {
    const extractionFailed = result.extraction_failed === true;
    const isGithub = replyKind === "github";
    const isProxy = this.isProxyAgent(agent);

    if (result.status !== "ok") {
      let detail = (result.reply ?? "").trim();
      // For GitHub delivery of proxy-agent with extraction_failed, use generic warning
      if (!detail && extractionFailed && isProxy && isGithub) {
        detail = "Agenten leverte uten strukturert svar — se logg.";
      } else if (!detail) {
        // Slack or non-proxy: use raw log fallback
        detail = (await this.readLog(agent, result)) || `exit code ${result.exit_code ?? "?"}`;
      }
      return { kind: "message", text: truncate(`⚠️ Agenten feilet (${result.status}).\n\n${detail}`, this.config.maxReplyChars) };
    }

    // A pure acknowledgement needs no message — the connector just reacts.
    if (result.intent === "ack") return { kind: "ack" };

    // action / feedback / unknown → post the clean reply, falling back to the
    // raw log for older results that carry no reply field. For GitHub we do
    // not expose the internal agent log as a public comment — use a generic
    // warning instead so transcriptions with internal context are never
    // posted publicly. Slack keeps the legacy behaviour (raw log is fine in
    // private channels). When extraction_failed is set, the agent found the
    // RESULT_MARKER but produced malformed JSON; the raw log almost certainly
    // contains the full transcription, so we skip it for proxy-agent on GitHub.
    let text = (result.reply ?? "").trim();
    if (!text) {
      // For GitHub delivery of proxy-agent with extraction_failed, use generic warning
      if (extractionFailed && isProxy && isGithub) {
        text = "⚠️ Agenten leverte uten strukturert svar — se logg.";
      } else if (!extractionFailed || !isProxy) {
        // Slack or non-proxy or no extraction failure: use raw log fallback
        text = await this.readLog(agent, result);
      }
      if (!text && extractionFailed && isProxy) {
        text = "⚠️ Agenten leverte uten strukturert svar — se logg.";
      } else if (!text) {
        text = "✅ Agenten er ferdig, men produserte ingen tekst.";
      }
    }
    return { kind: "message", text: truncate(text, this.config.maxReplyChars) };
  }

  /** Reads the agent's log file for a result (empty string if unavailable). */
  private async readLog(agent: AgentRoute, result: ResultLine): Promise<string> {
    if (!result.log) return "";
    // `result.log` comes from an agent's results line — contain it to the
    // agent's own triggers dir so a stray/hostile "../" or absolute path
    // cannot make integrations read arbitrary files and relay them onwards.
    const base = path.resolve(agent.triggersDir);
    const resolved = path.resolve(base, result.log);
    if (resolved !== base && !resolved.startsWith(base + path.sep)) {
      log.warn(`Refusing to read log outside triggers dir for "${result.id}": ${result.log}`);
      return "";
    }
    try {
      return (await fs.readFile(resolved, "utf8")).trim();
    } catch (err) {
      log.warn(`Could not read log ${result.log} for "${result.id}".`, err);
      return "";
    }
  }

  /** Whether the agent's structured result was extracted at all — used to avoid
   * posting raw internal logs as a public GitHub comment when extraction failed. */
  private isProxyAgent(agent: AgentRoute): boolean {
    return agent.name === "proxy-agent";
  }

  private async loadOffset(agent: AgentRoute): Promise<number> {
    try {
      const raw = await fs.readFile(this.offsetFileFor(agent), "utf8");
      const n = Number(raw.trim());
      if (Number.isFinite(n) && n >= 0) return n;
    } catch {
      // No offset yet — skip whatever backlog already exists so a fresh start
      // does not replay old results.
    }
    try {
      const stat = await fs.stat(agent.resultsFile);
      await this.saveOffset(agent, stat.size);
      return stat.size;
    } catch {
      return 0; // No results file yet.
    }
  }

  private saveOffset(agent: AgentRoute, offset: number): Promise<void> {
    return this.atomicWrite(this.offsetFileFor(agent), String(offset));
  }

  private persistPending(): Promise<void> {
    const obj = Object.fromEntries(this.pending);
    return this.atomicWrite(this.pendingFile, JSON.stringify(obj, null, 2));
  }

  /** Writes via a temp file + rename, chained so writes never overlap. */
  private atomicWrite(file: string, content: string): Promise<void> {
    this.writeChain = this.writeChain.then(async () => {
      const tmp = `${file}.tmp`;
      await fs.writeFile(tmp, content, "utf8");
      await fs.rename(tmp, file);
    });
    return this.writeChain;
  }
}

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + `\n\n… (avkortet, ${text.length - max} tegn utelatt)`;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}
