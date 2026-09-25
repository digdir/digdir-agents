import type { GithubConfig } from "../config.ts";
import { createLogger } from "../logger.ts";
import type { AgentQueue } from "../agent/queue.ts";
import type { Delivery, ReplyContext } from "../agent/types.ts";
import {
  GithubClient,
  eventIdFor,
  issueNumberFrom,
  reactionsUrlFor,
  type GithubNotification,
} from "./client.ts";

const log = createLogger("github");

export class GithubPoller {
  private readonly config: GithubConfig;
  private readonly client: GithubClient;
  private readonly queue: AgentQueue | null;
  /** True when answers are posted back, so the working reaction can be cleared. */
  private readonly awaitReply: boolean;
  private login = "";
  private lastModified: string | null = null;
  private running = false;
  /** Notifications acted on this session, guarding against duplicate handling. */
  private readonly handled = new Set<string>();

  constructor(config: GithubConfig, queue: AgentQueue | null = null, awaitReply = false) {
    this.config = config;
    this.client = new GithubClient(config);
    this.queue = queue;
    this.awaitReply = awaitReply;
  }

  /**
   * Delivers a result to an issue/PR: either a comment, or a silent ack that
   * just adds the acknowledgement reaction. Either way the "working" reaction
   * is cleared afterwards. Used by the result watcher.
   */
  async deliver(reply: Extract<ReplyContext, { kind: "github" }>, delivery: Delivery): Promise<void> {
    if (delivery.kind === "message") {
      await this.client.addIssueComment(reply.owner, reply.repo, reply.issueNumber, delivery.text);
    } else if (reply.reactionsUrl) {
      // ack: acknowledge with a reaction on the triggering comment/issue.
      await this.client.addReaction(reply.reactionsUrl, this.config.ackReaction);
    }
    if (reply.reactionsUrl && reply.reactionId != null) {
      try {
        await this.client.removeReaction(reply.reactionsUrl, reply.reactionId);
      } catch (err) {
        log.warn("Delivered the result but could not clear the working reaction.", err);
      }
    }
  }

  async start(signal: AbortSignal): Promise<void> {
    this.login = await this.client.getAuthenticatedLogin();
    log.info(`Authenticated as ${this.login}. Polling notifications every ${this.config.pollIntervalSeconds}s (min).`);
    if (this.config.allowedUsers.length === 0) {
      log.warn(
        "GITHUB_ALLOWED_USERS is empty — no GitHub user can trigger agent actions; " +
          "every GitHub work order will be dropped (fail-closed). Set a comma-separated " +
          "list of allowed logins to open the gate.",
      );
    }
    this.running = true;

    while (this.running && !signal.aborted) {
      let waitSeconds = this.config.pollIntervalSeconds;
      try {
        const result = await this.client.listNotifications(this.lastModified);
        this.lastModified = result.lastModified;
        if (result.pollIntervalSeconds && result.pollIntervalSeconds > waitSeconds) {
          waitSeconds = result.pollIntervalSeconds;
        }
        if (!result.notModified && result.notifications.length > 0) {
          log.debug(`Received ${result.notifications.length} notification(s).`);
          for (const n of result.notifications) {
            await this.handle(n);
          }
        }
      } catch (err) {
        log.error("Poll cycle failed; will retry.", err);
      }

      await sleep(waitSeconds * 1000, signal);
    }
  }

  stop(): void {
    this.running = false;
  }

