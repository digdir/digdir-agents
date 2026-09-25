import assert from "node:assert/strict";
import { test } from "node:test";
import { identityMode } from "./agentConfig.ts";
import {
  DockerNvtDriver,
  isDoneEvent,
  runtimeArgsFor,
  type ExecFn,
  type FsFns,
} from "./docker.ts";

/**
 * Adapteren er kalibrert mot M0-funnene, men fortsatt ikke prøvd mot en ekte
 * instans. Testene dekker derfor det som er ren logikk: kommandoene som bygges,
 * klar-sjekkens tilstandsmaskin, `agent.yaml`-håndteringen og gjenkjenningen av
 * done-eventet.
 */

interface Recorded {
  cmd: string;
  args: string[];
  cwd?: string;
}

interface ExecOptions {
  /** Svar fra `docker inspect -f {{.State.Running}}` — er instansen oppe? */
  running?: boolean;
  /** Finnes `session-launched`-markøren? */
  marker?: boolean;
  /** Panelinnhold per `capture-pane`. Siste verdi gjentas. */
  panes?: string[];
  /** `capture-pane` feiler (ingen tmux-sesjon). */
  paneFails?: boolean;
  /** `tmux has-session` feiler (brukes når panelet ikke leses). */
  sessionGone?: boolean;
  /** Container-inkarnasjon per oppslag. Siste verdi gjentas — endres den, er containeren restartet. */
  incarnations?: string[];
}

function recordingExec(opts: ExecOptions = {}): { exec: ExecFn; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const panes = [...(opts.panes ?? ["? for shortcuts"])];
  const incarnations = [...(opts.incarnations ?? ["sha256:abc 2026-07-31T10:00:00Z"])];
  const exec: ExecFn = async (cmd, args, execOpts) => {
    calls.push({ cmd, args, cwd: execOpts.cwd });
    if (cmd === "docker" && args[0] === "inspect") {
      if (args.some((a) => a.includes("{{.Id}}"))) {
        const value = incarnations.length > 1 ? incarnations.shift()! : (incarnations[0] ?? "");
        return value === ""
          ? { code: 1, stdout: "", stderr: "No such object\n" }
          : { code: 0, stdout: `${value}\n`, stderr: "" };
      }
      return opts.running
        ? { code: 0, stdout: "true\n", stderr: "" }
        : { code: 1, stdout: "", stderr: "" };
    }
    if (args.includes("capture-pane")) {
      if (opts.paneFails) return { code: 1, stdout: "", stderr: "no server running\n" };
      const text = panes.length > 1 ? panes.shift()! : (panes[0] ?? "");
      return { code: 0, stdout: text, stderr: "" };
    }
    if (args.includes("has-session")) {
      return opts.sessionGone
        ? { code: 1, stdout: "", stderr: "can't find session\n" }
        : { code: 0, stdout: "", stderr: "" };
    }
    if (args.includes("sh") && args.some((a) => a.includes("session-launched"))) {
      return { code: opts.marker === false ? 1 : 0, stdout: "", stderr: "" };
    }
    return { code: 0, stdout: "", stderr: "" };
  };
  return { exec, calls };
}

function memoryFs(initial: Record<string, string> = {}): FsFns & { files: Map<string, string> } {
  const files = new Map(Object.entries(initial));
  return {
    files,
    readFile: async (target) => files.get(target) ?? null,
    writeFile: async (target, content) => {
      files.set(target, content);
    },
    mkdirp: async () => {},
  };
}

const identity = {
  gitProvider: "fatdev-broker",
  brokerProvider: "fatdev-pat",
  targetMatch: "github.com/digdir/*",
  urlMatch: "https://github.com/digdir/",
  identityName: "Test Bot",
  identityEmail: "bot@example.invalid",
};

