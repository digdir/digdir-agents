import type { QueueEvent } from "./types.ts";

export interface SchedulerOptions {
  /** Maks antall topics som behandles samtidig. */
  maxParallel: number;
  /** Behandler ett event. Må ALLTID ende med en resultatlinje (se bridge.ts). */
  handler: (event: QueueEvent, topic: string) => Promise<void>;
  onError?: (event: QueueEvent, topic: string, err: unknown) => void;
}

/**
 * Serielt innen et topic, parallelt på tvers — maks N samtidige topics.
 *
 * Serialiseringen er ikke bare ytelse: alle events i et topic deler samme
 * nvt-instans, samme arbeidskopi og samme levende CLI-sesjon. To prompts inn i
 * samme tmux-sesjon samtidig ville flette seg sammen til én uleselig samtale
 * og to agenter i samme git-arbeidskopi.
 *
 * Køen per topic er FIFO, så oppfølgingsevents kommer inn i den rekkefølgen de
 * ble mottatt.
 */
export class TopicScheduler {
  /** topic → FIFO-kø. Innsettingsrekkefølgen gir rettferdighet på tvers. */
  private readonly queues = new Map<string, QueueEvent[]>();
  /** Topics med en aktiv arbeider akkurat nå. */
  private readonly active = new Set<string>();
  /** Event-id-er som er køet eller under arbeid — dedupe-vakt. */
  private readonly inFlight = new Set<string>();
  private readonly idleWaiters: (() => void)[] = [];
  private readonly opts: SchedulerOptions;

  constructor(opts: SchedulerOptions) {
    this.opts = opts;
  }

  /** Er dette eventet allerede køet eller under arbeid? */
  has(eventId: string): boolean {
    return this.inFlight.has(eventId);
  }

  inFlightIds(): ReadonlySet<string> {
    return this.inFlight;
  }

  activeTopics(): number {
    return this.active.size;
  }

  /** Har topicet en aktiv arbeider eller noe i kø? (TTL-nedtaking må vente.) */
  isBusy(topic: string): boolean {
    return this.active.has(topic) || (this.queues.get(topic)?.length ?? 0) > 0;
  }

  queuedCount(): number {
    let n = 0;
    for (const q of this.queues.values()) n += q.length;
    return n;
  }

  /**
   * Køer et event på topicet sitt. Returnerer false hvis id-en allerede er
   * kjent (idempotent — polleren kan trygt se samme event flere ganger).
   */
  enqueue(event: QueueEvent, topic: string): boolean {
    if (this.inFlight.has(event.id)) return false;
    this.inFlight.add(event.id);
    const queue = this.queues.get(topic);
    if (queue) queue.push(event);
    else this.queues.set(topic, [event]);
    return true;
  }

  /** Starter arbeidere for ventende topics, opp til taket. */
  pump(): void {
    for (const [topic, queue] of this.queues) {
      if (this.active.size >= this.opts.maxParallel) break;
      if (this.active.has(topic) || queue.length === 0) continue;
      this.active.add(topic);
      // `runTopic` fanger selv feil fra handleren; denne catch-en er en siste
      // skanse mot unhandled rejection (f.eks. om `onError` selv kaster).
      void this.runTopic(topic).catch(() => {});
    }
  }

  private async runTopic(topic: string): Promise<void> {
    try {
      for (;;) {
        const queue = this.queues.get(topic);
        const event = queue?.shift();
        if (!event) break;
        try {
          await this.opts.handler(event, topic);
        } catch (err) {
          this.opts.onError?.(event, topic, err);
        } finally {
          // Først nå kan polleren se eventet igjen. Handleren garanterer en
          // resultatlinje, så dedupe-regelen holder det ute derfra.
          this.inFlight.delete(event.id);
        }
      }
    } finally {
      this.active.delete(topic);
      if ((this.queues.get(topic)?.length ?? 0) === 0) this.queues.delete(topic);
      // En frigjort plass kan slippe inn et ventende topic.
      this.pump();
      if (this.active.size === 0 && this.queuedCount() === 0) {
        for (const resolve of this.idleWaiters.splice(0)) resolve();
      }
    }
  }

  /** Resolver når alle køer er tømt og ingen arbeidere er aktive. */
  async idle(): Promise<void> {
    if (this.active.size === 0 && this.queuedCount() === 0) return;
    await new Promise<void>((resolve) => this.idleWaiters.push(resolve));
  }
}
