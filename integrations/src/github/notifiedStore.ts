import { promises as fs } from "node:fs";
import path from "node:path";
import { createLogger } from "../logger.ts";

const log = createLogger("review-notify");

/** On-disk shape. A bare string array is the legacy (v1) format: all notified. */
interface StoredState {
  version: 2;
  /** PRs whose DM is confirmed sent. */
  notified: string[];
  /** PRs whose DM was started but never confirmed — see {@link NotifiedStore}. */
  pending: string[];
}

/**
 * Persists which PRs a review-notify DM has been sent for, keyed
 * "owner/repo#number" — one notification per PR number, for the lifetime of
 * the state file (issue #115: "ingen dubletter").
 *
 * Delivery uses a two-phase protocol so that a *persistence* failure can never
 * be mistaken for a *delivery* failure:
 *
 * 1. {@link markPending} writes an intent marker **before** the DM is sent. If
 *    that write fails, the DM is never attempted (fail closed) — no send, no
 *    duplicate.
 * 2. {@link markNotified} promotes the marker after the DM succeeds. If *this*
 *    write fails, the pending marker is already durable, so {@link has} keeps
 *    reporting the PR as handled and no second DM goes out.
 * 3. {@link clearPending} removes the marker when the DM itself failed, so the
 *    PR is retried on the next cycle.
 *
 * A marker left pending by a crash mid-send is deliberately treated as handled:
 * whether Slack received the message is unknowable, and a missed DM is a lesser
 * fault than a duplicate one. Such leftovers are logged at load.
 */
export class NotifiedStore {
  readonly file: string;
  private notified = new Set<string>();
  private pending = new Set<string>();

  constructor(stateDir: string) {
    this.file = path.join(stateDir, "review-notified.json");
  }

  /**
   * Reads the state file into memory. A missing file is a normal cold start.
   * Any *other* failure (unreadable file, malformed JSON, unexpected shape)
   * throws: silently starting empty would re-DM every PR ever notified.
   */
  async load(): Promise<void> {
    let raw: string;
    try {
      raw = await fs.readFile(this.file, "utf-8");
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") {
        this.notified = new Set();
        this.pending = new Set();
        return;
      }
      throw new Error(`Could not read notified-PR state (${this.file})`, { cause: err });
    }

    const state = parseState(raw, this.file);
    this.notified = new Set(state.notified);
    this.pending = new Set(state.pending);

    if (this.pending.size > 0) {
      log.warn(
        `${this.pending.size} review-notify DM(s) were interrupted mid-send and are treated as ` +
          `already delivered (no duplicate will be sent): ${[...this.pending].join(", ")}`,
      );
    }
  }

  /** True once a DM for this PR has been sent, or started and left unconfirmed. */
  has(key: string): boolean {
    return this.notified.has(key) || this.pending.has(key);
  }

  /** Phase 1: durably record the intent to send. Throws if it cannot be persisted. */
  async markPending(key: string): Promise<void> {
    if (this.notified.has(key) || this.pending.has(key)) return;
    this.pending.add(key);
    try {
      await this.persist();
    } catch (err) {
      // Nothing was sent, so drop the in-memory marker too and let the caller
      // skip this PR — it is retried once the disk is writable again.
      this.pending.delete(key);
      throw err;
    }
  }

  /** Phase 2: the DM went out. A failure here is safe — the pending marker holds. */
  async markNotified(key: string): Promise<void> {
    this.pending.delete(key);
    this.notified.add(key);
    await this.persist();
  }

  /** Rolls back phase 1 after a failed DM so the PR is retried next cycle. */
  async clearPending(key: string): Promise<void> {
    if (!this.pending.delete(key)) return;
    await this.persist();
  }

  /**
   * Writes the state via a temp file + rename, so an interrupted write leaves
   * the previous state intact instead of a truncated file the next load would
   * (correctly) refuse to parse.
   */
  private async persist(): Promise<void> {
    const state: StoredState = {
      version: 2,
      notified: [...this.notified],
      pending: [...this.pending],
    };
    await fs.mkdir(path.dirname(this.file), { recursive: true });
    const tmp = `${this.file}.tmp`;
    await fs.writeFile(tmp, JSON.stringify(state, null, 2), "utf-8");
    await fs.rename(tmp, this.file);
  }
}

/** Parses either the v2 object form or the legacy v1 string array. Throws on anything else. */
function parseState(raw: string, file: string): { notified: string[]; pending: string[] } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`Malformed notified-PR state (${file}): not valid JSON`, { cause: err });
  }

  // v1: a plain array of notified keys.
  if (Array.isArray(parsed)) {
    if (!parsed.every((k) => typeof k === "string")) {
      throw new Error(`Malformed notified-PR state (${file}): expected an array of strings`);
    }
    return { notified: parsed as string[], pending: [] };
  }

  if (parsed && typeof parsed === "object") {
    const { notified, pending } = parsed as { notified?: unknown; pending?: unknown };
    if (isStringArray(notified) && (pending === undefined || isStringArray(pending))) {
      return { notified, pending: pending ?? [] };
    }
  }

  throw new Error(`Malformed notified-PR state (${file}): unexpected shape`);
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((v) => typeof v === "string");
}
