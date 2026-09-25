import type { ReviewNotifyConfig } from "../config.ts";
import { createLogger } from "../logger.ts";
import type { CodeownersRule } from "./codeowners.ts";
import type { PullRequestCandidate } from "./client.ts";
import { isOwned } from "./codeowners.ts";
import { NotifiedStore } from "./notifiedStore.ts";

const log = createLogger("review-notify");

/** What the notifier needs from Slack: a DM channel, and a way to post into it. */
export interface SlackDm {
  /** Opens (or reuses) a 1:1 DM with the given user and returns its channel id. */
  openDm(userId: string): Promise<string>;
  postMessage(channel: string, text: string): Promise<void>;
}

/** What the notifier needs from GitHub — {@link GithubClient} satisfies this structurally. */
export interface ReviewGithubClient {
  listOpenPullRequestCandidates(owner: string, repo: string): Promise<PullRequestCandidate[]>;
  listPullRequestFiles(owner: string, repo: string, number: number): Promise<string[]>;
  getCodeowners(owner: string, repo: string): Promise<CodeownersRule[] | null>;
}

/**
 * Selects which open, non-draft, review-required PRs should get a DM.
 * A PR carrying the `auto-merge` label is skipped UNLESS it also touches a
 * CODEOWNERS-owned path — such a PR merges on its own once CI is green (see
 * doc/pr-prosess.md), so notifying a human would be noise; but branch
 * protection still blocks a CODEOWNERS-path PR on human review regardless of
 * the label, so that one still needs a human notified.
 */
export function selectForNotification(
  candidates: PullRequestCandidate[],
  isCodeownersBlocked: (c: PullRequestCandidate) => boolean,
): PullRequestCandidate[] {
  return candidates.filter((c) => {
    if (c.isDraft) return false;
    if (c.reviewDecision !== "REVIEW_REQUIRED") return false;
    if (c.labels.includes("auto-merge") && !isCodeownersBlocked(c)) return false;
    return true;
  });
}

/** Dedupe key: one notification per PR number (issue #115). */
export function keyFor(c: Pick<PullRequestCandidate, "owner" | "repo" | "number">): string {
  return `${c.owner}/${c.repo}#${c.number}`;
}

export function formatMessage(c: PullRequestCandidate): string {
  return `:eyes: PR venter på review: *${c.title}* (av ${c.author})\n${c.url}`;
}

export class ReviewNotifier {
  private readonly config: ReviewNotifyConfig;
  private readonly client: ReviewGithubClient;
  private readonly slack: SlackDm;
  private readonly store: NotifiedStore;
  private running = false;
  private dmChannel: string | null = null;
  private storeLoaded = false;

  constructor(config: ReviewNotifyConfig, client: ReviewGithubClient, slack: SlackDm) {
    this.config = config;
    this.client = client;
    this.slack = slack;
    this.store = new NotifiedStore(config.stateDir);
  }

  async start(signal: AbortSignal): Promise<void> {
    log.info(
      `Watching ${this.config.repos.length} repo(s) for PRs awaiting human review, every ${this.config.pollIntervalSeconds}s.`,
    );
    this.running = true;
    while (this.running && !signal.aborted) {
      await this.pollOnce();
      await sleep(this.config.pollIntervalSeconds * 1000, signal);
    }
  }

  stop(): void {
    this.running = false;
  }

  /**
   * Runs one poll cycle across all watched repos. Loads the dedupe state on
   * first use; if that state cannot be read, the whole cycle is skipped rather
   * than run blind — notifying from an empty set would re-DM every PR already
   * handled (issue #115: "ingen dubletter"). The next cycle retries the load.
   */
  private async pollOnce(): Promise<void> {
    if (!this.storeLoaded) {
      try {
        await this.store.load();
      } catch (err) {
        log.error(
          `Could not load review-notify state (${this.store.file}); deferring all notifications ` +
            `this cycle rather than risking duplicate DMs. Fix or remove the file to resume.`,
          err,
        );
        return;
      }
      this.storeLoaded = true;
    }
    for (const full of this.config.repos) {
      const [owner, repo] = full.split("/");
      try {
        await this.pollRepo(owner, repo);
      } catch (err) {
        // A single repo's failure (rate limit, transient network error, repo
        // renamed) must not stop the others or crash the poll loop.
        log.warn(`Poll cycle for ${full} failed; will retry next cycle.`, err);
      }
    }
  }

