import assert from "node:assert/strict";
import { appendFile, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { pendingEvents, TriggerFiles } from "./triggers.ts";
import type { QueueEvent } from "./types.ts";

async function tempTriggers(): Promise<TriggerFiles> {
  const dir = await mkdtemp(path.join(tmpdir(), "nvt-bridge-"));
  const triggers = new TriggerFiles(dir);
  await triggers.ensureDirs();
  return triggers;
}

function evt(id: string): QueueEvent {
  return {
    id,
    source: "agent",
    type: "delegation",
    received_at: "2026-07-29T08:10:33.246Z",
    prompt: "p",
  };
}

test("dedupe: ubehandlet = id uten linje i results.jsonl", () => {
  const inbox = [evt("a"), evt("b"), evt("c")];
  const done = new Set(["b"]);
  assert.deepEqual(
    pendingEvents(inbox, done).map((e) => e.id),
    ["a", "c"],
  );
});

test("dedupe: events under arbeid dispatches ikke på nytt", () => {
  const inbox = [evt("a"), evt("b")];
  assert.deepEqual(
    pendingEvents(inbox, new Set(), new Set(["a"])).map((e) => e.id),
    ["b"],
  );
});

test("dedupe: alt kvittert ut gir ingenting å gjøre", () => {
  assert.deepEqual(pendingEvents([evt("a")], new Set(["a"])), []);
});

test("manglende filer gir tom innboks, ikke feil", async () => {
  const triggers = new TriggerFiles(path.join(tmpdir(), "nvt-bridge-finnes-ikke"));
  assert.deepEqual(await triggers.readInbox(), []);
  assert.deepEqual([...(await triggers.readResultIds())], []);
});

test("ufullstendig siste linje ignoreres (skriver kan være midt i en append)", async () => {
  const triggers = await tempTriggers();
  await writeFile(
    triggers.inboxFile,
    JSON.stringify(evt("a")) + "\n" + '{"id":"b","source":"agen',
    "utf8",
  );
  assert.deepEqual((await triggers.readInbox()).map((e) => e.id), ["a"]);
});

test("ugyldig JSON-linje hoppes over uten å stoppe strømmen", async () => {
  const triggers = await tempTriggers();
  await writeFile(
    triggers.inboxFile,
    ["{ikke json}", JSON.stringify(evt("a")), "", JSON.stringify(evt("b"))].join("\n") + "\n",
    "utf8",
  );
  assert.deepEqual((await triggers.readInbox()).map((e) => e.id), ["a", "b"]);
});

test("events uten id hoppes over — de kan ikke kvitteres ut", async () => {
  const triggers = await tempTriggers();
  await writeFile(
    triggers.inboxFile,
    ['{"source":"agent","prompt":"x"}', JSON.stringify(evt("a"))].join("\n") + "\n",
    "utf8",
  );
  assert.deepEqual((await triggers.readInbox()).map((e) => e.id), ["a"]);
});

test("resultatlinjer skrives som én komplett linje med avsluttende linjeskift", async () => {
  const triggers = await tempTriggers();
  await triggers.appendResult({ id: "a", status: "ok", reply: "ferdig" });
  await triggers.appendResult({ id: "b", status: "error", reply: "feilet" });
  const { readFile } = await import("node:fs/promises");
  const text = await readFile(triggers.resultsFile, "utf8");
  assert.ok(text.endsWith("\n"));
  assert.equal(text.trimEnd().split("\n").length, 2);
  // Ingen linjeskift inne i en linje — integrations leser til siste \n.
  for (const line of text.trimEnd().split("\n")) {
    assert.doesNotThrow(() => JSON.parse(line));
  }
});

test("hasResult ser linjer skrevet av agenten i instansen", async () => {
  const triggers = await tempTriggers();
  assert.equal(await triggers.hasResult("a"), false);
  await appendFile(triggers.resultsFile, JSON.stringify({ id: "a", status: "ok" }) + "\n", "utf8");
  assert.equal(await triggers.hasResult("a"), true);
});

test("manglende linjeskift fra agenten repareres før vår linje skrives", async () => {
  // Regresjon: agenten skriver resultatlinja frihånds. Uten linjeskift ville
  // vår append havnet på slutten av dens linje og gjort BEGGE uleselige —
  // eventet ville stått ubehandlet for alltid.
  const triggers = await tempTriggers();
  const { writeFile, readFile } = await import("node:fs/promises");
  await writeFile(triggers.resultsFile, JSON.stringify({ id: "a", status: "ok" }), "utf8");

  await triggers.appendResult({ id: "b", status: "error" });

  const lines = (await readFile(triggers.resultsFile, "utf8")).trimEnd().split("\n");
  assert.equal(lines.length, 2);
  assert.deepEqual(
    lines.map((l) => (JSON.parse(l) as { id: string }).id),
    ["a", "b"],
  );
});

test("log-stien saneres — en id med ../ skriver ikke utenfor triggers/", async () => {
  const triggers = await tempTriggers();
  const rel = triggers.bridgeLogRelPath("slack-C1/../../../etc-x");
  assert.equal(rel, "logs/slack-C1_.._.._.._etc-x.bridge.log");
  assert.ok(!rel.includes("/../"), "ingen sti-escape");
  // integrations avviser log-verdier utenfor agentens triggers-katalog.
  assert.ok(rel.startsWith("logs/"));

  await triggers.appendBridgeLog("slack-C1/../../../etc-x", "hei");
  const { readFile } = await import("node:fs/promises");
  const written = await readFile(path.join(triggers.triggersDir, rel), "utf8");
  assert.match(written, /hei/);
});

test("en feilende bridge-logg kaster ikke (resultatlinja er viktigere)", async () => {
  // logs/ er en FIL, ikke en katalog → ENOTDIR ved skriving.
  const dir = await mkdtemp(path.join(tmpdir(), "nvt-bridge-log-"));
  await writeFile(path.join(dir, "logs"), "blokkerer", "utf8");
  const triggers = new TriggerFiles(dir);
  await assert.doesNotReject(() => triggers.appendBridgeLog("e1", "hei"));
});

test("bridge-loggen har eget suffiks så den ikke kolliderer med agentens logg", async () => {
  const triggers = await tempTriggers();
  assert.equal(triggers.bridgeLogRelPath("evt-1"), "logs/evt-1.bridge.log");
  await triggers.appendBridgeLog("evt-1", "hei");
  const { readFile } = await import("node:fs/promises");
  const text = await readFile(path.join(triggers.triggersDir, "logs", "evt-1.bridge.log"), "utf8");
  assert.match(text, /] hei\n$/);
});