  private async handle(n: GithubNotification): Promise<void> {
    // Dedupe on the event id (per comment / per update) — NOT the notification
    // id, which GitHub keeps stable for the whole thread (issue/PR). Keying on
    // the thread id would silently drop every later interaction with an issue
    // that was already handled once in this session.
    const key = eventIdFor(n);
    if (this.handled.has(key)) {
      // Already acted on this event — the thread can only reappear with the
      // same key when markThreadRead below failed transiently. Retry just the
      // read bookkeeping so the notification stops resurfacing every poll,
      // without redoing the actions themselves.
      try {
        await this.client.markThreadRead(n.id);
      } catch (err) {
        log.warn(`Could not mark handled thread read on ${n.repository.full_name}; retrying next poll.`, err);
      }
      return;
    }

    const where = `${n.repository.full_name} "${n.subject.title}"`;

    const relevant =
      n.reason === "assign" || n.reason === "mention" || n.reason === "team_mention";
    if (!relevant) {
      log.debug(`Ignoring notification (reason=${n.reason}) on ${where}.`);
      return; // Leave unhandled reasons unread; do not mark as handled.
    }

    // Hard allowlist gate (fail-closed, issue #70): only listed humans may
    // trigger agent actions, so the actor behind every relevant notification
    // must be determined first. For assigns that is the actor on the last
    // `assigned` event; for mentions/team_mentions it is the author of the
    // exact content that would become the prompt (the triggering comment, or
    // the issue/PR body when there is none) — the gate covers what actually
    // gets executed. A failed lookup leaves the actor null, which drops the
    // event below: work orders from an unverifiable actor are never queued.
    let actorLogin: string | null = null;
    try {
      if (n.reason === "assign") {
        const issueNum = issueNumberFrom(n.subject.url);
        if (issueNum != null) {
          actorLogin = await this.client.getLastAssigner(
            n.repository.owner.login,
            n.repository.name,
            issueNum,
            this.login,
          );
        }
      } else {
        const sourceUrl = n.subject.latest_comment_url ?? n.subject.url;
        if (sourceUrl) {
          actorLogin = await this.client.getResourceAuthor(sourceUrl);
        }
      }
    } catch (err) {
      log.warn(`Could not determine actor for ${n.reason} on ${where}; dropping (fail-closed).`, err);
    }

    if (actorLogin?.toLowerCase() === this.login.toLowerCase()) {
      // The bot's own events are noise, not an attack — skip quietly.
      log.debug(`Skipping self-triggered ${n.reason} on ${where}.`);
      this.handled.add(key);
      try {
        await this.client.markThreadRead(n.id);
      } catch (err) {
        log.warn(`Could not mark self-triggered thread read on ${n.repository.full_name}; retrying next poll.`, err);
      }
      return;
    }

    const allowed =
      actorLogin !== null && this.config.allowedUsers.includes(actorLogin.toLowerCase());
    if (!allowed) {
      // Security trail: WARN with actor, repo, issue and reason. No reaction,
      // no queue, no router call — the bot must not signal that it saw the
      // event; the thread is only marked read so it stops resurfacing.
      const issueNum = issueNumberFrom(n.subject.url);
      log.warn(
        `Dropping ${n.reason} on ${n.repository.full_name}#${issueNum ?? "?"} ` +
          `("${n.subject.title}"): actor "${actorLogin ?? "unknown"}" is not in GITHUB_ALLOWED_USERS.`,
      );
      this.handled.add(key);
      try {
        await this.client.markThreadRead(n.id);
      } catch (err) {
        log.warn(`Could not mark dropped thread read on ${n.repository.full_name}; retrying next poll.`, err);
      }
      return;
    }

    try {
      // Mentions and assignments are both work orders: a human assigning the
      // bot to an issue means "put the agent on this". The assignment stays
      // in place — the bot account owns the issue until it is resolved.
      await this.handleWorkOrder(n, where);

      this.handled.add(key);
      await this.client.markThreadRead(n.id);
    } catch (err) {
      log.error(`Failed to handle notification (reason=${n.reason}) on ${where}.`, err);
    }
  }

  private async handleWorkOrder(n: GithubNotification, where: string): Promise<void> {
    // When we will post an answer back, react with a transient "working"
    // reaction that gets cleared once answered; otherwise leave a persistent
    // acknowledgement reaction.
    const willClear = this.queue !== null && this.awaitReply;
    const reactionName = willClear ? this.config.workingReaction : this.config.reaction;

    const reactionsUrl = reactionsUrlFor(n);
    let reactionId: number | null = null;
    if (reactionsUrl) {
      reactionId = await this.client.addReaction(reactionsUrl, reactionName);
      log.info(`Reacted :${reactionName}: to ${n.reason} on ${where}.`);
    } else {
      log.warn(`Notified (${n.reason}) on ${where} but could not resolve a reaction target.`);
    }

    // Hand the work order off to the agent (if the queue bridge is enabled).
    // Pass the reaction target only when we intend to clear it after answering.
    if (this.queue) {
      await this.enqueueWorkOrder(
        n,
        where,
        willClear ? reactionsUrl : null,
        willClear ? reactionId : null,
      );
    }
  }

  private async enqueueWorkOrder(
    n: GithubNotification,
    where: string,
    reactionsUrl: string | null,
    reactionId: number | null,
  ): Promise<void> {
    if (!n.subject.url) return;
    const issueNumber = issueNumberFrom(n.subject.url);
    if (issueNumber === null) {
      log.warn(`Notified (${n.reason}) on ${where} but could not resolve issue number; not queued.`);
      return;
    }

    // Prefer the triggering comment's text; fall back to the issue/PR body.
    const sourceUrl = n.subject.latest_comment_url ?? n.subject.url;
    let text = "";
    try {
      text = await this.client.getBody(sourceUrl);
    } catch (err) {
      log.warn(`Could not fetch comment/body for ${where}; queuing title only.`, err);
    }
    const prompt = [n.subject.title, text].filter(Boolean).join("\n\n");

    const reply: ReplyContext = {
      kind: "github",
      owner: n.repository.owner.login,
      repo: n.repository.name,
      issueNumber,
      ...(reactionsUrl && reactionId != null ? { reactionsUrl, reactionId } : {}),
    };
    await this.queue!.submit(
      {
        id: eventIdFor(n),
        source: "github",
        type: n.reason,
        received_at: new Date().toISOString(),
        prompt,
        payload: {
          repo: n.repository.full_name,
          issue: issueNumber,
          subject_type: n.subject.type,
          subject_url: n.subject.url,
          comment_url: n.subject.latest_comment_url,
        },
      },
      reply,
    );
  }
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
