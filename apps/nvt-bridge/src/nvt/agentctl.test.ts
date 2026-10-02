import assert from "node:assert/strict";
import { test } from "node:test";
import { AgentctlDriver, parseSessions, promptArgs } from "./agentctl.ts";
import type { ExecFn } from "./docker.ts";

/**
 * Driveren er UKALIBRERT mot ekte agentctl (F1); testene dekker det som er
 * ren logikk: kommandoene som bygges, session-parsingen, idempotensen i
 * ensureInstance og tilstandsmaskinen i waitForDone.
 */

interface Recorded {
  cmd: string;
  args: string[];
  cwd?: string;
}

interface ExecOptions {
  /** Svar fra `get sessions -o json` per oppslag. Siste verdi gjentas. */
  sessionLists?: string[];
  /** `create` feiler med denne meldingen. */
  createFails?: string;
  /** `apply` feiler. */
  applyFails?: boolean;
}

function recordingExec(opts: ExecOptions = {}): { exec: ExecFn; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const lists = [...(opts.sessionLists ?? ["[]"])];
  const exec: ExecFn = async (cmd, args, execOpts) => {
    calls.push({ cmd, args, cwd: execOpts.cwd });
    if (args[0] === "apply") {
      return opts.applyFails
        ? { code: 1, stdout: "", stderr: "apply: no manifest\n" }
        : { code: 0, stdout: "", stderr: "" };
    }
    if (args[0] === "get" && args[1] === "sessions") {
      const body = lists.length > 1 ? lists.shift()! : (lists[0] ?? "[]");
      return { code: 0, stdout: body, stderr: "" };
    }
    if (args[0] === "create") {
      return opts.createFails
        ? { code: 1, stdout: "", stderr: `${opts.createFails}\n` }
        : { code: 0, stdout: "", stderr: "" };
    }
    return { code: 0, stdout: "", stderr: "" };
  };
  return { exec, calls };
}

function driverWith(exec: ExecFn, extra: Partial<ConstructorParameters<typeof AgentctlDriver>[0]> = {}) {
  return new AgentctlDriver({
    agentDir: "/agents/jr-sandbox",
    agentName: "jr-sandbox",
    exec,
    sleep: async () => {},
    ...extra,
  });
}

const sessionsJson = (entries: Array<{ name: string; state: string }>) => JSON.stringify(entries);

test("parseSessions: toppnivå-array med name/state", () => {
  const out = parseSessions(sessionsJson([{ name: "jr-a1b2", state: "Working" }]));
  assert.deepEqual(out, [{ name: "jr-a1b2", state: "Working" }]);
});

test("parseSessions: tolererer innpakking og alternative feltnavn", () => {
  const wrapped = JSON.stringify({
    sessions: [
      { metadata: { name: "x" }, status: { state: "Idle" } },
      { name: "y", status: "WaitingForInput" },
      { uten: "navn" },
    ],
  });
  assert.deepEqual(parseSessions(wrapped), [
    { name: "x", state: "Idle" },
    { name: "y", state: "WaitingForInput" },
  ]);
});

test("parseSessions: søppel gir tom liste, aldri kast", () => {
  assert.deepEqual(parseSessions("not json"), []);
  assert.deepEqual(parseSessions('{"sessions": "nope"}'), []);
});

test("promptArgs: prompt som posisjonsargument med eksplisitt scope", () => {
  assert.deepEqual(promptArgs("jr-x", "jr-sandbox", "gjør ting"), [
    "prompt",
    "session/jr-x",
    "--agent",
    "agent/jr-sandbox",
    "gjør ting",
  ]);
});

test("ensureInstance: apply én gang, create når sesjonen mangler, cwd = agentDir", async () => {
  const { exec, calls } = recordingExec({ sessionLists: ["[]", sessionsJson([{ name: "jr-x", state: "Idle" }])] });
  const driver = driverWith(exec);
  await driver.ensureInstance("topic-x", "jr-x");
  await driver.ensureInstance("topic-x", "jr-x");

  const applies = calls.filter((c) => c.args[0] === "apply");
  assert.equal(applies.length, 1, "apply skal kjøres én gang per prosess");
  assert.equal(applies[0]?.cwd, "/agents/jr-sandbox");
  const creates = calls.filter((c) => c.args[0] === "create");
  assert.deepEqual(creates.map((c) => c.args), [
    ["create", "session/jr-x", "--agent", "agent/jr-sandbox"],
  ]);
});

