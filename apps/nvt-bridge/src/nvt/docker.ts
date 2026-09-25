import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import {
  identityProblem,
  renderAgentConfig,
  type AgentConfigVars,
} from "./agentConfig.ts";
import type { DoneOutcome, NvtDriver, NvtInstance } from "./driver.ts";
import {
  classifyPane,
  DEFAULT_PANE_PATTERNS,
  type PaneState,
  type PanePatterns,
} from "./ready.ts";

/**
 * ==========================================================================
 *  Tynn adapter over nvt-agents lokale flyt — kalibrert mot M0-funnene
 *  (issue #96, `doc/plans/nvt-agent-integrasjon.md` § «M0-funn»).
 * ==========================================================================
 *
 * Alle antakelser om nvt-oppsettet bor HER og ingen andre steder i broen.
 * Kjernelogikken testes mot `FakeNvtDriver` og er uavhengig av denne fila.
 *
 * Hva M0 endret, og hvorfor:
 *
 * 1. **`agent-init` kjøres som script, ikke via `make`.** `make agent-init`
 *    kaller `scripts/agent-init.sh --name --type --autonomy` og forwarder
 *    *ikke* `--user`. Vi må ha `--user non-root` (M0-funn 1: claude nekter
 *    `--dangerously-skip-permissions` som root, tmux-sesjonen dør innen 5 s),
 *    så adapteren kaller scriptet direkte. `agent-up`/`agent-down` går
 *    fortsatt gjennom make — de tar bare `NAME`.
 * 2. **Klar-sjekk før første prompt** (M0-funn 5). Se `waitUntilReady`.
 * 3. **`agent.yaml` legges på plass før init** (M0-funn 4) — nvt-malen har
 *    `plugins: []`, og eksempelet der bruker `identity.mode: provider`, som
 *    ikke virker for broker-token. Se `agentConfig.ts`.
 * 4. **Sti-validering** (M0-funn 2) skjer ved oppstart, i `paths.ts`.
 *
 * Fortsatt uverifisert mot en ekte instans:
 *
 * - **Containernavnet**: `agent-<navn>-agent-1` (compose sitt
 *   `<prosjekt>-<tjeneste>-<n>`). Er tjenestenavnet noe annet, er
 *   `containerName()` det ene stedet å rette.
 * - **`subscribe`-formatet**: vi antar JSONL på stdout og at done-eventet har
 *   `type` (eller `event`) `plugin.agent.signal.done`. `isDoneEvent()` godtar
 *   begge feltnavn nettopp fordi dette ikke er prøvd ende-til-ende.
 * - **Klar-mønstrene** er claude-TUI-tekst, altså heuristikk. De kan
 *   overstyres med env — se `ready.ts`.
 * - **Nettverk/host-oppslag** berøres IKKE herfra (M0 bekreftet at
 *   `host.docker.internal:8787` virker); det er instans-config i
 *   `.agents/<navn>/env`.
 * - **Bind-mount-fella**: compose-mounts løses av *hostens* daemon, så
 *   `NVT_ROOT` og triggers-stien må være identiske inne i bridge-containeren
 *   og på hosten. Adapteren gjør ingen sti-oversetting — bevisst valg.
 */

export interface DockerNvtDriverOptions {
  /** Sjekkouten av nvt-agent — cwd for `make` og `scripts/`. */
  nvtRoot: string;
  /** Agent-type til `agent-init` (f.eks. `claude`). */
  agentType: string;
  /** `--autonomy` til `agent-init`. Default `trusted-local`. */
  autonomy?: string;
  /** `--user` til `agent-init`. Default (og eneste fungerende) `non-root`. */
  userMode?: string;
  /** tmux-sesjonen agenten kjører i (`AGENT_SESSION` i nvt). Default `agent`. */
  tmuxSession?: string;
  /** Identitet/provider til `agent.yaml`. Uten den kan configen ikke skrives. */
  agentConfig: Omit<AgentConfigVars, "agentType" | "runtimeArgs" | "userMode">;
  /** Mønstre for klar-sjekken. Default `DEFAULT_PANE_PATTERNS`. */
  panePatterns?: PanePatterns;
  /** Hvor tett klar-sjekken poller. Default 2000 ms. */
  readyPollMs?: number;
  /** Maks antall Enter gjennom onboarding-dialogene. Default 3. */
  maxEnterPresses?: number;
  /** Overstyring for testing/tørrkjøring. */
  exec?: ExecFn;
  fs?: FsFns;
  sleep?: (ms: number) => Promise<void>;
  log?: (message: string) => void;
}

