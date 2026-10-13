import assert from "node:assert/strict";
import { test } from "node:test";
import { AgentctlDriver, parseSessions, type PromptFileFns } from "./agentctl.ts";
import type { ExecFn } from "./docker.ts";

/**
 * Driveren er kalibrert mot agentctl-kilden (v0.1.0-preview.10), men ikke
 * kjørt mot en levende agent. Testene dekker det som er ren logikk:
 * kommandoene som bygges, session-parsingen, unarchive-flyten i
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
}

function recordingExec(opts: ExecOptions = {}): { exec: ExecFn; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const lists = [...(opts.sessionLists ?? ["[]"])];
  const exec: ExecFn = async (cmd, args, execOpts) => {
    calls.push({ cmd, args, cwd: execOpts.cwd });
    if (args[0] === "get" && args[1] === "sessions") {
      const body = lists.length > 1 ? lists.shift()! : (lists[0] ?? "[]");
      return { code: 0, stdout: body, stderr: "" };
    }
    if (args[0] === "create" && opts.createFails) {
      return { code: 1, stdout: "", stderr: `${opts.createFails}\n` };
    }
    return { code: 0, stdout: "", stderr: "" };
  };
  return { exec, calls };
}

const fakeFiles = () => {
  const written: Array<{ target: string; content: string }> = [];
  const removed: string[] = [];
  const files: PromptFileFns = {
    write: async (target, content) => {
      written.push({ target, content });
    },
    remove: async (target) => {
      removed.push(target);
    },
  };
  return { files, written, removed };
};

function driverWith(
  exec: ExecFn,
  extra: Partial<ConstructorParameters<typeof AgentctlDriver>[0]> = {},
) {
  return new AgentctlDriver({
    agentDir: "/agents/jr-sandbox",
    agentName: "jr-sandbox",
    exec,
    files: fakeFiles().files,
    sleep: async () => {},
    ...extra,
  });
}

const sessionsJson = (entries: Array<{ name: string; state: string }>) =>
  JSON.stringify(entries.map(({ name, state }) => ({ name, agent: "jr-sandbox", status: { state } })));

test("parseSessions: array av Session-objekter med status.state (kildens format)", () => {
  const out = parseSessions(sessionsJson([{ name: "jr-a1b2", state: "working" }]));
  assert.deepEqual(out, [{ name: "jr-a1b2", state: "working" }]);
});

test("parseSessions: tolererer innpakking og alternative feltnavn", () => {
  const wrapped = JSON.stringify({
    sessions: [
      { metadata: { name: "x" }, status: { state: "idle" } },
      { name: "y", state: "waitingForInput" },
      { uten: "navn" },
    ],
  });
  assert.deepEqual(parseSessions(wrapped), [
    { name: "x", state: "idle" },
    { name: "y", state: "waitingForInput" },
  ]);
});

test("parseSessions: søppel gir tom liste, aldri kast", () => {
  assert.deepEqual(parseSessions("not json"), []);
  assert.deepEqual(parseSessions('{"sessions": "nope"}'), []);
});

test("ensureInstance: apply én gang, create med --agent <navn>, cwd = agentDir, --archived i listing", async () => {
  const { exec, calls } = recordingExec({ sessionLists: ["[]"] });
  const driver = driverWith(exec);
  await driver.ensureInstance("topic-x", "jr-x");
  await driver.ensureInstance("topic-x", "jr-x");

  const applies = calls.filter((c) => c.args[0] === "apply");
  assert.equal(applies.length, 1, "apply skal kjøres én gang per prosess");
  assert.deepEqual(applies[0]?.args, ["apply", "--wait"]);
  assert.equal(applies[0]?.cwd, "/agents/jr-sandbox");

  const listing = calls.find((c) => c.args[0] === "get");
  assert.deepEqual(listing?.args, [
    "get",
    "sessions",
    "-o",
    "json",
    "--agent",
    "jr-sandbox",
    "--archived",
  ]);

  // create er ensure-semantikk og kjøres per kall (idempotent).
  const creates = calls.filter((c) => c.args[0] === "create");
  assert.equal(creates.length, 2);
  assert.deepEqual(creates[0]?.args, ["create", "session/jr-x", "--agent", "jr-sandbox"]);
});

test("ensureInstance: arkivert sesjon unarchives før create (TTL-gjenopptak)", async () => {
  const { exec, calls } = recordingExec({
    sessionLists: [sessionsJson([{ name: "jr-x", state: "archived" }])],
  });
  await driverWith(exec).ensureInstance("t", "jr-x");
  assert.deepEqual(
    calls.filter((c) => ["unarchive", "create"].includes(String(c.args[0]))).map((c) => c.args),
    [
      ["unarchive", "session/jr-x", "--agent", "jr-sandbox"],
      ["create", "session/jr-x", "--agent", "jr-sandbox"],
    ],
  );
});

test("ensureInstance: create-feil tilgis når sesjonen finnes aktiv etterpå (kappløp)", async () => {
  const { exec } = recordingExec({
    createFails: "already exists",
    sessionLists: ["[]", sessionsJson([{ name: "jr-x", state: "waitingForInput" }])],
  });
  await assert.doesNotReject(driverWith(exec).ensureInstance("t", "jr-x"));
});

test("ensureInstance: create-feil kastes når sesjonen fortsatt mangler", async () => {
  const { exec } = recordingExec({ createFails: "boom", sessionLists: ["[]"] });
  await assert.rejects(driverWith(exec).ensureInstance("t", "jr-x"), /create session\/jr-x/);
});

test("waitUntilReady: ok for aktiv sesjon, kaster ved failed/aldri-klar", async () => {
  const ok = recordingExec({
    sessionLists: [sessionsJson([{ name: "jr-x", state: "waitingForInput" }])],
  });
  await assert.doesNotReject(
    driverWith(ok.exec).waitUntilReady({ topic: "t", instance: "jr-x" }, { timeoutMs: 10_000 }),
  );

  const failed = recordingExec({
    sessionLists: [sessionsJson([{ name: "jr-x", state: "failed" }])],
  });
  let now = 0;
  const driver = driverWith(failed.exec, {
    sleep: async (ms) => {
      now += ms;
    },
  });
  const origNow = Date.now;
  Date.now = () => now;
  try {
    await assert.rejects(
      driver.waitUntilReady({ topic: "t", instance: "jr-x" }, { timeoutMs: 5000 }),
      /ble ikke klar innen 5s.*failed/,
    );
  } finally {
    Date.now = origNow;
  }
});

test("sendPrompt: skriver promptfil, sender med --file, rydder fila", async () => {
  const { exec, calls } = recordingExec();
  const tmp = fakeFiles();
  const driver = driverWith(exec, { files: tmp.files, tmpDir: "/tmp-test" });
  await driver.sendPrompt({ topic: "t", instance: "jr-x" }, "oppgavetekst");

  assert.equal(tmp.written.length, 1);
  assert.equal(tmp.written[0]?.content, "oppgavetekst");
  // path.join bruker plattformens separator — sjekk katalog og prefiks løst.
  assert.match(tmp.written[0]?.target ?? "", /tmp-test[/\\]nvt-bridge-prompt-jr-x-/);

  const prompt = calls.find((c) => c.args[0] === "prompt");
  assert.deepEqual(prompt?.args.slice(0, 5), [
    "prompt",
    "session/jr-x",
    "--agent",
    "jr-sandbox",
    "--file",
  ]);
  assert.equal(prompt?.args[5], tmp.written[0]?.target);
  assert.deepEqual(tmp.removed, [tmp.written[0]?.target], "tmp-fila skal ryddes");
});

test("sendPrompt: feil propageres, og tmp-fila ryddes likevel", async () => {
  const failing: ExecFn = async (_cmd, args) =>
    args[0] === "prompt"
      ? { code: 1, stdout: "", stderr: "nope\n" }
      : { code: 0, stdout: "[]", stderr: "" };
  const tmp = fakeFiles();
  await assert.rejects(
    driverWith(failing, { files: tmp.files }).sendPrompt({ topic: "t", instance: "jr-x" }, "x"),
    /prompt session\/jr-x .* feilet \(exit 1\): nope/,
  );
  assert.equal(tmp.removed.length, 1);
});

test("waitForDone: working → to rolige polls = done", async () => {
  const { exec } = recordingExec({
    sessionLists: [
      sessionsJson([{ name: "jr-x", state: "working" }]),
      sessionsJson([{ name: "jr-x", state: "working" }]),
      sessionsJson([{ name: "jr-x", state: "waitingForInput" }]),
      sessionsJson([{ name: "jr-x", state: "idle" }]),
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
    sessionLists: [sessionsJson([{ name: "jr-x", state: "waitingForInput" }])],
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

test("waitForDone: starting/failed/borte er aldri done — timeout", async () => {
  for (const list of ["[]", sessionsJson([{ name: "jr-x", state: "starting" }]), sessionsJson([{ name: "jr-x", state: "failed" }])]) {
    let now = 0;
    const { exec } = recordingExec({ sessionLists: [list] });
    const driver = driverWith(exec, {
      sleep: async (ms) => {
        now += ms;
      },
    });
    const origNow = Date.now;
    Date.now = () => now;
    try {
      const outcome = await driver.waitForDone({ topic: "t", instance: "jr-x" }, { timeoutMs: 8000 });
      assert.equal(outcome.kind, "timeout", `liste ${list} skal gi timeout`);
    } finally {
      Date.now = origNow;
    }
  }
});

test("stopInstance: archive med --agent <navn>, feil propageres", async () => {
  const { exec, calls } = recordingExec();
  await driverWith(exec).stopInstance({ topic: "t", instance: "jr-x" });
  assert.deepEqual(calls.at(-1)?.args, ["archive", "session/jr-x", "--agent", "jr-sandbox"]);

  const failing: ExecFn = async () => ({ code: 1, stdout: "", stderr: "nope\n" });
  await assert.rejects(
    driverWith(failing).stopInstance({ topic: "t", instance: "jr-x" }),
    /archive session\/jr-x .* feilet/,
  );
});
