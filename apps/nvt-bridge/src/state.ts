import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import type { TopicRecord, TopicsState } from "./types.ts";

/**
 * `state/topics.json` — topic → nvt-instans. Filen er en optimalisering og et
 * revisjonsspor, ikke sannheten: instansnavnet er deterministisk avledet av
 * topicet (`instanceNameFor`), så mister vi filen, finner bridgen samme
 * instans og samme workspace igjen. Det som faktisk tapes er TTL-klokka og
 * prompt-telleren.
 */
export class TopicStore {
  private state: TopicsState = { version: 1, topics: {} };
  /** Serialiserer skrivinger — topics kjører parallelt og deler denne filen. */
  private writeChain: Promise<void> = Promise.resolve();
  private writeSeq = 0;
  readonly stateDir: string;
  readonly file: string;

  constructor(stateDir: string) {
    this.stateDir = stateDir;
    this.file = path.join(stateDir, "topics.json");
  }

  async load(): Promise<void> {
    try {
      const parsed = JSON.parse(await readFile(this.file, "utf8")) as TopicsState;
      if (parsed && typeof parsed === "object" && parsed.topics) {
        this.state = { version: 1, topics: parsed.topics };
      }
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== "ENOENT") {
        // Korrupt state er ikke fatalt — den kan gjenskapes. Start tomt.
        this.state = { version: 1, topics: {} };
      }
    }
  }

  get(topic: string): TopicRecord | undefined {
    return this.state.topics[topic];
  }

  entries(): [string, TopicRecord][] {
    return Object.entries(this.state.topics);
  }

  /** Registrerer instansen for et topic (uendret hvis den finnes). */
  async upsert(topic: string, instance: string): Promise<TopicRecord> {
    const existing = this.state.topics[topic];
    if (existing) return existing;
    const record: TopicRecord = {
      instance,
      created_at: new Date().toISOString(),
      prompts: 0,
      up: true,
    };
    this.state.topics[topic] = record;
    await this.save();
    return record;
  }

  async markPrompted(topic: string, eventId: string): Promise<void> {
    const record = this.state.topics[topic];
    if (!record) return;
    record.last_prompt_at = new Date().toISOString();
    record.last_event_id = eventId;
    record.in_flight_event_id = eventId;
    record.prompts += 1;
    record.up = true;
    await this.save();
  }

  /**
   * Eventet er kvittert ut (av agenten eller av en fallback-linje). Rydder
   * in-flight-markøren, så en senere omstart ikke tror det står ubehandlet.
   */
  async markSettled(topic: string, eventId: string): Promise<void> {
    const record = this.state.topics[topic];
    if (!record || record.in_flight_event_id !== eventId) return;
    delete record.in_flight_event_id;
    await this.save();
  }

  async markDown(topic: string): Promise<void> {
    const record = this.state.topics[topic];
    if (!record) return;
    record.up = false;
    await this.save();
  }

  /**
   * Topics som ikke har fått en prompt på `ttlMs` og som vi tror er oppe.
   * `ttlMs <= 0` slår av TTL-nedtaking.
   */
  idleTopics(ttlMs: number, now = Date.now()): [string, TopicRecord][] {
    if (ttlMs <= 0) return [];
    return this.entries().filter(([, r]) => {
      if (!r.up) return false;
      const last = Date.parse(r.last_prompt_at ?? r.created_at);
      return Number.isFinite(last) && now - last >= ttlMs;
    });
  }

  /**
   * Atomisk skriving: skriv til temp, så rename over.
   *
   * To ting er nødvendige fordi topics behandles parallelt og deler denne
   * filen: skrivingene serialiseres i en kjede, og hver skriving får sitt eget
   * temp-filnavn. Uten begge kan to samtidige `save()` bruke samme temp-fil,
   * og den andre `rename` feiler med ENOENT — som ville boblet opp som en
   * «intern feil»-resultatlinje for et topic som egentlig gikk bra.
   */
  private save(): Promise<void> {
    const run = async (): Promise<void> => {
      await mkdir(this.stateDir, { recursive: true });
      const tmp = `${this.file}.${process.pid}.${this.writeSeq++}.tmp`;
      await writeFile(tmp, JSON.stringify(this.state, null, 2) + "\n", "utf8");
      await rename(tmp, this.file);
    };
    // `then(run, run)` — en tidligere feilet skriving skal ikke blokkere de neste.
    this.writeChain = this.writeChain.then(run, run);
    return this.writeChain;
  }
}