test("ensureInstance: create-feil tilgis når sesjonen finnes etterpå (kappløp)", async () => {
  const { exec } = recordingExec({
    createFails: "already exists",
    sessionLists: ["[]", sessionsJson([{ name: "jr-x", state: "Idle" }])],
  });
  const driver = driverWith(exec);
  await assert.doesNotReject(driver.ensureInstance("t", "jr-x"));
});

test("ensureInstance: create-feil kastes når sesjonen fortsatt mangler", async () => {
  const { exec } = recordingExec({ createFails: "boom", sessionLists: ["[]"] });
  const driver = driverWith(exec);
  await assert.rejects(driver.ensureInstance("t", "jr-x"), /create session\/jr-x/);
});

test("waitUntilReady: returnerer når sesjonen dukker opp, kaster ved timeout", async () => {
  const appears = recordingExec({
    sessionLists: ["[]", sessionsJson([{ name: "jr-x", state: "Idle" }])],
  });
  await assert.doesNotReject(
    driverWith(appears.exec).waitUntilReady({ topic: "t", instance: "jr-x" }, { timeoutMs: 10_000 }),
  );

  const never = recordingExec({ sessionLists: ["[]"] });
  let now = 0;
  const driver = driverWith(never.exec, {
    sleep: async (ms) => {
      now += ms;
    },
  });
  const origNow = Date.now;
  Date.now = () => now;
  try {
    await assert.rejects(
      driver.waitUntilReady({ topic: "t", instance: "jr-x" }, { timeoutMs: 5000 }),
      /ble ikke klar innen 5s/,
    );
  } finally {
    Date.now = origNow;
  }
});

test("waitForDone: Working → to rolige polls = done", async () => {
  const { exec } = recordingExec({
    sessionLists: [
      sessionsJson([{ name: "jr-x", state: "Working" }]),
      sessionsJson([{ name: "jr-x", state: "Working" }]),
      sessionsJson([{ name: "jr-x", state: "WaitingForInput" }]),
      sessionsJson([{ name: "jr-x", state: "Idle" }]),
    ],
  });
  const outcome = await driverWith(exec).waitForDone(
    { topic: "t", instance: "jr-x" },
    { timeoutMs: 60_000 },
  );
  assert.equal(outcome.kind, "done");
});

test("waitForDone: rolig sesjon FØR settle-vinduet er ikke done; etter vinduet er den det", async () => {
  let now = 0;
  const { exec } = recordingExec({
    sessionLists: [sessionsJson([{ name: "jr-x", state: "Idle" }])],
  });
  const driver = driverWith(exec, {
    settleTimeoutMs: 10_000,
    donePollMs: 4000,
    sleep: async (ms) => {
      now += ms;
    },
  });
  const origNow = Date.now;
  Date.now = () => now;
  try {
    const outcome = await driver.waitForDone({ topic: "t", instance: "jr-x" }, { timeoutMs: 60_000 });
    assert.equal(outcome.kind, "done");
    assert.ok(now >= 10_000, `done kom før settle-vinduet (t=${now}ms)`);
  } finally {
    Date.now = origNow;
  }
});

test("waitForDone: sesjon borte fra lista gir timeout, aldri done", async () => {
  let now = 0;
  const { exec } = recordingExec({ sessionLists: ["[]"] });
  const driver = driverWith(exec, {
    sleep: async (ms) => {
      now += ms;
    },
  });
  const origNow = Date.now;
  Date.now = () => now;
  try {
    const outcome = await driver.waitForDone({ topic: "t", instance: "jr-x" }, { timeoutMs: 8000 });
    assert.equal(outcome.kind, "timeout");
  } finally {
    Date.now = origNow;
  }
});

test("sendPrompt og stopInstance: riktige kommandoer, feil propageres", async () => {
  const { exec, calls } = recordingExec({ sessionLists: ["[]"] });
  const driver = driverWith(exec);
  await driver.sendPrompt({ topic: "t", instance: "jr-x" }, "oppgave");
  await driver.stopInstance({ topic: "t", instance: "jr-x" });
  assert.deepEqual(
    calls.map((c) => c.args[0]),
    ["prompt", "archive"],
  );

  const failing: ExecFn = async () => ({ code: 1, stdout: "", stderr: "nope\n" });
  await assert.rejects(
    driverWith(failing).sendPrompt({ topic: "t", instance: "jr-x" }, "oppgave"),
    /prompt session\/jr-x .* feilet \(exit 1\): nope/,
  );
});
