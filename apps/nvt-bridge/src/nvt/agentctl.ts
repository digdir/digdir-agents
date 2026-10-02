import type { ExecFn } from "./docker.ts";
import type { DoneOutcome, NvtDriver, NvtInstance } from "./driver.ts";

/**
 * ==========================================================================
 *  Tynn adapter over agentctl — UKALIBRERT (F1 i doc/plans/jr-paa-agentctl.md)
 * ==========================================================================
 *
 * Driver for agenter definert med agent.yaml (agentctl/sandbox-stacken som
 * flyttes inn fra altinn-studio). Alle antakelser om agentctl bor HER og i
 * `agents/jr-sandbox/README.md` («Kalibreringspunkter»); kjernelogikken
 * testes mot `FakeNvtDriver` og er uavhengig av denne fila.
 *
 * Topologien er en annen enn docker-driverens: nvt har én container per
 * topic; agentctl har ÉN varig Agent (sandbox) med én varig Session per
 * topic. `instance`-navnet fra bridgen brukes som session-navn.
 *
 * Antakelser skrevet mot altinn-studios HARNESSES.md og agents/README.md,
 * som er fasiten til koden flytter inn:
 *
 * - `agentctl apply --wait` (cwd = agent-katalogen) er idempotent
 *   konvergens og returnerer når agenten er Ready.
 * - `agentctl create session/<navn>` oppretter en sesjon headless (attach
 *   er en egen kommando).
 * - `agentctl prompt session/<navn> <tekst>` leverer prompten som
 *   posisjonsargument og returnerer når den er submittet. Viser kalibreringen
 *   at CLI-et vil ha stdin i stedet, er `promptArgs()` det ene stedet å rette.
 * - `agentctl get sessions -o json` lister sesjonene med navn og state;
 *   `Working` betyr «turen pågår». `parseSessions()` er bevisst tolerant på
 *   feltnavn til formatet er verifisert.
 * - `agentctl archive session/<navn>` tilsvarer nvt-ens `agent-down`:
 *   samtale og navn består, `unarchive`/neste prompt gjenopptar.
 * - Session-scope angis med `--agent agent/<navn>` (og cwd-inferens som
 *   backup, siden alle kall kjører fra agent-katalogen).
 *
 * nvt-ens `--external`-flagg finnes ikke her: untrusted-input-rammen bæres
 * av selve prompt-teksten (prompt.ts bygger nonce-avgrensere uansett, og
 * `dialect: "agentctl"` dropper `agentdctl signal done`-steget).
 */

export interface AgentctlDriverOptions {
  /** Katalogen med agent.yaml (f.eks. agents/jr-sandbox) — cwd for alle kall. */
  agentDir: string;
  /** `metadata.name` i agent.yaml. */
  agentName: string;
  /** agentctl-binæren. Default `agentctl` fra PATH. */
  bin?: string;
  /** Hvor tett done-sjekken poller session-state. Default 2000 ms. */
  donePollMs?: number;
  /**
   * Hvor lenge waitForDone venter på å se `Working` før en rolig sesjon
   * regnes som «turen var ferdig før vi rakk å se den». Default 30 s.
   */
  settleTimeoutMs?: number;
  /** Overstyring for testing. */
  exec?: ExecFn;
  sleep?: (ms: number) => Promise<void>;
  log?: (message: string) => void;
}

export interface SessionInfo {
  name: string;
  state: string;
}

export class AgentctlDriver implements NvtDriver {
  private readonly opts: AgentctlDriverOptions;
  private readonly bin: string;
  private readonly exec: ExecFn;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly log: (message: string) => void;
  /** Agenten konvergeres én gang per prosess; agentd eier livssyklusen etterpå. */
  private converged = false;

  constructor(opts: AgentctlDriverOptions) {
    this.opts = opts;
    this.bin = opts.bin ?? "agentctl";
    this.exec = opts.exec ?? defaultExec;
    this.sleep = opts.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.log = opts.log ?? (() => {});
  }

