import { NvtBridge } from "./bridge.ts";
import { loadConfig } from "./config.ts";
import { DockerNvtDriver } from "./nvt/docker.ts";
import { DryRunNvtDriver } from "./nvt/dryrun.ts";
import type { NvtDriver } from "./nvt/driver.ts";
import { assertAgentCanTraverse } from "./nvt/paths.ts";
import { TopicStore } from "./state.ts";
import { TriggerFiles } from "./triggers.ts";

/** Tidsstemplet logg til stdout. Aldri hemmeligheter her — kun id-er og navn. */
function log(message: string): void {
  console.log(`[${new Date().toISOString()}] [nvt-bridge] ${message}`);
}

// Null runtime-avhengigheter: Node laster .env selv (21.7+). Mangler filen,
// kommer configen fra prosessmiljøet (compose `env_file`).
try {
  process.loadEnvFile();
} catch {
  // Ingen .env — helt greit.
}

const config = loadConfig();

// Sti-validering FØR noe annet (M0-funn 2): ligger nvt-sjekkouten eller
// triggers-katalogen bak en katalog uid 1000 ikke kommer gjennom (klassikeren
// er /root, mode 0700), feiler bootstrap inne i containeren med en kryptisk
// «Permission denied» langt fra årsaken. Da er det bedre å ikke starte.
if (!config.dryRun) {
  const pathCheck = { skip: config.skipPathCheck, log };
  await assertAgentCanTraverse("NVT_ROOT", config.nvtRoot, pathCheck);
  // Agenten skriver resultatlinja selv, så triggers/ må også være skrivbar for
  // uid 1000 — ellers ender hvert event i fallback-feil etter en time.
  await assertAgentCanTraverse("NVT_BRIDGE_TRIGGERS_DIR", config.triggersDir, {
    ...pathCheck,
    requireWrite: true,
  });
}

const driver: NvtDriver = config.dryRun
  ? new DryRunNvtDriver(log)
  : new DockerNvtDriver({
      nvtRoot: config.nvtRoot,
      agentType: config.agentType,
      autonomy: config.autonomy,
      userMode: config.userMode,
      tmuxSession: config.tmuxSession,
      agentConfig: config.agentConfig,
      panePatterns: config.panePatterns,
      readyPollMs: config.readyPollMs,
      maxEnterPresses: config.maxEnterPresses,
      log,
    });

if (config.dryRun) {
  log("TØRRKJØRING (NVT_BRIDGE_DRY_RUN=1): ingen nvt-instanser startes");
}
if (config.userMode !== "non-root") {
  // M0-funn 1: claude nekter --dangerously-skip-permissions som root, og
  // tmux-sesjonen dør innen 5 s. Vi stopper ikke — codex/interactive kan ha
  // andre behov — men det skal stå i loggen når det går galt.
  log(
    `ADVARSEL: NVT_AGENT_USER_MODE=${config.userMode}. claude nekter bypass-flagget som root ` +
      `(M0-funn 1) og sesjonen dør innen 5 s. Bruk non-root.`,
  );
}

const bridge = new NvtBridge({
  triggers: new TriggerFiles(config.triggersDir),
  store: new TopicStore(config.stateDir),
  driver,
  instanceNaming: config.instanceNaming,
  maxParallel: config.maxParallel,
  promptTimeoutMs: config.promptTimeoutMs,
  readyTimeoutMs: config.readyTimeoutMs,
  resultGraceMs: config.resultGraceMs,
  idleTtlMs: config.idleTtlMs,
  instanceTriggersPath: config.instanceTriggersPath,
  log,
});

const controller = new AbortController();
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    log(`${signal} mottatt — avslutter etter inneværende syklus`);
    controller.abort();
  });
}

await bridge.run(config.pollMs, controller.signal);