export type ExecFn = (
  cmd: string,
  args: string[],
  opts: { cwd?: string },
) => Promise<{ code: number; stdout: string; stderr: string }>;

/** Det lille filsystem-snittet adapteren trenger — injiserbart for tester. */
export interface FsFns {
  /** `null` når fila ikke finnes. */
  readFile(target: string): Promise<string | null>;
  writeFile(target: string, content: string): Promise<void>;
  mkdirp(target: string): Promise<void>;
}

export class DockerNvtDriver implements NvtDriver {
  private readonly opts: DockerNvtDriverOptions;
  private readonly exec: ExecFn;
  private readonly fs: FsFns;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly log: (message: string) => void;
  private readonly patterns: PanePatterns;
  private readonly session: string;
  /** Instanser vi har kjørt `agent-up` for i denne prosessen. */
  private readonly started = new Set<string>();
  /**
   * Instanser vi har sett klar-prompten i, med *hvilken* container-inkarnasjon
   * det gjaldt (`<id> <startedAt>`). Restartes containeren utenfor broen
   * (`docker restart`, host-reboot med `restart: unless-stopped`), er det en ny
   * sesjon med ny onboarding — og da må klar-sjekken gjøres på nytt selv om
   * `agent-up` aldri ble kjørt herfra.
   */
  private readonly readyConfirmed = new Map<string, string>();

  constructor(opts: DockerNvtDriverOptions) {
    this.opts = opts;
    this.exec = opts.exec ?? runCommand;
    this.fs = opts.fs ?? nodeFs;
    this.sleep = opts.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.log = opts.log ?? (() => {});
    this.patterns = opts.panePatterns ?? DEFAULT_PANE_PATTERNS;
    this.session = opts.tmuxSession ?? "agent";
  }

  /**
   * Compose-container for instansens runtime. Se advarselen øverst — dette
   * navnet er det mest sannsynlige som må rettes.
   */
  containerName(instance: string): string {
    return `agent-${instance}-agent-1`;
  }

  /** `.agents/<navn>/agent.yaml` i nvt-sjekkouten. */
  agentConfigPath(instance: string): string {
    return path.join(this.opts.nvtRoot, ".agents", instance, "agent.yaml");
  }

  async ensureInstance(topic: string, instance: string): Promise<NvtInstance> {
    const ref: NvtInstance = { topic, instance };
    if (this.started.has(instance) && (await this.isRunning(instance))) {
      return ref;
    }
    if (!(await this.isRunning(instance))) {
      // Configen må ligge der FØR init: `agent-init` skriver nvt-malen bare
      // når fila ikke finnes, og malen har ikke identiteten vi trenger.
      await this.ensureAgentConfig(instance);

      const userMode = this.opts.userMode ?? "non-root";
      this.log(`agent-init --name ${instance} --type ${this.opts.agentType} --user ${userMode}`);
      // Direkte script-kall: make-målet forwarder ikke --user (se topp av fila).
      await this.run("bash", [
        "scripts/agent-init.sh",
        "--name",
        instance,
        "--type",
        this.opts.agentType,
        "--autonomy",
        this.opts.autonomy ?? "trusted-local",
        "--user",
        userMode,
      ]);
      this.log(`agent-up NAME=${instance}`);
      await this.run("make", ["agent-up", `NAME=${instance}`]);
      // Ny sesjon ⇒ onboardingen kan stå der igjen.
      this.readyConfirmed.delete(instance);
    }
    this.started.add(instance);
    return ref;
  }