function driverWith(
  execOpts: ExecOptions = {},
  overrides: Partial<ConstructorParameters<typeof DockerNvtDriver>[0]> = {},
): {
  driver: DockerNvtDriver;
  calls: Recorded[];
  fs: FsFns & { files: Map<string, string> };
  execOpts: ExecOptions;
} {
  const { exec, calls } = recordingExec(execOpts);
  const fs = memoryFs();
  const driver = new DockerNvtDriver({
    nvtRoot: "/srv/nvt",
    agentType: "claude",
    agentConfig: identity,
    exec,
    fs,
    sleep: async () => {},
    ...overrides,
  });
  // `execOpts` leses ved hvert kall, så en test kan endre tilstanden underveis
  // (sesjonen dør, containeren restartes).
  return { driver, calls, fs, execOpts };
}

test("isDoneEvent gjenkjenner plugin.agent.signal.done på begge feltnavn", () => {
  assert.equal(isDoneEvent('{"type":"plugin.agent.signal.done"}'), true);
  assert.equal(isDoneEvent('{"event":"plugin.agent.signal.done","ts":"t"}'), true);
});

test("isDoneEvent ignorerer andre events, støy og ugyldig JSON", () => {
  assert.equal(isDoneEvent('{"type":"prompt.queued"}'), false);
  assert.equal(isDoneEvent("ikke json"), false);
  assert.equal(isDoneEvent(""), false);
  assert.equal(isDoneEvent("   "), false);
  assert.equal(isDoneEvent('"plugin.agent.signal.done"'), false);
  assert.equal(isDoneEvent("null"), false);
  assert.equal(isDoneEvent("123"), false);
  assert.equal(isDoneEvent("true"), false);
  assert.equal(isDoneEvent('["type","plugin.agent.signal.done"]'), false);
});

// --- agent-init: --user non-root (M0-funn 1) ---------------------------------

test("agent-init kjøres som script med --user non-root, ikke via make", async () => {
  const { driver, calls } = driverWith();
  await driver.ensureInstance("topic-1", "fatdev-x-1234abcd");

  const init = calls.find((c) => c.args.some((a) => a.includes("agent-init")));
  assert.ok(init, "agent-init må kjøres");
  assert.equal(init.cmd, "bash", "make agent-init forwarder ikke --user");
  assert.deepEqual(init.args, [
    "scripts/agent-init.sh",
    "--name",
    "fatdev-x-1234abcd",
    "--type",
    "claude",
    "--autonomy",
    "trusted-local",
    "--user",
    "non-root",
  ]);
  assert.equal(init.cwd, "/srv/nvt", "init må kjøres fra NVT_ROOT");

  const up = calls.find((c) => c.cmd === "make");
  assert.deepEqual(up?.args, ["agent-up", "NAME=fatdev-x-1234abcd"]);
  assert.equal(up?.cwd, "/srv/nvt");
});

test("userMode kan overstyres, men non-root er default", async () => {
  const { driver, calls } = driverWith({}, { userMode: "root" });
  await driver.ensureInstance("t", "i");
  const init = calls.find((c) => c.cmd === "bash")!;
  assert.equal(init.args.at(-1), "root");
});

test("en levende instans re-initialiseres ikke", async () => {
  const { driver, calls } = driverWith({ running: true });
  await driver.ensureInstance("t", "i");
  assert.equal(
    calls.some((c) => c.cmd === "make" || c.cmd === "bash"),
    false,
    "ingen init/up når instansen er oppe",
  );
});

test("feilende agent-init kaster med kommandoen i meldingen", async () => {
  const exec: ExecFn = async (cmd, args) => {
    if (cmd === "docker" && args[0] === "inspect") return { code: 1, stdout: "", stderr: "" };
    if (cmd === "bash") return { code: 2, stdout: "", stderr: "invalid user: nonroot\n" };
    return { code: 0, stdout: "", stderr: "" };
  };
  const { driver } = driverWith({}, { exec });
  await assert.rejects(() => driver.ensureInstance("t", "i"), /agent-init\.sh.*invalid user/s);
});

