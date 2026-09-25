import { SocketModeClient } from "@slack/socket-mode";
import { WebClient } from "@slack/web-api";
import type { SlackConfig } from "../config.ts";
import { createLogger } from "../logger.ts";
import type { AgentQueue } from "../agent/queue.ts";
import type { Delivery, ReplyContext } from "../agent/types.ts";

const log = createLogger("slack");

/** Shape of the fields we read off an incoming Slack `message` event. */
interface SlackMessageEvent {
  type: string;
  subtype?: string;
  channel?: string;
  /** "im" for a 1:1 DM, "channel"/"group"/"mpim" otherwise. */
  channel_type?: string;
  ts?: string;
  thread_ts?: string;
  user?: string;
  bot_id?: string;
  text?: string;
}

/** Shape of the fields we read off an incoming `reaction_added` event. */
interface SlackReactionEvent {
  type: string;
  /** The user who added the reaction. */
  user?: string;
  /** The emoji name (no colons), e.g. "thumbsup". */
  reaction?: string;
  /** Author of the message that was reacted to. */
  item_user?: string;
  /** The reacted-to item; for messages it carries the channel and ts. */
  item?: { type?: string; channel?: string; ts?: string };
}

export class SlackConnector {
  private readonly config: SlackConfig;
  private readonly socket: SocketModeClient;
  private readonly web: WebClient;
  private readonly queue: AgentQueue | null;
  /** True when answers are posted back, so the working reaction can be cleared. */
  private readonly awaitReply: boolean;
  private selfUserId: string | undefined;

  constructor(config: SlackConfig, queue: AgentQueue | null = null, awaitReply = false) {
    this.config = config;
    this.socket = new SocketModeClient({ appToken: config.appToken });
    this.web = new WebClient(config.botToken);
    this.queue = queue;
    this.awaitReply = awaitReply;
  }

  /**
   * Delivers a result: either a threaded reply, or a silent ack that just adds
   * the acknowledgement reaction to the original message. The "working"
   * reaction is **always** cleared afterwards — also when posting failed: a
   * lingering :thinking_face: signals "still working" and is worse than a
   * missing ack reaction. Used by the result watcher.
   */
  async deliver(reply: Extract<ReplyContext, { kind: "slack" }>, delivery: Delivery): Promise<void> {
    try {
      if (delivery.kind === "message") {
        await this.web.chat.postMessage({
          channel: reply.channel,
          thread_ts: reply.threadTs,
          text: delivery.text,
        });
      } else if (reply.messageTs) {
        // ack: acknowledge with a reaction on the original message. Best
        // effort — a failed ack reaction must not block the cleanup below.
        try {
          await this.addReaction(reply.channel, reply.messageTs, this.config.ackReaction);
        } catch (err) {
          const code = (err as { data?: { error?: string } })?.data?.error;
          log.warn(
            `Could not add the ack reaction :${this.config.ackReaction}: in ${reply.channel} @ ${reply.messageTs} (${code ?? "unknown error"}).`,
            err,
          );
        }
      }
    } finally {
      await this.clearWorkingReaction(reply);
    }
  }

  /**
   * Removes the transient "working" reaction from a message, if one was set.
   * Logs every attempt and outcome — a reaction that silently stays behind
   * looks like the bot is still thinking (see issue #46). Never throws.
   */
  private async clearWorkingReaction(reply: Extract<ReplyContext, { kind: "slack" }>): Promise<void> {
    if (!reply.workingReaction || !reply.messageTs) {
      log.debug(`No working reaction to clear in ${reply.channel} (workingReaction/messageTs not set on the reply context).`);
      return;
    }
    try {
      await this.web.reactions.remove({
        channel: reply.channel,
        timestamp: reply.messageTs,
        name: reply.workingReaction,
      });
      log.info(`Cleared working reaction :${reply.workingReaction}: in ${reply.channel} @ ${reply.messageTs}.`);
    } catch (err) {
      const code = (err as { data?: { error?: string } })?.data?.error;
      if (code === "no_reaction") {
        // Nothing to remove (never added, or already removed) — worth a trace,
        // not a warning.
        log.info(`Working reaction :${reply.workingReaction}: in ${reply.channel} @ ${reply.messageTs} was already gone.`);
        return;
      }
      log.warn(
        `Could not clear working reaction :${reply.workingReaction}: in ${reply.channel} @ ${reply.messageTs} (${code ?? "unknown error"}).`,
        err,
      );
    }
  }

  /** Adds a reaction, treating an already-present reaction as success. */
  private async addReaction(channel: string, timestamp: string, name: string): Promise<void> {
    try {
      await this.web.reactions.add({ channel, timestamp, name });
      log.info(`Reacted :${name}: in ${channel} @ ${timestamp}.`);
    } catch (err) {
      const code = (err as { data?: { error?: string } })?.data?.error;
      if (code !== "already_reacted") throw err;
    }
  }

