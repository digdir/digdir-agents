import { promises as fs } from "node:fs";
import path from "node:path";
import { createLogger } from "../logger.ts";

const log = createLogger("review-notify");

/**
 * Persists which PRs a review-notify DM has already been sent for, keyed
 * "owner/repo#number" — one notification per PR number, for the lifetime of
 * the state file (issue #115: "ingen dubletter").
 */
export class NotifiedStore {
  readonly file: string;
  private keys = new Set<string>();

  constructor(stateDir: string) {
    this.file = path.join(stateDir, "review-notified.json");
  }

  async load(): Promise<void> {
    try {
      const raw = await fs.readFile(this.file, "utf-8");
      const parsed: unknown = JSON.parse(raw);
      this.keys = new Set(Array.isArray(parsed) ? (parsed as string[]) : []);
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== "ENOENT") {
        log.warn(`Could not read notified-PR state (${this.file}); starting empty.`, err);
      }
      this.keys = new Set();
    }
  }

  has(key: string): boolean {
    return this.keys.has(key);
  }

  async markNotified(key: string): Promise<void> {
    this.keys.add(key);
    await fs.mkdir(path.dirname(this.file), { recursive: true });
    await fs.writeFile(this.file, JSON.stringify([...this.keys], null, 2), "utf-8");
  }
}
