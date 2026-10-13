import { randomUUID } from "node:crypto";
import { mkdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import type { ExecFn } from "./docker.ts";
import type { DoneOutcome, NvtDriver, NvtInstance } from "./driver.ts";

/**
 * ==========================================================================
 *  Tynn adapter over agentctl — kalibrert mot kilden (agentctl/ @
 *  v0.1.0-preview.10), ennå ikke kjørt mot en levende agent (F1-E2E i
 *  doc/plans/jr-paa-agentctl.md gjenstår)
 * ==========================================================================
 *
 * Driver for agenter definert med agent.yaml (agentctl/sandbox i dette
 * repoet). Alle antakelser om agentctl bor HER og i
 * `agents/jr-sandbox/README.md`; kjernelogikken testes mot `FakeNvtDriver`
 * og er uavhengig av denne fila.
 *
 * Topologien er en annen enn docker-driverens: nvt har én container per
 * topic; agentctl har ÉN varig Agent (sandbox) med én varig Session per
 * topic. `instance`-navnet fra bridgen brukes som session-navn.
 *
 * Kalibrert mot `agentctl/src/bin/agentctl/main.rs` og
 * `agentctl/src/sessions/mod.rs`:
 *
 * - `apply --wait` (cwd = agent-katalogen) er deklarativ konvergens og
 *   venter til agenten er Ready (default-timeout 10m i CLI-et).
 * - `create session/<navn> --agent <agentnavn>` er ensure-semantikk
 *   (`ensure_session`): oppretter ELLER gjenbruker, og venter til harnesset
 *   er klart — uten attach. Dette er også klar-ventingen vår.
 * - Prompt leveres med `--file <fil>` (ikke posisjonsargument): prompten
 *   inneholder upålitelig tekst og kan være lang — en fil unngår både
 *   Windows-argumentgrenser og quoting. `--prompt`/stdin finnes også.
 * - `get sessions -o json --agent <navn> --archived` gir et JSON-array av
 *   Session-objekter (camelCase): `name`, `agent`, `status.state`.
 *   State-verdiene er camelCase med liten forbokstav: `starting`, `working`,
 *   `waitingForInput`, `idle`, `archiving`, `archived`, `failed`.
 * - `--agent` tar agent-NAVNET (`jr-sandbox`), ikke ressursformen.
 * - `archive session/<navn>` stopper harnesset og skjuler sesjonen fra
 *   listinger; navn og samtale består. Arkiverte sesjoner må `unarchive`-s
 *   før ny bruk — derfor lister vi med `--archived` og unarchiver i
 *   `ensureInstance`.
 *
 * nvt-ens `--external`-flagg finnes ikke her: untrusted-input-rammen bæres
 * av selve prompt-teksten (prompt.ts bygger nonce-avgrensere, og
 * `dialect: "agentctl"` dropper `agentdctl signal done`-steget).
 * Onboarding-dialog-problemet fra M0 finnes ikke: imaget pre-seeder
 * `~/.claude/.claude.json` (se agents/jr-sandbox/Dockerfile), og `create`
 * venter uansett på harness-klar.
 */

export interface AgentctlDriverOptions {
  /** Katalogen med agent.yaml (f.eks. agents/jr-sandbox) — cwd for alle kall. */
  agentDir: string;
  /** `metadata.name` i agent.yaml. */
  agentName: string;
  /** agentctl-binæren. Default `agentctl` fra PATH. */
  bin?: string;
  /** Hvor tett klar-/done-sjekkene poller session-state. Default 2000 ms. */
  donePollMs?: number;
  /**
   * Hvor lenge waitForDone venter på å se `working` før en rolig sesjon
   * regnes som «turen var ferdig før vi rakk å se den». Default 30 s.
   */
  settleTimeoutMs?: number;
  /** Katalog for midlertidige promptfiler. Default `os.tmpdir()`. */
  tmpDir?: string;
  /** Overstyring for testing. */
  exec?: ExecFn;
  files?: PromptFileFns;
  sleep?: (ms: number) => Promise<void>;
  log?: (message: string) => void;
}

/** Det lille filsnittet promptleveringen trenger — injiserbart for tester. */
export interface PromptFileFns {
  write(target: string, content: string): Promise<void>;
  remove(target: string): Promise<void>;
}

export interface SessionInfo {
  name: string;
  state: string;
}

/** Session-states fra `agentctl/src/sessions/mod.rs` (serde camelCase). */
const WORKING = "working";
const CALM_STATES = new Set(["waitingForInput", "idle"]);
const ARCHIVED_STATES = new Set(["archiving", "archived"]);

export class AgentctlDriver implements NvtDriver {
  private readonly opts: AgentctlDriverOptions;
  private readonly bin: string;
  private readonly exec: ExecFn;
  private readonly files: PromptFileFns;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly log: (message: string) => void;
  /** Agenten konvergeres én gang per prosess; agentd eier livssyklusen etterpå. */
  private converged = false;

  constructor(opts: AgentctlDriverOptions) {
    this.opts = opts;
    this.bin = opts.bin ?? "agentctl";
    this.exec = opts.exec ?? defaultExec;
    this.files = opts.files ?? nodeFiles;
    this.sleep = opts.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.log = opts.log ?? (() => {});
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

  /** Lister ALLE sesjoner, også arkiverte — ensureInstance må se dem. */
  private async listSessions(): Promise<SessionInfo[]> {
    const stdout = await this.ctl([
      "get",
      "sessions",
      "-o",
      "json",
      "--agent",
      this.opts.agentName,
      "--archived",
    ]);
    return parseSessions(stdout);
  }

  async ensureInstance(topic: string, instance: string): Promise<NvtInstance> {
    const ref: NvtInstance = { topic, instance };
    if (!this.converged) {
      this.log(`agentctl apply --wait (agent ${this.opts.agentName})`);
      await this.ctl(["apply", "--wait"]);
      this.converged = true;
    }
    const existing = (await this.listSessions()).find((s) => s.name === instance);
    if (existing && ARCHIVED_STATES.has(existing.state)) {
      // TTL-en vår arkiverer inaktive topics; et nytt event på topicet
      // gjenopptar samtalen. `create` alene gjenoppliver ikke en arkivert
      // sesjon — den må unarchives først.
      this.log(`agentctl unarchive ${this.sessionRef(instance)} (topic ${topic})`);
      await this.ctl(["unarchive", this.sessionRef(instance), "--agent", this.opts.agentName]);
    }
    // `create` er ensure-semantikk: oppretter eller gjenbruker, og venter til
    // harnesset er klart. Trygt å kjøre hver gang (idempotent per kontrakten
    // i driver.ts).
    if (!existing) this.log(`agentctl create ${this.sessionRef(instance)} (topic ${topic})`);
    try {
      await this.ctl(["create", this.sessionRef(instance), "--agent", this.opts.agentName]);
    } catch (err) {
      // Kappløp: finnes sesjonen nå (og er i bruk), er målet nådd.
      const after = (await this.listSessions()).find((s) => s.name === instance);
      if (!after || ARCHIVED_STATES.has(after.state)) throw err;
    }
    return ref;
  }

  /**
   * `create` i ensureInstance har allerede ventet til harnesset er klart
   * (ensure_session med WaitPolicy::UntilConverged), og agentd køer selv
   * input ved oppstartsracer. Dette er derfor en bekreftelse: sesjonen står
   * i lista og er ikke `failed`/arkivert.
   */
  async waitUntilReady(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<void> {
    const deadline = Date.now() + opts.timeoutMs;
    const pollMs = this.opts.donePollMs ?? 2000;
    let last = "sesjonen finnes ikke i `get sessions`";
    for (;;) {
      if (opts.signal?.aborted) {
        throw new Error(
          `klar-sjekken for ${instance.instance} ble avbrutt fordi broen avslutter ` +
            `(deploy/omstart). Ingen prompt ble sendt — oppgaven må sendes på nytt.`,
        );
      }
      try {
        const session = (await this.listSessions()).find((s) => s.name === instance.instance);
        if (session && session.state !== "failed" && !ARCHIVED_STATES.has(session.state)) {
          return;
        }
        last = session ? `state er \`${session.state}\`` : last;
      } catch (err) {
        last = firstLine(String(err));
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await this.sleep(Math.min(pollMs, remaining));
    }
    throw new Error(
      `sesjonen ${this.sessionRef(instance.instance)} ble ikke klar innen ` +
        `${Math.round(opts.timeoutMs / 1000)}s (${last}). Ingen prompt ble sendt. ` +
        `Sjekk \`agentctl get sessions -o json --agent ${this.opts.agentName} --archived\` på hosten.`,
    );
  }

  async sendPrompt(instance: NvtInstance, prompt: string): Promise<void> {
    // Via fil: prompten er lang, upålitelig tekst — en fil unngår
    // argumentgrenser og quoting, og read_prompt_arg leser den ordrett.
    const file = path.join(
      this.opts.tmpDir ?? os.tmpdir(),
      `nvt-bridge-prompt-${instance.instance}-${randomUUID()}.md`,
    );
    await this.files.write(file, prompt);
    try {
      await this.ctl([
        "prompt",
        this.sessionRef(instance.instance),
        "--agent",
        this.opts.agentName,
        "--file",
        file,
      ]);
    } finally {
      // Best effort — en gjenglemt tmp-fil skal aldri velte leveransen.
      await this.files.remove(file).catch(() => {});
    }
  }

  /**
   * Ferdig-deteksjon via session-state i to faser: først vente på å SE
   * `working` (inntil `settleTimeoutMs` — en rask tur kan være ferdig før
   * første poll), deretter to påfølgende polls i hvilestate
   * (`waitingForInput`/`idle` — samme dobbelt-poll-prinsipp som agentd selv
   * bruker for completion-venting i `prompt --wait`).
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

      if (state === WORKING) {
        workingSeen = true;
        calmPolls = 0;
      } else if (state !== null && CALM_STATES.has(state)) {
        if (workingSeen || Date.now() >= settleBy) {
          calmPolls++;
          if (calmPolls >= 2) {
            return { kind: "done", at: new Date().toISOString() };
          }
        }
      } else {
        // `starting`, `failed`, arkivert eller borte fra lista er aldri et
        // «done» — resultatlinje uten fullført tur finnes ikke.
        calmPolls = 0;
      }

      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await this.sleep(Math.min(pollMs, remaining));
    }
    return { kind: "timeout", waitedMs: Date.now() - started };
  }

  /**
   * TTL-opprydding. `archive` stopper harnesset og skjuler sesjonen;
   * samtalen og navnet består, og ensureInstance unarchiver ved neste event.
   */
  async stopInstance(instance: NvtInstance): Promise<void> {
    this.log(`agentctl archive ${this.sessionRef(instance.instance)}`);
    await this.ctl(["archive", this.sessionRef(instance.instance), "--agent", this.opts.agentName]);
  }
}

/**
 * Parsing av `get sessions -o json`: et toppnivå-array av Session-objekter
 * med `name` (streng) og `status.state`. Tolerant på innpakking og feltnavn
 * (eldre/nyere agentctl), og en uleselig liste gir tom liste — «ikke
 * klar»/aldri «done», ikke et krasj.
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
      typeof status?.state === "string"
        ? status.state
        : typeof entry.state === "string"
          ? entry.state
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

const nodeFiles: PromptFileFns = {
  write: async (target, content) => {
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, content, "utf8");
  },
  remove: (target) => rm(target, { force: true }),
};

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