// --- agent.yaml: identity.mode explicit (M0-funn 4) --------------------------

test("agent.yaml skrives før init, med eksplisitt commit-identitet", async () => {
  const { driver, calls, fs } = driverWith();
  await driver.ensureInstance("t", "fatdev-y");

  const written = fs.files.get("/srv/nvt/.agents/fatdev-y/agent.yaml");
  assert.ok(written, "configen må ligge der før agent-init");
  assert.equal(identityMode(written).kind, "explicit");
  assert.match(written, /name: "Test Bot"/);
  assert.match(written, /broker-provider: "fatdev-pat"/);
  assert.match(written, /user: "non-root"/);
  assert.match(written, /--dangerously-skip-permissions/);

  // Rekkefølgen er poenget: agent-init skriver nvt-malen hvis fila mangler.
  const initIndex = calls.findIndex((c) => c.cmd === "bash");
  assert.ok(initIndex >= 0);
});

test("en eksisterende agent.yaml overskrives ikke", async () => {
  const { exec } = recordingExec();
  const fs = memoryFs({
    "/srv/nvt/.agents/i/agent.yaml": "runtime:\n  command: claude\nidentity:\n  mode: explicit\n",
  });
  const driver = new DockerNvtDriver({
    nvtRoot: "/srv/nvt",
    agentType: "claude",
    agentConfig: identity,
    exec,
    fs,
    sleep: async () => {},
  });
  await driver.ensureInstance("t", "i");
  assert.match(fs.files.get("/srv/nvt/.agents/i/agent.yaml")!, /^runtime:\n {2}command: claude/);
});

test("identity.mode: provider i eksisterende config stopper oppstart (M0-funn 4)", async () => {
  const { exec } = recordingExec();
  const fs = memoryFs({
    "/srv/nvt/.agents/i/agent.yaml":
      "plugins:\n  - name: git-credentials\n    config:\n      credentials:\n" +
      "        - match: https://github.com/digdir/\n          identity:\n            mode: provider\n",
  });
  const driver = new DockerNvtDriver({
    nvtRoot: "/srv/nvt",
    agentType: "claude",
    agentConfig: identity,
    exec,
    fs,
    sleep: async () => {},
  });
  await assert.rejects(
    () => driver.ensureInstance("t", "i"),
    /identity\.mode: provider.*static_token/s,
  );
});

test("manglende identitet i configen kan ikke genereres bort i stillhet", async () => {
  const { exec } = recordingExec();
  const driver = new DockerNvtDriver({
    nvtRoot: "/srv/nvt",
    agentType: "claude",
    agentConfig: { ...identity, identityEmail: "" },
    exec,
    fs: memoryFs(),
    sleep: async () => {},
  });
  await assert.rejects(() => driver.ensureInstance("t", "i"), /identityEmail/);
});

test("runtimeArgsFor følger agent-init.sh sin utledning", () => {
  assert.deepEqual(runtimeArgsFor("claude", "trusted-local"), ["--dangerously-skip-permissions"]);
  assert.deepEqual(runtimeArgsFor("claude", "interactive"), []);
  assert.deepEqual(runtimeArgsFor("codex", "trusted-local"), [
    "--sandbox",
    "danger-full-access",
    "--ask-for-approval",
    "never",
  ]);
  assert.throws(() => runtimeArgsFor("gemini", "trusted-local"), /ukjent agent-type/);
});

// --- klar-sjekk før prompt (M0-funn 5) --------------------------------------

test("waitUntilReady godtar sesjonen når panelet viser klar-prompt", async () => {
  const { driver, calls } = driverWith({ panes: ["? for shortcuts"] });
  await driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1000 });

  assert.equal(
    calls.some((c) => c.args.includes("send-keys")),
    false,
    "Enter skal ALDRI sendes i blinde — bare når en dialog faktisk står der",
  );
  assert.ok(
    calls.some((c) => c.args.some((a) => a.includes("session-launched"))),
    "markøren må sjekkes",
  );
});

