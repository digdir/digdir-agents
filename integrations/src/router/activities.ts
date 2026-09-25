import { promises as fs } from "node:fs";
import path from "node:path";
import { createLogger } from "../logger.ts";

const log = createLogger("router-index");

/** One open activity: a Slack thread or a GitHub issue/PR the bot is part of. */
export interface Activity {
  key: string;
  kind: "slack-thread" | "github-issue";
  /** Reference back to the origin (channel/thread_ts or repo/issue). */
  ref: Record<string, unknown>;
  /** Truncated title/first message the embedding was computed from. */
  text: string;
  embedding: number[];
  updated_at: string;
}

/** A match annotated onto an event: references only, never the text. */
export interface ActivityMatch {
  key: string;
  kind: Activity["kind"];
  ref: Record<string, unknown>;
  score: number;
}

/** Upper bound on indexed activities; the oldest (by updated_at) are dropped. */
const MAX_ENTRIES = 200;

/**
 * Small persisted index of open activities, each with an embedding of its
 * title/first message. Lives as one JSON file in the state dir (the
 * `integrations-state` volume in Docker), so it survives restarts.
 */
export class ActivityIndex {
  private entries = new Map<string, Activity>();
  private readonly file: string;
  /** Serializes writes so overlapping saves cannot corrupt the file. */
  private writeChain: Promise<void> = Promise.resolve();

  constructor(stateDir: string) {
    this.file = path.join(stateDir, "router-activities.json");
  }

  async init(): Promise<void> {
    try {
      const raw = await fs.readFile(this.file, "utf8");
      const arr = JSON.parse(raw) as unknown;
      if (Array.isArray(arr)) {
        for (const a of arr as Activity[]) {
          if (a && typeof a.key === "string" && Array.isArray(a.embedding)) this.entries.set(a.key, a);
        }
      }
      log.info(`Loaded ${this.entries.size} activities from ${this.file}.`);
    } catch {
      // No index yet — start empty.
    }
  }

  has(key: string): boolean {
    return this.entries.has(key);
  }

  /** Adds an activity and trims the index to the newest MAX_ENTRIES. */
  async upsert(activity: Activity): Promise<void> {
    this.entries.set(activity.key, activity);
    if (this.entries.size > MAX_ENTRIES) {
      const oldestFirst = [...this.entries.values()].sort((a, b) => a.updated_at.localeCompare(b.updated_at));
      for (const drop of oldestFirst.slice(0, this.entries.size - MAX_ENTRIES)) {
        this.entries.delete(drop.key);
      }
    }
    await this.persist();
  }

  /** Refreshes an existing activity's timestamp (keeps its embedding). */
  async touch(key: string): Promise<void> {
    const entry = this.entries.get(key);
    if (!entry) return;
    entry.updated_at = new Date().toISOString();
    await this.persist();
  }

  /** Best matches for a query embedding above the threshold, excluding self. */
  match(embedding: number[], excludeKey: string, threshold: number, max: number): ActivityMatch[] {
    const scored: ActivityMatch[] = [];
    for (const a of this.entries.values()) {
      if (a.key === excludeKey) continue;
      const score = cosine(embedding, a.embedding);
      if (score >= threshold) {
        scored.push({ key: a.key, kind: a.kind, ref: a.ref, score: Number(score.toFixed(4)) });
      }
    }
    return scored.sort((x, y) => y.score - x.score).slice(0, max);
  }

  /** Writes via a temp file + rename, chained so writes never overlap. */
  private persist(): Promise<void> {
    const content = JSON.stringify([...this.entries.values()]);
    this.writeChain = this.writeChain.catch(() => {}).then(async () => {
      const tmp = `${this.file}.tmp`;
      await fs.writeFile(tmp, content, "utf8");
      await fs.rename(tmp, this.file);
    });
    return this.writeChain;
  }
}

function cosine(a: number[], b: number[]): number {
  if (a.length === 0 || a.length !== b.length) return 0;
  let dot = 0;
  let normA = 0;
  let normB = 0;
  for (let i = 0; i < a.length; i++) {
    dot += a[i] * b[i];
    normA += a[i] * a[i];
    normB += b[i] * b[i];
  }
  const denom = Math.sqrt(normA) * Math.sqrt(normB);
  return denom === 0 ? 0 : dot / denom;
}