  private agentRef(): string {
    return `agent/${this.opts.agentName}`;
  }

  private sessionRef(instance: string): string {
    return `session/${instance}`;
  }

  /** Kjører agentctl fra agent-katalogen og kaster med kommandoen i meldingen. */
  private async ctl(args: string[]): Promise<string> {
    const { code, stdout, stderr } = await this.exec(this.bin, args, {
      cwd: this.opts.agentDir,
    });
    if (code !== 0) {
      throw new Error(`${this.bin} ${args.join(" ")} feilet (exit ${code}): ${firstLine(stderr)}`);
    }
    return stdout;
  }

  private async listSessions(): Promise<SessionInfo[]> {
    const stdout = await this.ctl(["get", "sessions", "-o", "json", "--agent", this.agentRef()]);
    return parseSessions(stdout);
  }

  async ensureInstance(topic: string, instance: string): Promise<NvtInstance> {
    const ref: NvtInstance = { topic, instance };
    if (!this.converged) {
      this.log(`agentctl apply --wait (${this.agentRef()})`);
      await this.ctl(["apply", "--wait"]);
      this.converged = true;
    }
    const sessions = await this.listSessions();
    if (!sessions.some((s) => s.name === instance)) {
      this.log(`agentctl create ${this.sessionRef(instance)} (topic ${topic})`);
      try {
        await this.ctl(["create", this.sessionRef(instance), "--agent", this.agentRef()]);
      } catch (err) {
        // Kappløp eller arkivert sesjon med samme navn: finnes den nå, er
        // målet nådd. (unarchive-behov er et kalibreringspunkt.)
        const after = await this.listSessions();
        if (!after.some((s) => s.name === instance)) throw err;
      }
    }
    return ref;
  }

