import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { NotifiedStore } from "./notifiedStore.ts";

async function tempStateDir(): Promise<string> {
  return mkdtemp(path.join(tmpdir(), "review-notify-"));
}

test("PR som ikke er markert varslet er ikke i store", async () => {
  const store = new NotifiedStore(await tempStateDir());
  await store.load();
  assert.equal(store.has("digdir/digdir-ai-agents#1"), false);
});

test("markNotified gjør has() true for samme kjøring", async () => {
  const store = new NotifiedStore(await tempStateDir());
  await store.load();
  await store.markNotified("digdir/digdir-ai-agents#1");
  assert.equal(store.has("digdir/digdir-ai-agents#1"), true);
});

test("state overlever omstart (leses tilbake fra disk)", async () => {
  const dir = await tempStateDir();
  const store = new NotifiedStore(dir);
  await store.load();
  await store.markNotified("digdir/digdir-ai-agents#1");

  const reopened = new NotifiedStore(dir);
  await reopened.load();
  assert.equal(reopened.has("digdir/digdir-ai-agents#1"), true);
  assert.equal(reopened.has("digdir/digdir-ai-agents#2"), false);
});

test("manglende state-fil er ikke en feil — starter tom", async () => {
  const store = new NotifiedStore(path.join(await tempStateDir(), "does-not-exist"));
  await store.load();
  assert.equal(store.has("digdir/digdir-ai-agents#1"), false);
});

test("korrupt state-fil starter tomt i stedet for å krasje", async () => {
  const dir = await tempStateDir();
  const store = new NotifiedStore(dir);
  await store.load();
  await store.markNotified("digdir/digdir-ai-agents#1");
  await writeFile(store.file, "{ikke json", "utf-8");

  const reopened = new NotifiedStore(dir);
  await reopened.load();
  assert.equal(reopened.has("digdir/digdir-ai-agents#1"), false);
});

test("review-notified.json er gyldig, lesbar JSON", async () => {
  const dir = await tempStateDir();
  const store = new NotifiedStore(dir);
  await store.load();
  await store.markNotified("digdir/digdir-ai-agents#1");
  const parsed = JSON.parse(await readFile(store.file, "utf-8"));
  assert.deepEqual(parsed, ["digdir/digdir-ai-agents#1"]);
});