  /**
   * Skriver `agent.yaml` når den mangler, og verifiserer identiteten når den
   * finnes. Vi retter aldri en eksisterende config: den kan være håndredigert i
   * en levende sesjon, og en stille omskriving er verre enn en ærlig feil.
   */
  private async ensureAgentConfig(instance: string): Promise<void> {
    const configPath = this.agentConfigPath(instance);
    const existing = await this.fs.readFile(configPath);
    if (existing !== null) {
      const problem = identityProblem(existing, configPath);
      if (problem?.level === "error") throw new Error(problem.message);
      if (problem) this.log(`ADVARSEL: ${problem.message}`);
      return;
    }
    const content = renderAgentConfig({
      ...this.opts.agentConfig,
      agentType: this.opts.agentType,
      userMode: this.opts.userMode ?? "non-root",
      runtimeArgs: runtimeArgsFor(this.opts.agentType, this.opts.autonomy ?? "trusted-local"),
    });
    await this.fs.mkdirp(path.dirname(configPath));
    await this.fs.writeFile(configPath, content);
    this.log(`skrev ${configPath} (identity.mode: explicit)`);
  }

  /**
   * Venter til sesjonen kan ta imot en prompt (M0-funn 5).
   *
   * Onboardingen er en **sesjonsstart**-tilstand, så sjekken har to nivåer:
   *
   * - *Fersk sesjon* (vi har ikke sett klar-prompten i denne container-
   *   inkarnasjonen): markøren må finnes, tmux svare, og panelet vise
   *   klar-prompt. Står en onboarding-dialog der, sendes ett Enter per runde
   *   (maks `maxEnterPresses`).
   * - *Bekreftet sesjon*: markør + levende tmux er nok. Vi leser ikke panelet,
   *   og sender ikke Enter. Grunnen er at panelet da inneholder transkriptet —
   *   inkludert den delegerte oppgaveteksten, som er upålitelig input. Å la
   *   den styre om broen trykker Enter (eller nekter å levere) ville gitt
   *   avsender en knapp hen ikke skal ha. Agenten kan dessuten være midt i
   *   arbeid, og `agentd` køer prompten selv.
   *
   * Enter sendes altså aldri i blinde: bare når en dialog faktisk er tegnet i
   * en sesjon som ikke har vært i bruk.
   *
   * Kaster ved timeout. Feilmeldingen sier hva som manglet, men gjengir
   * *aldri* panelinnholdet: det havner i resultatlinja og videre til Slack, og
   * en tmux-skjerm kan inneholde hva som helst.
   */
  async waitUntilReady(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<void> {
    const name = instance.instance;
    const pollMs = this.opts.readyPollMs ?? 2000;
    const maxEnter = this.opts.maxEnterPresses ?? 3;
    const deadline = Date.now() + opts.timeoutMs;

    let marker = false;
    let sessionAlive = false;
    let state: PaneState | "ikke lest" = "ikke lest";
    let enters = 0;

    for (;;) {
      if (opts.signal?.aborted) {
        throw new Error(
          `klar-sjekken for ${name} ble avbrutt fordi broen avslutter (deploy/omstart). ` +
            `Ingen prompt ble sendt, og ingenting er levert — oppgaven må sendes på nytt.`,
        );
      }

      const incarnation = await this.incarnation(name);
      const strict = incarnation === null || this.readyConfirmed.get(name) !== incarnation;

      marker = await this.readyMarkerPresent(name);
      const pane = strict ? await this.capturePane(name) : await this.sessionExists(name);
      sessionAlive = pane.ok;
      state = strict ? (pane.ok ? classifyPane(pane.text, this.patterns) : "unknown") : "ikke lest";

      if (marker && sessionAlive) {
        if (!strict) return;
        if (state === "ready") {
          this.log(`${name}: klar-prompt i tmux-panelet etter ${enters} Enter`);
          if (incarnation !== null) this.readyConfirmed.set(name, incarnation);
          return;
        }
        if (state === "onboarding" && enters < maxEnter) {
          enters++;
          this.log(
            `${name}: onboarding-dialog i tmux-panelet — sender Enter (${enters}/${maxEnter})`,
          );
          await this.sendEnter(name);
        }
      }

      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await this.sleep(Math.min(pollMs, remaining));
    }

    throw new Error(
      `sesjonen i ${name} ble ikke klar innen ${Math.round(opts.timeoutMs / 1000)}s ` +
        `(session-launched-markør: ${yesNo(marker)}, tmux-sesjon «${this.session}»: ` +
        `${yesNo(sessionAlive)}, panel: ${sessionAlive ? state : "ikke lest (ingen sesjon)"}, ` +
        `Enter sendt: ${enters}). ` +
        `Ingen prompt ble sendt — den ville blitt spist av onboardingen. ` +
        `Sjekk sesjonen i code-server: http://${name}.agent.localhost:4090. ` +
        `Har claude byttet ordlyd i TUI-et, kan mønsteret settes med NVT_READY_PATTERN.`,
    );
  }

  async sendPrompt(instance: NvtInstance, prompt: string): Promise<void> {
    // `--external` er ikke valgfritt: delegerte prompts er upålitelig input,
    // og flagget er det som gir nvt sin «untrusted input»-preamble.
    const { code, stderr } = await this.exec(
      "docker",
      [
        "exec",
        this.containerName(instance.instance),
        "agentdctl",
        "prompt",
        "--source",
        "host",
        "--external",
        prompt,
      ],
      {},
    );
    if (code !== 0) {
      throw new Error(
        `agentdctl prompt feilet for ${instance.instance} (exit ${code}): ${firstLine(stderr)}`,
      );
    }
  }

  async waitForDone(
    instance: NvtInstance,
    opts: { timeoutMs: number; signal?: AbortSignal },
  ): Promise<DoneOutcome> {
    const started = Date.now();
    return await new Promise<DoneOutcome>((resolve) => {
      const child = spawn(
        "docker",
        ["exec", this.containerName(instance.instance), "agentdctl", "subscribe"],
        { stdio: ["ignore", "pipe", "ignore"] },
      );
      let settled = false;
      let buffer = "";

      const finish = (outcome: DoneOutcome) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        opts.signal?.removeEventListener("abort", onAbort);
        child.kill("SIGTERM");
        resolve(outcome);
      };
      const onAbort = () =>
        finish({ kind: "timeout", waitedMs: Date.now() - started });
      const timer = setTimeout(
        () => finish({ kind: "timeout", waitedMs: Date.now() - started }),
        opts.timeoutMs,
      );
      timer.unref?.();
      opts.signal?.addEventListener("abort", onAbort, { once: true });

      child.stdout?.setEncoding("utf8");
      child.stdout?.on("data", (chunk: string) => {
        buffer += chunk;
        const lastNl = buffer.lastIndexOf("\n");
        if (lastNl === -1) return;
        const lines = buffer.slice(0, lastNl).split("\n");
        buffer = buffer.slice(lastNl + 1);
        for (const line of lines) {
          if (isDoneEvent(line)) {
            finish({ kind: "done", at: new Date().toISOString() });
            return;
          }
        }
      });
      // Dør subscribe (instansen nede, feil containernavn), er det ikke et
      // «done» — da skal bridgen skrive en error-linje, ikke anta suksess.
      child.on("error", onAbort);
      child.on("exit", () => {
        if (!settled) {
          finish({ kind: "timeout", waitedMs: Date.now() - started });
        }
      });
    });
  }

