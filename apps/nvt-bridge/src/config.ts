import path from "node:path";
import type { AgentConfigVars } from "./nvt/agentConfig.ts";
import {
  DEFAULT_ONBOARDING_PATTERN,
  DEFAULT_READY_PATTERN,
  patternFromEnv,
  type PanePatterns,
} from "./nvt/ready.ts";
import type { InstanceNameOptions } from "./topic.ts";

export interface BridgeConfig {
  triggersDir: string;
  stateDir: string;
  pollMs: number;
  maxParallel: number;
  promptTimeoutMs: number;
  resultGraceMs: number;
  idleTtlMs: number;
  nvtRoot: string;
  agentType: string;
  /** `--autonomy` til `agent-init`. */
  autonomy: string;
  /** `--user` til `agent-init`. `non-root` er det eneste som virker (M0-funn 1). */
  userMode: string;
  /** tmux-sesjonen agenten kjører i. */
  tmuxSession: string;
  /** Hvor lenge klar-sjekken venter før den gir opp (M0-funn 5). */
  readyTimeoutMs: number;
  readyPollMs: number;
  maxEnterPresses: number;
  panePatterns: PanePatterns;
  /** Identitet og provider til `agent.yaml` (M0-funn 4). */
  agentConfig: Omit<AgentConfigVars, "agentType" | "runtimeArgs" | "userMode">;
  instanceNaming: InstanceNameOptions;
  /** Hvor agentens triggers/ er mountet inne i instansen. */
  instanceTriggersPath: string;
  /** Hopp over sti-sjekken mot uid 1000 (M0-funn 2). */
  skipPathCheck: boolean;
  dryRun: boolean;
}