  private async pollRepo(owner: string, repo: string): Promise<void> {
    const candidates = await this.client.listOpenPullRequestCandidates(owner, repo);

    // The CODEOWNERS check needs two extra API calls per PR, so only run it
    // for the PRs it can actually change the outcome for.
    const blocked = new Map<number, boolean>();
    const autoMergeCandidates = candidates.filter(
      (c) => !c.isDraft && c.reviewDecision === "REVIEW_REQUIRED" && c.labels.includes("auto-merge"),
    );
    if (autoMergeCandidates.length > 0) {
      const codeowners = await this.client.getCodeowners(owner, repo);
      for (const c of autoMergeCandidates) {
        if (!codeowners) {
          blocked.set(c.number, false);
          continue;
        }
        const files = await this.client.listPullRequestFiles(owner, repo, c.number);
        blocked.set(c.number, files.some((f) => isOwned(codeowners, f)));
      }
    }

    const selected = selectForNotification(candidates, (c) => blocked.get(c.number) ?? false);
    for (const c of selected) {
      const key = keyFor(c);
      if (this.store.has(key)) continue;
      await this.deliver(c, key);
    }
  }

  /**
   * Sends one DM under the store's two-phase protocol, so a disk failure can
   * never turn into a duplicate DM:
   *
   * - marker write fails → nothing is sent (fail closed), retried next cycle;
   * - DM fails → marker is rolled back, retried next cycle;
   * - DM succeeds but the confirming write fails → the durable pending marker
   *   already suppresses a second DM, so the PR is not re-notified.
   *
   * Every failure is logged and swallowed: a broken DM must never take down the
   * poll loop (issue #115 requirement).
   */
  private async deliver(c: PullRequestCandidate, key: string): Promise<void> {
    try {
      await this.store.markPending(key);
    } catch (err) {
      log.warn(
        `Could not persist review-notify intent for ${key}; skipping the DM this cycle to ` +
          `avoid an unrecorded (and later duplicated) notification.`,
        err,
      );
      return;
    }

    try {
      await this.notify(c);
    } catch (err) {
      log.warn(`Could not send review-notify DM for ${key}; will retry next poll.`, err);
      try {
        await this.store.clearPending(key);
      } catch (clearErr) {
        // The marker outlives the failed send: the PR stays suppressed rather
        // than risking a duplicate. Loud, because a DM was genuinely lost.
        log.error(
          `Review-notify DM for ${key} failed and its pending marker could not be cleared; ` +
            `the PR will not be retried. Notify the reviewer manually.`,
          clearErr,
        );
      }
      return;
    }

    try {
      await this.store.markNotified(key);
    } catch (err) {
      log.error(
        `Review-notify DM for ${key} was delivered, but recording it failed; the pending ` +
          `marker keeps it from being sent twice.`,
        err,
      );
    }
  }

  private async notify(c: PullRequestCandidate): Promise<void> {
    try {
      if (!this.dmChannel) {
        this.dmChannel = await this.slack.openDm(this.config.notifyUserId);
      }
      await this.slack.postMessage(this.dmChannel, formatMessage(c));
    } catch (err) {
      // Drop the cached channel so a stale/invalid id is not reused forever.
      this.dmChannel = null;
      throw err;
    }
    log.info(`Notified ${this.config.notifyUserId} about ${keyFor(c)} awaiting review.`);
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