  async stopInstance(instance: NvtInstance): Promise<void> {
    this.log(`agent-down NAME=${instance.instance}`);
    await this.run("make", ["agent-down", `NAME=${instance.instance}`]);
    this.started.delete(instance.instance);
    // Neste oppstart er en ny sesjon — onboardingen kan komme tilbake.
    this.readyConfirmed.delete(instance.instance);
  }

  /**
   * Markøren `agentd` selv venter på. Stien slås opp med samme
   * env-presedens som `runtime/core/start-agent-session.sh` bruker, i
   * containerens eget skall — vi gjetter ikke på HOME.
   */
  private async readyMarkerPresent(instance: string): Promise<boolean> {
    const { code } = await this.exec(
      "docker",
      [
        "exec",
        this.containerName(instance),
        "sh",
        "-c",
        'test -f "${NVT_AGENT_SESSION_READY_MARKER:-${NVT_STATE_DIR:-$HOME/.nvt-agent}/agentd/session-launched}"',
      ],
      {},
    );
    return code === 0;
  }

  /** Panelinnholdet i tmux-sesjonen. `ok: false` = ingen sesjon (ennå). */
  private async capturePane(instance: string): Promise<{ ok: boolean; text: string }> {
    const { code, stdout } = await this.exec(
      "docker",
      ["exec", this.containerName(instance), "tmux", "capture-pane", "-p", "-t", this.session],
      {},
    );
    return { ok: code === 0, text: stdout };
  }