  async start(): Promise<void> {
    const auth = await this.web.auth.test();
    this.selfUserId = auth.user_id as string | undefined;
    log.info(`Authenticated as ${auth.user} (${this.selfUserId}) in ${auth.team}.`);

    this.socket.on("message", async ({ event, ack }: { event: SlackMessageEvent; ack: () => Promise<void> }) => {
      // Always acknowledge quickly so Slack does not redeliver.
      await ack();
      await this.onMessage(event).catch((err) => log.error("Failed to react to message.", err));
    });

    this.socket.on("reaction_added", async ({ event, ack }: { event: SlackReactionEvent; ack: () => Promise<void> }) => {
      await ack();
      await this.onReaction(event).catch((err) => log.error("Failed to handle reaction.", err));
    });

    await this.socket.start();
    await this.setPresence("auto");
    log.info(`Connected via Socket Mode. Reacting with :${this.config.reaction}: to new messages.`);
  }

  async stop(): Promise<void> {
    await this.setPresence("away");
    await this.socket.disconnect();
  }

  /**
   * Sets the bot user's presence: "auto" shows the green/active dot, "away"
   * shows the hollow away dot. Requires the `users:write` scope; a missing
   * scope is logged as a warning rather than treated as fatal.
   */
  private async setPresence(presence: "auto" | "away"): Promise<void> {
    try {
      await this.web.users.setPresence({ presence });
      log.info(`Set presence to ${presence === "auto" ? "active" : "away"}.`);
    } catch (err) {
      const code = (err as { data?: { error?: string } })?.data?.error;
      log.warn(`Could not set presence to ${presence} (${code ?? "unknown error"}). Add the users:write scope.`);
    }
  }

  private async onMessage(event: SlackMessageEvent): Promise<void> {
    // Only react to plain, user-authored top-level messages. Skip edits/joins/
    // deletes (subtypes), other bots, and our own messages to avoid loops.
    if (event.subtype) {
      log.debug(`Skipping message with subtype=${event.subtype}.`);
      return;
    }
    if (event.bot_id || (this.selfUserId && event.user === this.selfUserId)) {
      log.debug("Skipping bot / self message.");
      return;
    }
    if (!event.channel || !event.ts) {
      log.debug("Skipping message without channel/ts.");
      return;
    }
    // In a 1:1 DM (channel_type "im") every message is directed at the bot, so
    // react to all of them. Everywhere else (channels, private groups, group
    // DMs, threads) only react when the bot is explicitly @-mentioned
    // (a direct <@BOT_ID> mention, not @channel/@here or usergroup mentions).
    if (event.channel_type !== "im") {
      if (!this.selfUserId || !event.text?.includes(`<@${this.selfUserId}>`)) {
        log.debug("Skipping non-DM message that does not mention the bot.");
        return;
      }
    }

    // When we will post an answer back, react with a transient "working"
    // reaction that gets cleared once answered; otherwise leave a persistent
    // acknowledgement reaction.
    const willClear = this.queue !== null && this.awaitReply;
    const reactionName = willClear ? this.config.workingReaction : this.config.reaction;
    await this.addReaction(event.channel, event.ts, reactionName);

    // Hand the message off to the agent (if the queue bridge is enabled).
    if (this.queue) await this.enqueueMessage(event, willClear ? reactionName : undefined);
  }

  private async enqueueMessage(event: SlackMessageEvent, workingReaction?: string): Promise<void> {
    if (!event.channel || !event.ts) return;
    // Strip the bot mention so the prompt is just the user's request.
    const prompt = (event.text ?? "")
      .replace(new RegExp(`<@${this.selfUserId}>`, "g"), "")
      .trim();
    // Reply in the message's thread; a top-level message starts one under itself.
    const threadTs = event.thread_ts ?? event.ts;

    const reply: ReplyContext = {
      kind: "slack",
      channel: event.channel,
      threadTs,
      // Remember what to clear once answered (only when we await a reply).
      ...(workingReaction ? { messageTs: event.ts, workingReaction } : {}),
    };
    await this.queue!.submit(
      {
        id: `slack-${event.channel}-${event.ts}`,
        source: "slack",
        type: event.channel_type === "im" ? "message.im" : "app_mention",
        received_at: new Date().toISOString(),
        prompt,
        payload: {
          channel: event.channel,
          ts: event.ts,
          thread_ts: threadTs,
          user: event.user,
          channel_type: event.channel_type,
        },
      },
      reply,
    );
  }

