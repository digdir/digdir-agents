import { WebClient } from "@slack/web-api";
import type { SlackDm } from "../github/reviewNotifier.ts";

/** Slack-backed {@link SlackDm}: opens a 1:1 DM and posts plain-text messages into it. */
export class SlackWebDm implements SlackDm {
  private readonly web: WebClient;

  constructor(botToken: string) {
    // No SDK-level retries: @slack/web-api retries up to 10 times over ~30
    // minutes by default, and ReviewNotifier awaits every DM — one unhappy
    // call would stall the PRs behind it and the next poll cycle with it.
    // Retrying is the notifier's job: an unmarked PR is picked up again on
    // the next cycle.
    this.web = new WebClient(botToken, { retryConfig: { retries: 0 } });
  }

  /** Opens (or reuses) the 1:1 DM channel with `userId` and returns its channel id. */
  async openDm(userId: string): Promise<string> {
    const res = await this.web.conversations.open({ users: userId });
    const channel = res.channel?.id;
    if (!channel) {
      throw new Error(`conversations.open did not return a channel id for user ${userId}`);
    }
    return channel;
  }

  /** Posts a plain-text message to an already-open channel. */
  async postMessage(channel: string, text: string): Promise<void> {
    await this.web.chat.postMessage({ channel, text });
  }
}
