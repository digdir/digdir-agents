import type { NvtDriver } from "./nvt/driver.ts";
import { fallbackReply, renderPrompt } from "./prompt.ts";
import { TopicScheduler } from "./scheduler.ts";
import type { TopicStore } from "./state.ts";
import { instanceNameFor, type InstanceNameOptions } from "./topic.ts";
import { topicFor } from "./topic.ts";
import { pendingEvents, type TriggerFiles } from "./triggers.ts";
import type { QueueEvent } from "./types.ts";

export interface BridgeOptions {
  triggers: TriggerFiles;
  store: TopicStore;
  driver: NvtDriver;
  instanceNaming: InstanceNameOptions;
  maxParallel: number;
  /** Hvor lenge vi venter på `agentdctl signal done`. */
  promptTimeoutMs: number;
  /** Hvor lenge vi venter på at sesjonen blir klar til å ta imot prompten. */
  readyTimeoutMs: number;
  /** Nådefrist for resultatlinja etter `signal done`. */
  resultGraceMs: number;
  /** TTL for `agent-down` av inaktive topics. 0 = av. */
  idleTtlMs: number;
  /** Hvor agentens `triggers/` er mountet inne i instansen. Default `/triggers`. */
  instanceTriggersPath?: string;
  log?: (message: string) => void;
  /** Injiserbar for tester. */
  sleep?: (ms: number) => Promise<void>;
}

/**
 * Broen mellom filkontrakten og levende nvt-instanser.
 *
 * Invariantene, i prioritert rekkefølge:
 *
 * 1. **Hvert dispatchet event ender med en resultatlinje.** Uten den er
 *    eventet evig ubehandlet, og polleren ville dispatche det på nytt i
 *    ring. Dette er samme grunn som at agent-entrypointene har en
 *    EXIT-trap; her er det try/catch rundt hele `handleEvent`.
 * 2. **Aldri en fabrikert suksess.** Broen skriver kun `status:"error"`
 *    selv. En `status:"ok"` kan bare komme fra agenten i instansen.
 * 3. **Serielt per topic** — én levende sesjon og én arbeidskopi per topic.
 */
export class NvtBridge {
  readonly scheduler: TopicScheduler;
  private readonly opts: BridgeOptions;
  private readonly log: (message: string) => void;
  private readonly sleep: (ms: number) => Promise<void>;

  private abortSignal?: AbortSignal;

  constructor(opts: BridgeOptions) {
    this.opts = opts;
    this.log = opts.log ?? (() => {});
    this.sleep = opts.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.scheduler = new TopicScheduler({
      maxParallel: opts.maxParallel,
      handler: (event, topic) => this.handleEvent(event, topic),
      onError: (event, topic, err) =>
        this.log(`topic ${topic}: event ${event.id} feilet uventet: ${describe(err)}`),
    });
  }

  /** Én pollesyklus: finn ubehandlede events, kø dem, rydd inaktive topics. */
  async pollOnce(): Promise<void> {
    const [inbox, doneIds] = await Promise.all([
      this.opts.triggers.readInbox(),
      this.opts.triggers.readResultIds(),
    ]);
    for (const event of pendingEvents(inbox, doneIds, this.scheduler.inFlightIds())) {
      const topic = topicFor(event);
      if (this.scheduler.enqueue(event, topic)) {
        this.log(`topic ${topic}: køet event ${event.id}`);
      }
    }
    this.scheduler.pump();
    await this.sweepIdleTopics();
  }

  /** Poll-løkka. Kjører til `signal` aborteres. */
  async run(pollMs: number, signal?: AbortSignal): Promise<void> {
    this.abortSignal = signal;
    await this.opts.triggers.ensureDirs();
    await this.opts.store.load();
    this.log(
      `bridge startet: poller ${this.opts.triggers.inboxFile} hvert ${Math.round(pollMs / 1000)}s ` +
        `(maks ${this.opts.maxParallel} parallelle topics)`,
    );
    while (!signal?.aborted) {
      try {
        await this.pollOnce();
      } catch (err) {
        // En feilende pollesyklus skal ikke drepe brua.
        this.log(`pollesyklus feilet: ${describe(err)}`);
      }
      await this.sleepUntil(pollMs, signal);
    }
    // Avslutt ryddig: la events som er under arbeid få skrevet resultatlinja
    // si. Ellers ville de stått uten linje, og neste bridge-prosess måtte
    // rapportere dem som ukjent utfall.
    if (this.scheduler.activeTopics() > 0 || this.scheduler.queuedCount() > 0) {
      this.log(
        `venter på ${this.scheduler.activeTopics()} aktive topics før avslutning ` +
          `(${this.scheduler.queuedCount()} i kø)`,
      );
      await this.scheduler.idle();
    }
    this.log("avsluttet");
  }

