import { loadConfig } from "./config.ts";
import { createLogger } from "./logger.ts";
import { GithubPoller } from "./github/poller.ts";
import { GithubClient } from "./github/client.ts";
import { ReviewNotifier } from "./github/reviewNotifier.ts";
import { SlackConnector } from "./slack/connector.ts";
import { SlackWebDm } from "./slack/reviewDm.ts";
import { AgentQueue } from "./agent/queue.ts";
import { Router } from "./router/router.ts";
import type { ResultPosters } from "./agent/types.ts";

const log = createLogger("main");

async function main(): Promise<void> {
  const config = loadConfig();

  if (!config.github.enabled && !config.slack.enabled) {
    log.warn("Neither GITHUB_ENABLED nor SLACK_ENABLED is true — nothing to do. Exiting.");
    return;
  }

  const abort = new AbortController();
  // First-line router (optional): only meaningful together with the queue.
  const router =
    config.router.enabled && config.agentQueue.enabled
      ? new Router(config.router, config.agentQueue.stateDir)
      : null;
  const queue = config.agentQueue.enabled ? new AgentQueue(config.agentQueue, router) : null;
  if (queue) await queue.init();
  if (router) await router.init();

  // When we both enqueue and post answers back, connectors use a transient
  // "working" reaction that the result watcher clears once the answer arrives.
  const awaitReply = config.agentQueue.enabled && config.agentQueue.postResults;

  const github = config.github.enabled ? new GithubPoller(config.github, queue, awaitReply) : null;
  const slack = config.slack.enabled ? new SlackConnector(config.slack, queue, awaitReply) : null;

  // Slack-DM review notifier (optional — issue #115): independent of the
  // agent queue bridge, so it runs even when AGENT_QUEUE_ENABLED is false.
  const reviewNotifier = config.reviewNotify.enabled
    ? new ReviewNotifier(
        config.reviewNotify,
        new GithubClient(config.github),
        new SlackWebDm(config.slack.botToken),
      )
    : null;

  // Wire connectors into the queue so submit() can short-circuit acks directly.
  if (queue && (github || slack)) {
    queue.setConnectors({ slack: slack ?? undefined, github: github ?? undefined });
  }

  const shutdown = (signal: string) => {
    log.info(`Received ${signal}, shutting down…`);
    abort.abort();
    github?.stop();
    slack?.stop().catch((err) => log.error("Error during Slack shutdown.", err));
    reviewNotifier?.stop();
    // Give in-flight work a brief moment, then force-exit.
    setTimeout(() => process.exit(0), 1000).unref();
  };
  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("SIGTERM", () => shutdown("SIGTERM"));

  // Start connectors concurrently. A failure in one must not stop the other.
  const tasks: Promise<void>[] = [];
  if (github) {
    tasks.push(
      github.start(abort.signal).catch((err) => {
        log.error("GitHub connector stopped with an error.", err);
      }),
    );
  }
  if (slack) {
    tasks.push(
      slack.start().catch((err) => {
        log.error("Slack connector failed to start.", err);
      }),
    );
  }
  if (reviewNotifier) {
    tasks.push(
      reviewNotifier.start(abort.signal).catch((err) => {
        log.error("Review notifier stopped with an error.", err);
      }),
    );
  }

  // Poll proxy-agent's results and post answers back to their origin.
  if (queue && config.agentQueue.postResults) {
    const posters: ResultPosters = {};
    if (slack) posters.slack = (reply, delivery) => slack.deliver(reply, delivery);
    if (github) posters.github = (reply, delivery) => github.deliver(reply, delivery);
    tasks.push(
      queue.startResultWatcher(posters, abort.signal).catch((err) => {
        log.error("Result watcher stopped with an error.", err);
      }),
    );
  }

  log.info("integrations started.");
  await Promise.all(tasks);
}

main().catch((err) => {
  log.error("Fatal error during startup.", err);
  process.exit(1);
});
