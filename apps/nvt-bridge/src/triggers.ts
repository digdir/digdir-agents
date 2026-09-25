import { appendFile, mkdir, open, readFile } from "node:fs/promises";
import path from "node:path";
import { safeId } from "./topic.ts";
import type { QueueEvent, ResultLine } from "./types.ts";

/**
 * Lesing og skriving av filkontrakten. Alt her er bevisst konservativt:
 * integrations leser `results.jsonl` med en byte-offset og stopper ved siste
 * `\n`, så en delvis skrevet linje ville stoppe hele strømmen. Vi skriver
 * derfor alltid én komplett linje i ett `appendFile`-kall (O_APPEND, så to
 * skrivere kan ikke flette seg inn i hverandre), og leser aldri lenger enn
 * til siste linjeskift.
 */
export class TriggerFiles {
  readonly triggersDir: string;
  readonly inboxFile: string;
  readonly resultsFile: string;
  readonly logsDir: string;

  constructor(triggersDir: string) {
    this.triggersDir = triggersDir;
    this.inboxFile = path.join(triggersDir, "inbox.jsonl");
    this.resultsFile = path.join(triggersDir, "results.jsonl");
    this.logsDir = path.join(triggersDir, "logs");
  }

  async ensureDirs(): Promise<void> {
    await mkdir(this.logsDir, { recursive: true });
  }

  /**
   * Alle komplette linjer i en JSONL-fil, parset. Ufullstendig siste linje
   * (en skriver kan være midt i en append) og ugyldig JSON hoppes over —
   * samme toleranse som ps1-runneren og integrations har.
   */
  private async readJsonl(file: string): Promise<unknown[]> {
    let text: string;
    try {
      text = await readFile(file, "utf8");
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") return [];
      throw err;
    }
    const lastNl = text.lastIndexOf("\n");
    if (lastNl === -1) return [];
    const out: unknown[] = [];
    for (const line of text.slice(0, lastNl).split("\n")) {
      if (line.trim() === "") continue;
      try {
        out.push(JSON.parse(line));
      } catch {
        // Ugyldig linje — hopp over, ikke stopp strømmen.
      }
    }
    return out;
  }

  async readInbox(): Promise<QueueEvent[]> {
    const events: QueueEvent[] = [];
    for (const row of await this.readJsonl(this.inboxFile)) {
      const evt = row as Partial<QueueEvent>;
      // Events uten id kan ikke kvitteres ut i results.jsonl og hoppes over
      // (samme regel som ps1-runneren).
      if (typeof evt.id !== "string" || evt.id === "") continue;
      events.push(evt as QueueEvent);
    }
    return events;
  }

  /** Id-ene som allerede har en resultatlinje. */
  async readResultIds(): Promise<Set<string>> {
    const ids = new Set<string>();
    for (const row of await this.readJsonl(this.resultsFile)) {
      const id = (row as { id?: unknown }).id;
      if (typeof id === "string" && id !== "") ids.add(id);
    }
    return ids;
  }

  /** Har dette eventet fått en resultatlinje? */
  async hasResult(id: string): Promise<boolean> {
    return (await this.readResultIds()).has(id);
  }

  /**
   * Skriver én resultatlinje.
   *
   * Agenten inne i instansen skriver til samme fil, og den skriver linja
   * frihånds (i motsetning til entrypointene, som bygger den med jq). Glemmer
   * den linjeskiftet, ville vår append havnet på slutten av *dens* linje og
   * gjort begge uleselige — integrations ville da hoppet over hele linja, og
   * eventet blitt evig ubehandlet. Vi reparerer derfor et manglende linjeskift
   * før vi skriver.
   */
  async appendResult(line: ResultLine): Promise<void> {
    const prefix = (await this.endsWithNewline()) ? "" : "\n";
    await appendFile(this.resultsFile, prefix + JSON.stringify(line) + "\n", "utf8");
  }

  /** Slutter results.jsonl med linjeskift? (Tom/manglende fil regnes som ja.) */
  private async endsWithNewline(): Promise<boolean> {
    let handle;
    try {
      handle = await open(this.resultsFile, "r");
      const { size } = await handle.stat();
      if (size === 0) return true;
      const buf = Buffer.alloc(1);
      await handle.read(buf, 0, 1, size - 1);
      return buf[0] === 0x0a;
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") return true;
      throw err;
    } finally {
      await handle?.close();
    }
  }

  /**
   * Sti til bridgens egen logg for et event, relativt til triggers-dir.
   *
   * Id-en saneres: integrations krever at `log` peker innenfor agentens egen
   * triggers-katalog, og en id med `/` eller `..` ville både gitt en ugyldig
   * `log`-verdi og fått `appendBridgeLog` til å skrive utenfor katalogen.
   */
  bridgeLogRelPath(id: string): string {
    return path.posix.join("logs", `${safeId(id)}.bridge.log`);
  }

  /**
   * Append til bridgens egen logg for et event. Egen filnavn-suffiks
   * (`.bridge.log`) fordi agenten inne i instansen skriver `logs/<id>.log`
   * selv — de to skal ikke kollidere.
   *
   * Best effort: loggen er diagnostikk. En feil her skal ALDRI hindre at
   * resultatlinja blir skrevet — uten den er eventet evig ubehandlet.
   */
  async appendBridgeLog(id: string, message: string): Promise<void> {
    const stamp = new Date().toISOString();
    try {
      await appendFile(
        path.join(this.triggersDir, this.bridgeLogRelPath(id)),
        `[${stamp}] ${message}\n`,
        "utf8",
      );
    } catch {
      // Ignorert med vilje — se over.
    }
  }
}

/**
 * Dedupe-regelen: ubehandlet = event-id uten linje i `results.jsonl`.
 * Identisk med `scripts/agent-runner.ps1` og resten av pipelinen.
 *
 * `inFlight` er bridgens tillegg: et event som er dispatchet men ikke ferdig
 * har ennå ingen resultatlinje, så uten denne ville neste polling dispatche
 * det på nytt. (ps1-runneren slapp unna med «én container per topic» som
 * implisitt vakt; bridgen køer per topic og trenger id-vakten eksplisitt.)
 */
export function pendingEvents(
  inbox: QueueEvent[],
  doneIds: ReadonlySet<string>,
  inFlight: ReadonlySet<string> = new Set(),
): QueueEvent[] {
  return inbox.filter((e) => !doneIds.has(e.id) && !inFlight.has(e.id));
}