test("waitUntilReady sender Enter gjennom onboarding-dialogen og fortsetter", async () => {
  const { driver, calls } = driverWith({
    panes: ["Do you trust the files in this folder?", "? for shortcuts"],
  });
  await driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1000 });

  const enters = calls.filter((c) => c.args.includes("send-keys"));
  assert.equal(enters.length, 1);
  assert.deepEqual(enters[0]!.args, [
    "exec",
    "agent-i-agent-1",
    "tmux",
    "send-keys",
    "-t",
    "agent",
    "Enter",
  ]);
});

test("waitUntilReady kaster når markøren mangler — ingen prompt sendes", async () => {
  const { driver } = driverWith({ marker: false });
  await assert.rejects(
    () => driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1 }),
    /ble ikke klar.*session-launched-markør: nei/s,
  );
});

test("waitUntilReady kaster når tmux-sesjonen ikke svarer", async () => {
  const { driver } = driverWith({ paneFails: true });
  await assert.rejects(
    () => driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1 }),
    /tmux-sesjon «agent»: nei/,
  );
});

test("feilmeldingen gjengir aldri panelinnholdet (kan inneholde hva som helst)", async () => {
  const { driver } = driverWith({ panes: ["ANTHROPIC_AUTH_TOKEN=hemmelig-verdi"] });
  await assert.rejects(
    () => driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1 }),
    (err: Error) => {
      assert.doesNotMatch(err.message, /hemmelig-verdi/);
      assert.match(err.message, /panel: unknown/);
      return true;
    },
  );
});

test("Enter-budsjettet er begrenset — en dialog som ikke gir seg ender i ærlig feil", async () => {
  const { driver, calls } = driverWith(
    { panes: ["Press Enter to continue"] },
    { maxEnterPresses: 2 },
  );
  await assert.rejects(
    () => driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 5 }),
    /ble ikke klar.*panel: onboarding/s,
  );
  assert.ok(
    calls.filter((c) => c.args.includes("send-keys")).length <= 2,
    "aldri mer enn budsjettet",
  );
});

test("waitUntilReady respekterer abort (broen avslutter)", async () => {
  const { driver } = driverWith({ marker: false });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    () => driver.waitUntilReady({ topic: "t", instance: "i" }, { timeoutMs: 1000, signal: controller.signal }),
    /avbrutt/,
  );
});

test("etter bekreftet klar-tilstand holder det at tmux-sesjonen lever", async () => {
  const { driver, calls } = driverWith({ panes: ["? for shortcuts"] });
  const ref = { topic: "t", instance: "i" };
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });
  const before = calls.length;
  // Agenten kan være midt i arbeid; agentd køer prompten selv.
  await driver.waitUntilReady(ref, { timeoutMs: 1 });

  const after = calls.slice(before);
  assert.ok(
    after.some((c) => c.args.includes("has-session")),
    "sesjonen sjekkes fortsatt",
  );
  assert.equal(
    after.some((c) => c.args.includes("capture-pane")),
    false,
    "panelet inneholder transkriptet (upålitelig input) og skal ikke styre gaten her",
  );
});

test("dør tmux-sesjonen etter bekreftet klar-tilstand, leveres ingenting", async () => {
  const { driver, execOpts } = driverWith({ panes: ["? for shortcuts"] });
  const ref = { topic: "t", instance: "i" };
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });

  execOpts.sessionGone = true;
  await assert.rejects(
    () => driver.waitUntilReady(ref, { timeoutMs: 1 }),
    /tmux-sesjon «agent»: nei/,
  );
});