  /**
   * Handles emoji reactions on messages the bot is involved with: reactions on
   * the bot's own messages, or on messages in threads where the bot
   * participates. The reaction (and the message it sits on) is handed to the
   * agent, which interprets what it means (e.g. a 👍 as a positive-feedback /
   * learning signal). Reactions elsewhere in channels the bot happens to be a
   * member of are ignored (issue #61).
   */
  private async onReaction(event: SlackReactionEvent): Promise<void> {
    // Skip our own reactions — the bot adds thinking_face/ack reactions itself,
    // and acting on those would loop.
    if (this.selfUserId && event.user === this.selfUserId) {
      log.debug("Skipping our own reaction.");
      return;
    }
    if (!event.reaction || !event.item?.channel || !event.item?.ts) {
      log.debug("Skipping reaction without emoji/target.");
      return;
    }

    const channel = event.item.channel;
    const messageTs = event.item.ts;

    // Filter before touching the message: only reactions on the bot's own
    // messages, or in threads the bot already participates in. This must
    // happen before the working reaction below — reacting first is exactly
    // the noisy behavior the filter exists to stop.
    const onOwnMessage = this.selfUserId !== undefined && event.item_user === this.selfUserId;
    const context = await this.fetchThreadContext(channel, messageTs);
    const inOwnThread =
      context !== null && this.selfUserId !== undefined && context.authors.has(this.selfUserId);
    if (!onOwnMessage && !inOwnThread) {
      log.debug(
        `Skipping reaction :${event.reaction}: on ${channel}@${messageTs} — not on our message or in a thread we participate in.`,
      );
      return;
    }

    const by = event.item_user ? ` (message by ${event.item_user})` : "";
    log.info(`Picked up reaction :${event.reaction}: from ${event.user ?? "?"} on ${channel}@${messageTs}${by}.`);

    if (!this.queue) return; // Observe-only when the bridge is off.

    const text = context?.text ?? "";
    const threadTs = context?.threadTs ?? messageTs;

    // Show the working reaction on the reacted-to message while the agent runs.
    const willClear = this.awaitReply;
    const working = willClear ? this.config.workingReaction : this.config.reaction;
    await this.addReaction(channel, messageTs, working);

    const prompt =
      `En Slack-bruker reagerte med :${event.reaction}: på følgende melding:\n\n` +
      `"${text || "(fant ikke meldingsteksten)"}"`;

    const reply: ReplyContext = {
      kind: "slack",
      channel,
      threadTs,
      ...(willClear ? { messageTs, workingReaction: working } : {}),
    };
    await this.queue.submit(
      {
        id: `slack-reaction-${channel}-${messageTs}-${event.user ?? "x"}-${event.reaction}`,
        source: "slack",
        type: "reaction_added",
        received_at: new Date().toISOString(),
        prompt,
        payload: {
          channel,
          ts: messageTs,
          thread_ts: threadTs,
          reaction: event.reaction,
          reactor: event.user,
          message_author: event.item_user,
        },
      },
      reply,
    );
  }

  /**
   * Fetches the reacted-to message plus the thread it lives in. Uses
   * `conversations.replies` rather than `conversations.history`: history only
   * returns top-level messages, so a reacted-to thread reply came back empty
   * ("fant ikke meldingsteksten", issue #61). Returns null when the lookup
   * fails entirely.
   */
  private async fetchThreadContext(
    channel: string,
    ts: string,
  ): Promise<{ text: string; threadTs: string; authors: Set<string> } | null> {
    try {
      let messages = await this.fetchReplies(channel, ts);
      const target = messages.find((m) => m.ts === ts) ?? messages[0];
      const threadTs = target?.thread_ts ?? ts;
      // Given a reply's ts, Slack may return only that reply — re-fetch from
      // the thread root so `authors` reflects the whole conversation.
      if (threadTs !== ts && !messages.some((m) => m.ts === threadTs)) {
        messages = await this.fetchReplies(channel, threadTs);
      }
      const authors = new Set<string>();
      for (const m of messages) if (m.user) authors.add(m.user);
      return { text: target?.text ?? "", threadTs, authors };
    } catch (err) {
      log.warn("Could not fetch the reacted-to message/thread.", err);
      return null;
    }
  }

  private async fetchReplies(
    channel: string,
    ts: string,
  ): Promise<Array<{ ts?: string; user?: string; text?: string; thread_ts?: string }>> {
    const allMessages: Array<{ ts?: string; user?: string; text?: string; thread_ts?: string }> = [];
    let cursor: string | undefined;
    do {
      const res = await this.web.conversations.replies({ channel, ts, limit: 100, cursor });
      const messages = (res.messages ?? []) as Array<{ ts?: string; user?: string; text?: string; thread_ts?: string }>;
      allMessages.push(...messages);
      cursor = res.response_metadata?.next_cursor as string | undefined;
    } while (cursor);
    return allMessages;
  }
}