  /**
   * agentctl/agentd eier selv input-køing og oppstartsracene (jf.
   * HARNESSES.md: «Create without a prompt, then immediately prompt --wait»
   * er en støttet flyt). Klar-nivået her er derfor at sesjonen finnes og at
   * daemonen svarer — ikke tmux-panel-heuristikk som i docker-driveren.
   */
  async waitUntilReady(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<void> {
    const deadline = Date.now() + opts.timeoutMs;
    const pollMs = this.opts.donePollMs ?? 2000;
    let lastError = "";
    for (;;) {
      if (opts.signal?.aborted) {
        throw new Error(
          `klar-sjekken for ${instance.instance} ble avbrutt fordi broen avslutter ` +
            `(deploy/omstart). Ingen prompt ble sendt — oppgaven må sendes på nytt.`,
        );
      }
      try {
        const sessions = await this.listSessions();
        if (sessions.some((s) => s.name === instance.instance)) return;
        lastError = "sesjonen finnes ikke i `get sessions`";
      } catch (err) {
        lastError = firstLine(String(err));
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await this.sleep(Math.min(pollMs, remaining));
    }
    throw new Error(
      `sesjonen ${this.sessionRef(instance.instance)} ble ikke klar innen ` +
        `${Math.round(opts.timeoutMs / 1000)}s (${lastError}). Ingen prompt ble sendt. ` +
        `Sjekk \`agentctl get sessions -o json --agent ${this.agentRef()}\` på hosten.`,
    );
  }

  async sendPrompt(instance: NvtInstance, prompt: string): Promise<void> {
    await this.ctl(promptArgs(instance.instance, this.opts.agentName, prompt));
  }

  /**
   * Ferdig-deteksjon via session-state i to faser: først vente på å SE
   * `Working` (inntil `settleTimeoutMs` — en rask tur kan være ferdig før
   * første poll), deretter to påfølgende polls uten `Working` (samme
   * dobbelt-poll-prinsipp som agentd selv bruker for completion).
   *
   * En «done» her er uansett bare et hint: broen godtar aldri suksess uten
   * at agenten faktisk skrev resultatlinja (fallback-prinsippet i bridge.ts).
   */
  async waitForDone(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<DoneOutcome> {
    const started = Date.now();
    const deadline = started + opts.timeoutMs;
    const pollMs = this.opts.donePollMs ?? 2000;
    const settleBy = started + (this.opts.settleTimeoutMs ?? 30_000);
    let workingSeen = false;
    let calmPolls = 0;

    while (Date.now() < deadline && !opts.signal?.aborted) {
      let state: string | null = null;
      try {
        const sessions = await this.listSessions();
        state = sessions.find((s) => s.name === instance.instance)?.state ?? null;
      } catch {
        // Forbigående feil mot daemonen er ikke et «done» — poll videre.
      }

      if (state === "Working") {
        workingSeen = true;
        calmPolls = 0;
      } else if (state !== null) {
        if (workingSeen || Date.now() >= settleBy) {
          calmPolls++;
          if (calmPolls >= 2) {
            return { kind: "done", at: new Date().toISOString() };
          }
        }
      } else {
        // Sesjonen borte fra lista er aldri et «done».
        calmPolls = 0;
      }

      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await this.sleep(Math.min(pollMs, remaining));
    }
    return { kind: "timeout", waitedMs: Date.now() - started };
  }

  /**
   * TTL-opprydding. `archive` er agentctl-ekvivalenten til `agent-down`:
   * samtalen og navnet består, og neste event på topicet gjenopptar.
   */
  async stopInstance(instance: NvtInstance): Promise<void> {
    this.log(`agentctl archive ${this.sessionRef(instance.instance)}`);
    await this.ctl(["archive", this.sessionRef(instance.instance), "--agent", this.agentRef()]);
  }
}

/** Det ene stedet promptleveringen bor (kalibreringspunkt: argument vs. stdin). */
export function promptArgs(instance: string, agentName: string, prompt: string): string[] {
  return ["prompt", `session/${instance}`, "--agent", `agent/${agentName}`, prompt];
}

/**
 * Tolerant parsing av `get sessions -o json` til formatet er verifisert:
 * godtar et toppnivå-array eller `{sessions|items: []}`, navn i `name` eller
 * `metadata.name`, state i `state`, `status.state` eller `status` (streng).
 * Ukjente innslag hoppes over — en uleselig liste skal gi «ikke klar»/aldri
 * «done», ikke et krasj.
 */
export function parseSessions(stdout: string): SessionInfo[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    return [];
  }
  const list = Array.isArray(parsed)
    ? parsed
    : isRecord(parsed) && Array.isArray(parsed.sessions)
      ? parsed.sessions
      : isRecord(parsed) && Array.isArray(parsed.items)
        ? parsed.items
        : [];
  const out: SessionInfo[] = [];
  for (const entry of list) {
    if (!isRecord(entry)) continue;
    const meta = isRecord(entry.metadata) ? entry.metadata : undefined;
    const name =
      typeof entry.name === "string" ? entry.name : typeof meta?.name === "string" ? meta.name : "";
    const status = isRecord(entry.status) ? entry.status : undefined;
    const state =
      typeof entry.state === "string"
        ? entry.state
        : typeof status?.state === "string"
          ? status.state
          : typeof entry.status === "string"
            ? entry.status
            : "";
    if (name !== "") out.push({ name, state });
  }
  return out;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function firstLine(text: string): string {
  return text.split("\n").find((l) => l.trim() !== "")?.trim() ?? "";
}

// Egen default-exec i stedet for å importere docker.ts sin private — samme
// semantikk: aldri kaste, exit -1 ved spawn-feil.
import { spawn } from "node:child_process";
const defaultExec: ExecFn = (cmd, args, opts) =>
  new Promise((resolve) => {
    const child = spawn(cmd, args, { cwd: opts.cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout?.setEncoding("utf8");
    child.stderr?.setEncoding("utf8");
    child.stdout?.on("data", (c: string) => (stdout += c));
    child.stderr?.on("data", (c: string) => (stderr += c));
    child.on("error", (err) => resolve({ code: -1, stdout, stderr: String(err) }));
    child.on("close", (code) => resolve({ code: code ?? -1, stdout, stderr }));
  });
