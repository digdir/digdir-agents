import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { TopicStore } from "./state.ts";

async function tempStore(): Promise<TopicStore> {
  const dir = await mkdtemp(path.join(tmpdir(), "nvt-bridge-s-"));
  const store = new TopicStore(path.join(dir, "state"));
  await store.load();
  return store;
}

test("upsert registrerer topic → instans og beholder den ved gjentak", async () => {
  const store = await tempStore();
  const first = await store.upsert("topic-1", "fatdev-topic-1-aabbccdd");
  const again = await store.upsert("topic-1", "et-annet-navn");
  assert.equal(again.instance, first.instance, "instansen skal ikke bytte navn");
});

test("state overlever omstart (topics.json leses tilbake)", async () => {
  const store = await tempStore();
  await store.upsert("topic-1", "fatdev-x-1234abcd");
  await store.markPrompted("topic-1", "e1");

  const reopened = new TopicStore(store.stateDir);
  await reopened.load();
  const record = reopened.get("topic-1");
  assert.equal(record?.instance, "fatdev-x-1234abcd");
  assert.equal(record?.prompts, 1);
  assert.equal(record?.last_event_id, "e1");
});

test("topics.json er gyldig, lesbar JSON", async () => {
  const store = await tempStore();
  await store.upsert("topic-1", "fatdev-x-1234abcd");
  const parsed = JSON.parse(await readFile(store.file, "utf8"));
  assert.equal(parsed.version, 1);
  assert.equal(parsed.topics["topic-1"].instance, "fatdev-x-1234abcd");
});

test("korrupt state-fil starter tomt i stedet for å krasje", async () => {
  const store = await tempStore();
  await store.upsert("topic-1", "fatdev-x-1234abcd");
  await writeFile(store.file, "{ikke json", "utf8");

  const reopened = new TopicStore(store.stateDir);
  await reopened.load();
  assert.equal(reopened.get("topic-1"), undefined);
});

test("manglende state-fil er ikke en feil", async () => {
  const store = new TopicStore(path.join(tmpdir(), "nvt-bridge-finnes-ikke", "state"));
  await store.load();
  assert.deepEqual(store.entries(), []);
});

test("idleTopics finner kun topics som har vært stille lenge nok", async () => {
  const store = await tempStore();
  await store.upsert("gammel", "i-gammel");
  await store.markPrompted("gammel", "e1");
  const now = Date.now() + 10 * 60_000;

  assert.deepEqual(
    store.idleTopics(5 * 60_000, now).map(([t]) => t),
    ["gammel"],
  );
  assert.deepEqual(store.idleTopics(60 * 60_000, now), [], "ikke inaktiv ennå");
});

test("TTL 0 slår av nedtaking", async () => {
  const store = await tempStore();
  await store.upsert("t", "i-t");
  assert.deepEqual(store.idleTopics(0, Date.now() + 10 ** 9), []);
});

test("samtidige skrivinger fra parallelle topics kolliderer ikke", async () => {
  // Regresjon: felles temp-filnavn ga ENOENT på rename for den andre
  // skriveren, som boblet opp som en falsk «intern feil» for et topic som
  // gikk helt fint. Parallelle topics er et akseptansekriterium (issue #97).
  const store = await tempStore();
  const outcomes = await Promise.allSettled(
    Array.from({ length: 12 }, (_, i) => store.upsert(`topic-${i}`, `instans-${i}`)),
  );
  assert.deepEqual(
    outcomes.filter((o) => o.status === "rejected"),
    [],
  );

  const reopened = new TopicStore(store.stateDir);
  await reopened.load();
  assert.equal(reopened.entries().length, 12, "alle topics skal være persistert");
});

test("samtidige markPrompted etterlater en lesbar state-fil", async () => {
  const store = await tempStore();
  for (let i = 0; i < 6; i++) await store.upsert(`t-${i}`, `i-${i}`);
  await Promise.all(Array.from({ length: 6 }, (_, i) => store.markPrompted(`t-${i}`, `e-${i}`)));

  const reopened = new TopicStore(store.stateDir);
  await reopened.load();
  for (let i = 0; i < 6; i++) {
    assert.equal(reopened.get(`t-${i}`)?.prompts, 1, `t-${i} tapte prompt-tellingen`);
  }
});

test("et topic som er tatt ned tas ikke ned igjen", async () => {
  const store = await tempStore();
  await store.upsert("t", "i-t");
  await store.markDown("t");
  assert.deepEqual(store.idleTopics(1, Date.now() + 10 ** 9), []);
});