  /** Som `sleep`, men våkner umiddelbart ved abort. */
  private async sleepUntil(ms: number, signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) return;
    if (!signal) return this.sleep(ms);
    await new Promise<void>((resolve) => {
      const onAbort = () => resolve();
      signal.addEventListener("abort", onAbort, { once: true });
      void this.sleep(ms).then(() => {
        signal.removeEventListener("abort", onAbort);
        resolve();
      });
    });
  }

  /**
   * Behandler ett event: sørg for instans, injiser prompt, vent på at agenten
   * melder seg ferdig, og verifiser at resultatlinja faktisk kom.
   */
  private async handleEvent(event: QueueEvent, topic: string): Promise<void> {
    const started_at = new Date().toISOString();
    const instanceName = instanceNameFor(topic, this.opts.instanceNaming);

    try {
      // Loggingen er best effort og ligger inne i try-en: ingenting utenfor
      // den får stå mellom «vi har tatt eventet» og «det finnes en
      // resultatlinje».
      await this.opts.triggers.appendBridgeLog(
        event.id,
        `dispatch: topic=${topic} instans=${instanceName}`,
      );

      // Var eventet allerede injisert av en tidligere bridge-prosess? Da skal
      // det IKKE promptes på nytt — det ville lagt en andre prompt inn i en
      // levende sesjon som kanskje står midt i arbeidet.
      if (await this.recoverInFlight(event, topic, started_at)) return;

      await this.opts.store.upsert(topic, instanceName);
      const record = this.opts.store.get(topic);
      // State er sannheten når den finnes: et topic som allerede har en
      // instans skal ikke bytte navn (og dermed workspace) om navnereglene
      // endres mellom kjøringer.
      const instance = record?.instance ?? instanceName;

      const ref = await this.opts.driver.ensureInstance(topic, instance);

      // Klar-sjekk FØR markeringen og prompten (M0-funn 5): en prompt som
      // injiseres i claude-onboardingen forsvinner uten spor — ingen feil,
      // ingen `signal done`, bare en timeout på slutten. Kaster sjekken, er
      // ingen prompt sendt, og eventet skal ikke stå som injisert.
      await this.opts.driver.waitUntilReady(ref, {
        timeoutMs: this.opts.readyTimeoutMs,
        signal: this.abortSignal,
      });
      this.log(`topic ${topic}: instans ${instance} klar, injiserer event ${event.id}`);

      // Markeres FØR prompten sendes: krasjer bridgen mellom disse to, må
      // gjenopptakelsen anta at prompten kan ha nådd sesjonen.
      await this.opts.store.markPrompted(topic, event.id);
      await this.opts.driver.sendPrompt(
        ref,
        renderPrompt(event, topic, this.opts.instanceTriggersPath ?? "/triggers"),
      );
      await this.opts.triggers.appendBridgeLog(event.id, "prompt injisert, venter på signal done");

      const outcome = await this.opts.driver.waitForDone(ref, {
        timeoutMs: this.opts.promptTimeoutMs,
        signal: this.abortSignal,
      });

      // Agenten skriver resultatlinja selv. Etter `signal done` gir vi den en
      // nådefrist; ved timeout er det ingen grunn til å vente mer, men vi
      // sjekker én gang — agenten kan ha levert uten å signalisere.
      const graceMs = outcome.kind === "done" ? this.opts.resultGraceMs : 0;
      await this.opts.triggers.appendBridgeLog(
        event.id,
        outcome.kind === "done"
          ? `signal done mottatt ${outcome.at}, venter opptil ${graceMs}ms på resultatlinja`
          : `ingen signal done innen ${outcome.waitedMs}ms`,
      );

      if (await this.awaitResultLine(event.id, graceMs)) {
        this.log(`topic ${topic}: event ${event.id} kvittert ut av agenten`);
        await this.opts.store.markSettled(topic, event.id);
        await this.opts.triggers.appendBridgeLog(event.id, "resultatlinje fra agenten funnet");
        return;
      }

      const reason = outcome.kind === "done" ? "done-without-result" : "timeout";
      await this.writeFallback(event, topic, instance, started_at, reason);
    } catch (err) {
      // Interne feil (instans nede, make/docker feilet) skal også ende i en
      // resultatlinje — ellers spinner polleren på eventet for alltid. Derfor
      // skrives linja FØRST her; logging og state-rydding er best effort etterpå.
      let already = false;
      try {
        already = await this.opts.triggers.hasResult(event.id);
      } catch {
        // Kan vi ikke lese resultatfila, heller en mulig dublett enn et tapt svar.
      }
      if (!already) {
        await this.opts.triggers.appendResult({
          id: event.id,
          status: "error",
          exit_code: 1,
          log: this.opts.triggers.bridgeLogRelPath(event.id),
          intent: "action",
          reply:
            `nvt-broen fikk ikke levert oppgaven til en agentinstans: ${describe(err)}. ` +
            `Ingen leveranse er bekreftet. Topic \`${topic}\`.`,
          started_at,
          finished_at: new Date().toISOString(),
        });
      }
      await this.settleQuietly(topic, event.id);
      await this.opts.triggers.appendBridgeLog(event.id, `intern feil: ${describe(err)}`);
    }
  }

  /**
   * Gjenopptakelse etter omstart: et event som state sier allerede er injisert
   * skal ikke promptes på nytt. Vi venter nådefristen på at agenten (som kan
   * være midt i arbeidet i en fortsatt levende sesjon) skriver resultatlinja,
   * og melder ellers ærlig at utfallet er ukjent.
   *
   * Returnerer true når eventet er håndtert her.
   */
  private async recoverInFlight(
    event: QueueEvent,
    topic: string,
    started_at: string,
  ): Promise<boolean> {
    const record = this.opts.store.get(topic);
    if (record?.in_flight_event_id !== event.id) return false;

    this.log(
      `topic ${topic}: event ${event.id} var allerede injisert før omstart — prompter ikke på nytt`,
    );
    if (await this.awaitResultLine(event.id, this.opts.resultGraceMs)) {
      await this.opts.store.markSettled(topic, event.id);
      await this.opts.triggers.appendBridgeLog(event.id, "resultatlinje funnet etter omstart");
      return true;
    }
    await this.opts.triggers.appendResult({
      id: event.id,
      status: "error",
      exit_code: 1,
      log: this.opts.triggers.bridgeLogRelPath(event.id),
      intent: "action",
      reply:
        `nvt-broen ble startet på nytt mens denne oppgaven var under arbeid i instansen ` +
        `\`${record.instance}\`. Oppgaven ble ikke sendt inn igjen (det ville lagt en ny ` +
        `prompt inn i en levende sesjon), og broen kan ikke bekrefte utfallet. Sjekk ` +
        `code-server på http://${record.instance}.agent.localhost:4090 — arbeidet kan ` +
        `ligge der. Topic \`${topic}\`.`,
      started_at,
      finished_at: new Date().toISOString(),
    });
    await this.settleQuietly(topic, event.id);
    this.log(`topic ${topic}: event ${event.id} → fallback-resultatlinje (bridge-omstart)`);
    return true;
  }

  private async settleQuietly(topic: string, eventId: string): Promise<void> {
    try {
      await this.opts.store.markSettled(topic, eventId);
    } catch {
      // State er gjenskapbar; en feil her skal ikke velte behandlingen.
    }
  }

  /** Fallback-linja: alltid `status:"error"`, aldri en fabrikert suksess. */
  private async writeFallback(
    event: QueueEvent,
    topic: string,
    instance: string,
    started_at: string,
    reason: "done-without-result" | "timeout",
  ): Promise<void> {
    const reply = fallbackReply(reason, {
      instance,
      topic,
      graceSeconds: Math.round(this.opts.resultGraceMs / 1000),
      timeoutSeconds: Math.round(this.opts.promptTimeoutMs / 1000),
    });
    await this.opts.triggers.appendResult({
      id: event.id,
      status: "error",
      exit_code: 1,
      log: this.opts.triggers.bridgeLogRelPath(event.id),
      intent: "action",
      reply,
      started_at,
      finished_at: new Date().toISOString(),
    });
    await this.settleQuietly(topic, event.id);
    this.log(`topic ${topic}: event ${event.id} → fallback-resultatlinje (${reason})`);
    await this.opts.triggers.appendBridgeLog(event.id, `fallback-resultatlinje skrevet (${reason})`);
  }

  /**
   * Venter opptil `graceMs` på at resultatlinja for `id` dukker opp. Sjekker
   * alltid minst én gang, og rett før fallbacken skrives — det minsker vinduet
   * der både agenten og broen skriver en linje for samme id.
   */
  private async awaitResultLine(id: string, graceMs: number): Promise<boolean> {
    // 1s: hvert forsøk leser hele results.jsonl, så tettere polling koster mer
    // enn den vinner.
    const step = 1000;
    let waited = 0;
    for (;;) {
      if (await this.opts.triggers.hasResult(id)) return true;
      if (waited >= graceMs) return false;
      await this.sleep(Math.min(step, graceMs - waited));
      waited += step;
    }
  }

  /** TTL: `agent-down` for topics uten aktivitet. Workspacet beholdes. */
  private async sweepIdleTopics(): Promise<void> {
    for (const [topic, record] of this.opts.store.idleTopics(this.opts.idleTtlMs)) {
      if (this.scheduler.isBusy(topic)) continue;
      try {
        await this.opts.driver.stopInstance({ topic, instance: record.instance });
        await this.opts.store.markDown(topic);
        this.log(`topic ${topic}: instans ${record.instance} tatt ned (inaktiv), workspace beholdt`);
      } catch (err) {
        this.log(`topic ${topic}: agent-down feilet: ${describe(err)}`);
      }
    }
  }
}

function describe(err: unknown): string {
  const message = err instanceof Error ? err.message : String(err);
  return message.split("\n")[0]?.trim() || "ukjent feil";
}