  /** Lever tmux-sesjonen? Brukes når vi ikke skal lese panelinnholdet. */
  private async sessionExists(instance: string): Promise<{ ok: boolean; text: string }> {
    const { code } = await this.exec(
      "docker",
      ["exec", this.containerName(instance), "tmux", "has-session", "-t", this.session],
      {},
    );
    return { ok: code === 0, text: "" };
  }

  /**
   * `<container-id> <startet-tidspunkt>` — identifiserer *denne* oppstarten av
   * containeren. `null` når containeren ikke finnes eller docker svarer rart;
   * da behandles sesjonen som fersk (streng sjekk), som er den trygge siden.
   */
  private async incarnation(instance: string): Promise<string | null> {
    const { code, stdout } = await this.exec(
      "docker",
      [
        "inspect",
        "-f",
        "{{.Id}} {{.State.StartedAt}}",
        this.containerName(instance),
      ],
      {},
    );
    const value = stdout.trim();
    return code === 0 && value !== "" ? value : null;
  }

  private async sendEnter(instance: string): Promise<void> {
    const { code, stderr } = await this.exec(
      "docker",
      ["exec", this.containerName(instance), "tmux", "send-keys", "-t", this.session, "Enter"],
      {},
    );
    if (code !== 0) {
      // Loggen er audit-sporet: den skal ikke påstå at et Enter ble sendt hvis
      // tmux avviste det.
      this.log(`${instance}: send-keys feilet (exit ${code}): ${firstLine(stderr)}`);
    }
  }

  /** Kjører en kommando i nvt-sjekkouten og kaster med målet i meldingen. */
  private async run(cmd: string, args: string[]): Promise<void> {
    const { code, stderr } = await this.exec(cmd, args, { cwd: this.opts.nvtRoot });
    if (code !== 0) {
      throw new Error(
        `${cmd} ${args.join(" ")} feilet (exit ${code}): ${firstLine(stderr)}`,
      );
    }
  }

  private async isRunning(instance: string): Promise<boolean> {
    const { code, stdout } = await this.exec(
      "docker",
      ["inspect", "-f", "{{.State.Running}}", this.containerName(instance)],
      {},
    );
    return code === 0 && stdout.trim() === "true";
  }
}

/**
 * Samme utledning som `scripts/agent-init.sh` gjør: `trusted-local` slår på
 * bypass-flagget. Vi trenger den fordi vi skriver `agent.yaml` selv.
 */
export function runtimeArgsFor(agentType: string, autonomy: string): string[] {
  if (autonomy !== "trusted-local") return [];
  if (agentType === "claude") return ["--dangerously-skip-permissions"];
  if (agentType === "codex") return ["--sandbox", "danger-full-access", "--ask-for-approval", "never"];
  throw new Error(`ukjent agent-type «${agentType}» — kan ikke utlede runtime-args til agent.yaml`);
}

/**
 * Godtar både `type` og `event` som feltnavn — hvilket nvt faktisk bruker er
 * ikke verifisert ende-til-ende. Ugyldige linjer ignoreres.
 */
export function isDoneEvent(line: string): boolean {
  const trimmed = line.trim();
  if (trimmed === "") return false;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return false;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return false;
  }
  const obj = parsed as { type?: unknown; event?: unknown };
  const name = typeof obj.type === "string" ? obj.type : obj.event;
  return name === "plugin.agent.signal.done";
}

function firstLine(text: string): string {
  return text.split("\n").find((l) => l.trim() !== "")?.trim() ?? "";
}

function yesNo(value: boolean): string {
  return value ? "ja" : "nei";
}

const nodeFs: FsFns = {
  readFile: async (target) => {
    try {
      return await readFile(target, "utf8");
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw err;
    }
  },
  writeFile: (target, content) => writeFile(target, content, "utf8"),
  mkdirp: async (target) => {
    await mkdir(target, { recursive: true });
  },
};

const runCommand: ExecFn = (cmd, args, opts) =>
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
