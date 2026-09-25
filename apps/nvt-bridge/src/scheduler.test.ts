import assert from "node:assert/strict";
import { test } from "node:test";
import { TopicScheduler } from "./scheduler.ts";
import type { QueueEvent } from "./types.ts";

function evt(id: string): QueueEvent {
  return { id, source: "agent", type: "delegation", received_at: "t", prompt: "p" };
}

/** Et løfte vi kan resolve utenfra — gjør rekkefølgen deterministisk. */
function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => (resolve = r));
  return { promise, resolve };
}

test("serielt per topic: event 2 starter ikke før event 1 er ferdig", async () => {
  const order: string[] = [];
  const gate = deferred();
  const scheduler = new TopicScheduler({
    maxParallel: 4,
    handler: async (event) => {
      order.push(`start:${event.id}`);
      if (event.id === "e1") await gate.promise;
      order.push(`slutt:${event.id}`);
    },
  });

  scheduler.enqueue(evt("e1"), "topic-a");
  scheduler.enqueue(evt("e2"), "topic-a");
  scheduler.pump();
  await Promise.resolve();

  assert.deepEqual(order, ["start:e1"], "e2 skal vente på e1");
  gate.resolve();
  await scheduler.idle();
  assert.deepEqual(order, ["start:e1", "slutt:e1", "start:e2", "slutt:e2"]);
});

test("parallelt på tvers av topics", async () => {
  const started: string[] = [];
  const gates = new Map([
    ["a", deferred()],
    ["b", deferred()],
  ]);
  const scheduler = new TopicScheduler({
    maxParallel: 4,
    handler: async (_event, topic) => {
      started.push(topic);
      await gates.get(topic)!.promise;
    },
  });

  scheduler.enqueue(evt("e1"), "a");
  scheduler.enqueue(evt("e2"), "b");
  scheduler.pump();
  await Promise.resolve();

  assert.deepEqual(started.sort(), ["a", "b"], "begge topics skal være i gang");
  assert.equal(scheduler.activeTopics(), 2);
  for (const gate of gates.values()) gate.resolve();
  await scheduler.idle();
});

test("maks N topics samtidig; det tredje venter på en ledig plass", async () => {
  const started: string[] = [];
  const gates = new Map(["a", "b", "c"].map((t) => [t, deferred()] as const));
  const scheduler = new TopicScheduler({
    maxParallel: 2,
    handler: async (_event, topic) => {
      started.push(topic);
      await gates.get(topic)!.promise;
    },
  });

  for (const topic of ["a", "b", "c"]) scheduler.enqueue(evt(`e-${topic}`), topic);
  scheduler.pump();
  await Promise.resolve();

  assert.deepEqual(started, ["a", "b"]);
  assert.equal(scheduler.activeTopics(), 2);

  gates.get("a")!.resolve();
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(started, ["a", "b", "c"], "c slipper inn når a er ferdig");

  gates.get("b")!.resolve();
  gates.get("c")!.resolve();
  await scheduler.idle();
  assert.equal(scheduler.activeTopics(), 0);
});

test("samme event-id køes ikke to ganger", () => {
  const scheduler = new TopicScheduler({ maxParallel: 1, handler: async () => {} });
  assert.equal(scheduler.enqueue(evt("e1"), "a"), true);
  assert.equal(scheduler.enqueue(evt("e1"), "a"), false);
  assert.equal(scheduler.queuedCount(), 1);
  assert.equal(scheduler.has("e1"), true);
});

test("en handler som kaster stopper ikke resten av køen", async () => {
  const seen: string[] = [];
  const errors: string[] = [];
  const scheduler = new TopicScheduler({
    maxParallel: 1,
    handler: async (event) => {
      seen.push(event.id);
      if (event.id === "e1") throw new Error("boom");
    },
    onError: (event) => errors.push(event.id),
  });
  scheduler.enqueue(evt("e1"), "a");
  scheduler.enqueue(evt("e2"), "a");
  scheduler.pump();
  await scheduler.idle();
  assert.deepEqual(seen, ["e1", "e2"]);
  assert.deepEqual(errors, ["e1"]);
});

test("isBusy hindrer TTL-nedtaking av et topic med arbeid", async () => {
  const gate = deferred();
  const scheduler = new TopicScheduler({
    maxParallel: 1,
    handler: () => gate.promise,
  });
  scheduler.enqueue(evt("e1"), "a");
  assert.equal(scheduler.isBusy("a"), true, "køet arbeid teller som busy");
  scheduler.pump();
  await Promise.resolve();
  assert.equal(scheduler.isBusy("a"), true, "aktiv arbeider teller som busy");
  assert.equal(scheduler.isBusy("b"), false);
  gate.resolve();
  await scheduler.idle();
  assert.equal(scheduler.isBusy("a"), false);
});

test("id-vakten slippes når eventet er ferdig behandlet", async () => {
  const scheduler = new TopicScheduler({ maxParallel: 1, handler: async () => {} });
  scheduler.enqueue(evt("e1"), "a");
  scheduler.pump();
  await scheduler.idle();
  assert.equal(scheduler.has("e1"), false);
});