export function loadConfig(
  env: NodeJS.ProcessEnv = process.env,
  cwd: string = process.cwd(),
): BridgeConfig {
  const dryRun = boolFrom(env.NVT_BRIDGE_DRY_RUN);
  const nvtRoot = (env.NVT_ROOT ?? "").trim();
  if (!dryRun && nvtRoot === "") {
    throw new Error(
      "NVT_ROOT må peke på nvt-agent-sjekkouten (eller sett NVT_BRIDGE_DRY_RUN=1). " +
        "Kopiér .env.example til .env.",
    );
  }

  const userMode = (env.NVT_AGENT_USER_MODE ?? "non-root").trim();
  if (userMode !== "non-root" && userMode !== "root") {
    throw new Error(`NVT_AGENT_USER_MODE: forventet "non-root" eller "root", fikk "${userMode}"`);
  }

  // Valideres her, ikke per event: `agent-init` og runtime-argumentene i
  // agent.yaml støtter bare disse to. Ellers hadde broen startet fint og feilet
  // først når en oppgave kom — én feilmelding til Slack per delegering.
  const agentType = (env.NVT_AGENT_TYPE ?? "claude").trim();
  if (agentType !== "claude" && agentType !== "codex") {
    throw new Error(`NVT_AGENT_TYPE: forventet "claude" eller "codex", fikk "${agentType}"`);
  }
  const autonomy = (env.NVT_AGENT_AUTONOMY ?? "trusted-local").trim();
  if (autonomy !== "trusted-local" && autonomy !== "interactive") {
    throw new Error(
      `NVT_AGENT_AUTONOMY: forventet "trusted-local" eller "interactive", fikk "${autonomy}"`,
    );
  }

  // Commit-identiteten må stå eksplisitt i agent.yaml (M0-funn 4), og
  // bot-navnet skal ikke i repoet — derfor env, uten default. Mangler den,
  // ville agenten jobbet en time og så feilet på `git commit`.
  const identityName = (env.NVT_GIT_IDENTITY_NAME ?? "").trim();
  const identityEmail = (env.NVT_GIT_IDENTITY_EMAIL ?? "").trim();
  const brokerProvider = (env.NVT_BROKER_PROVIDER ?? "").trim();
  if (!dryRun) {
    const missing = [
      ["NVT_GIT_IDENTITY_NAME", identityName],
      ["NVT_GIT_IDENTITY_EMAIL", identityEmail],
      ["NVT_BROKER_PROVIDER", brokerProvider],
    ]
      .filter(([, value]) => value === "")
      .map(([name]) => name);
    if (missing.length > 0) {
      throw new Error(
        `Mangler ${missing.join(", ")}. Uten commit-identitet og broker-provider kan ` +
          `instansens agent.yaml ikke genereres (M0-funn 4), og agenten får ikke committet. ` +
          `Se .env.example.`,
      );
    }
  }

  return {
    triggersDir: path.resolve(
      cwd,
      (env.NVT_BRIDGE_TRIGGERS_DIR ?? "../../agents/nvt-fat-developer/triggers").trim(),
    ),
    stateDir: path.resolve(cwd, (env.NVT_BRIDGE_STATE_DIR ?? "./state").trim()),
    pollMs: intFrom(env.NVT_BRIDGE_POLL_SECONDS, 10, "NVT_BRIDGE_POLL_SECONDS", 1) * 1000,
    maxParallel: intFrom(env.NVT_BRIDGE_MAX_PARALLEL, 2, "NVT_BRIDGE_MAX_PARALLEL", 1),
    promptTimeoutMs:
      intFrom(env.NVT_BRIDGE_PROMPT_TIMEOUT_SECONDS, 3600, "NVT_BRIDGE_PROMPT_TIMEOUT_SECONDS", 1) *
      1000,
    resultGraceMs:
      intFrom(env.NVT_BRIDGE_RESULT_GRACE_SECONDS, 60, "NVT_BRIDGE_RESULT_GRACE_SECONDS", 0) * 1000,
    idleTtlMs:
      intFrom(env.NVT_BRIDGE_IDLE_TTL_MINUTES, 180, "NVT_BRIDGE_IDLE_TTL_MINUTES", 0) * 60_000,
    nvtRoot,
    agentType,
    autonomy,
    userMode,
    tmuxSession: (env.NVT_AGENT_TMUX_SESSION ?? "agent").trim(),
    readyTimeoutMs:
      intFrom(env.NVT_BRIDGE_READY_TIMEOUT_SECONDS, 180, "NVT_BRIDGE_READY_TIMEOUT_SECONDS", 1) *
      1000,
    readyPollMs:
      intFrom(env.NVT_BRIDGE_READY_POLL_SECONDS, 2, "NVT_BRIDGE_READY_POLL_SECONDS", 1) * 1000,
    maxEnterPresses: intFrom(
      env.NVT_BRIDGE_MAX_ONBOARDING_ENTER,
      3,
      "NVT_BRIDGE_MAX_ONBOARDING_ENTER",
      0,
    ),
    panePatterns: {
      ready: patternFromEnv(env.NVT_READY_PATTERN, DEFAULT_READY_PATTERN, "NVT_READY_PATTERN"),
      onboarding: patternFromEnv(
        env.NVT_ONBOARDING_PATTERN,
        DEFAULT_ONBOARDING_PATTERN,
        "NVT_ONBOARDING_PATTERN",
      ),
    },
    agentConfig: {
      gitProvider: (env.NVT_GIT_PROVIDER ?? "fatdev-broker").trim(),
      brokerProvider,
      targetMatch: (env.NVT_GIT_TARGET_MATCH ?? "github.com/digdir/*").trim(),
      urlMatch: (env.NVT_GIT_URL_MATCH ?? "https://github.com/digdir/").trim(),
      identityName,
      identityEmail,
    },
    instanceTriggersPath: (env.NVT_INSTANCE_TRIGGERS_PATH ?? "/triggers").trim(),
    skipPathCheck: boolFrom(env.NVT_BRIDGE_SKIP_PATH_CHECK),
    instanceNaming: {
      prefix: (env.NVT_INSTANCE_PREFIX ?? "fatdev").trim(),
      maxLength: intFrom(env.NVT_INSTANCE_NAME_MAX, 40, "NVT_INSTANCE_NAME_MAX", 16),
    },
    dryRun,
  };
}

function intFrom(raw: string | undefined, fallback: number, name: string, min: number): number {
  const value = (raw ?? "").trim();
  if (value === "") return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < min) {
    throw new Error(`${name}: forventet et heltall >= ${min}, fikk "${value}"`);
  }
  return parsed;
}

function boolFrom(raw: string | undefined): boolean {
  const value = (raw ?? "").trim().toLowerCase();
  return value === "1" || value === "true" || value === "yes";
}