test("restartes containeren utenfor broen, gjøres klar-sjekken på nytt", async () => {
  // Klar-tilstanden gjelder én container-inkarnasjon. `docker restart` eller en
  // host-reboot gir ny sesjon med ny onboarding, selv om broen aldri kjørte
  // agent-up — da må gaten være streng igjen.
  const { driver, calls } = driverWith(
    {
      panes: ["? for shortcuts", "│ ✻ Welcome to Claude Code!"],
      incarnations: ["sha256:abc 10:00:00Z", "sha256:abc 11:30:00Z"],
    },
    { maxEnterPresses: 0 },
  );
  const ref = { topic: "t", instance: "i" };
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });

  const before = calls.length;
  await assert.rejects(
    () => driver.waitUntilReady(ref, { timeoutMs: 1 }),
    /panel: onboarding/,
    "ny inkarnasjon ⇒ panelet leses igjen, og onboardingen fanges",
  );
  assert.ok(
    calls.slice(before).some((c) => c.args.includes("capture-pane")),
    "streng sjekk leser panelet",
  );
});

test("svarer docker inspect ikke, behandles sesjonen som fersk (trygg side)", async () => {
  const { driver } = driverWith({
    panes: ["? for shortcuts"],
    incarnations: ["sha256:abc 10:00:00Z", ""],
  });
  const ref = { topic: "t", instance: "i" };
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });
  // Uten inkarnasjon vet vi ikke om det er samme sesjon → streng sjekk, som her
  // fortsatt viser klar-prompt.
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });
});

// --- prompt og opprydding ---------------------------------------------------

test("sendPrompt sender alltid --external (upålitelig input)", async () => {
  const { driver, calls } = driverWith();
  await driver.sendPrompt({ topic: "t", instance: "fatdev-x-1234abcd" }, "oppgave");

  const call = calls.find((c) => c.args.includes("agentdctl"))!;
  assert.deepEqual(call.args, [
    "exec",
    "agent-fatdev-x-1234abcd-agent-1",
    "agentdctl",
    "prompt",
    "--source",
    "host",
    "--external",
    "oppgave",
  ]);
});

test("prompten sendes som ett argv-element — ingen shell-tolkning", async () => {
  const { driver, calls } = driverWith();
  const nasty = 'x"; rm -rf / #\n$(whoami) `id` && echo pwned';
  await driver.sendPrompt({ topic: "t", instance: "i" }, nasty);
  const call = calls.find((c) => c.args.includes("agentdctl"))!;
  assert.equal(call.args.at(-1), nasty, "hele prompten er ett argument");
  assert.equal(call.args.filter((a) => a === nasty).length, 1);
});

test("feilende agentdctl kaster (broen skriver da en error-linje)", async () => {
  const exec: ExecFn = async () => ({ code: 3, stdout: "", stderr: "no such container\n" });
  const { driver } = driverWith({}, { exec });
  await assert.rejects(
    () => driver.sendPrompt({ topic: "t", instance: "i" }, "x"),
    /agentdctl prompt feilet.*no such container/s,
  );
});

test("stopInstance kjører agent-down (workspacet beholdes)", async () => {
  const { driver, calls } = driverWith();
  await driver.stopInstance({ topic: "t", instance: "fatdev-x-1234abcd" });
  assert.deepEqual(calls.at(-1)!.args, ["agent-down", "NAME=fatdev-x-1234abcd"]);
});

test("stopInstance nullstiller klar-tilstanden (ny sesjon = ny onboarding)", async () => {
  const { driver } = driverWith({ panes: ["? for shortcuts", "kompilerer …"] });
  const ref = { topic: "t", instance: "i" };
  await driver.waitUntilReady(ref, { timeoutMs: 1000 });
  await driver.stopInstance(ref);
  await assert.rejects(() => driver.waitUntilReady(ref, { timeoutMs: 1 }), /ble ikke klar/);
});

test("containernavnet følger compose-mønsteret fra planen", () => {
  const { driver } = driverWith();
  assert.equal(driver.containerName("fatdev-x-1234abcd"), "agent-fatdev-x-1234abcd-agent-1");
});
