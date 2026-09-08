import { WebClient } from "@slack/web-api";
import type { SlackDm } from "../github/reviewNotifier.ts";

/** Slack-backed {@link SlackDm}: opens a 1:1 DM and posts plain-text messages into it. */
export class SlackWebDm implements SlackDm {
  private readonly web: WebClient;

  constructor(botToken: string) {
    this.web = new WebClient(botToken);
  }

  async openDm(userId: string): Promise<string> {
    const res = await this.web.conversations.open({ users: userId });
    const channel = res.channel?.id;
    if (!channel) {
      throw new Error(`conversations.open did not return a channel id for user ${userId}`);
    }
    return channel;
  }

  async postMessage(channel: string, text: string): Promise<void> {
    await this.web.chat.postMessage({ channel, text });
  }
}
