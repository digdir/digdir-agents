import assert from "node:assert/strict";
import { test } from "node:test";
import { instanceNameFor, safeId, topicFor } from "./topic.ts";
import type { QueueEvent } from "./types.ts";

function evt(id: string, originEventId?: string): QueueEvent {
  return {
    id,
    source: "agent",
    type: "delegation",
    received_at: "2026-07-29T08:10:33.246Z",
    prompt: "gjør noe",
    payload:
      originEventId === undefined
        ? {}
        : { origin: { agent: "proxy-agent", event_id: originEventId, hops: 1 } },
  };
}

test("topic er payload.origin.event_id når den finnes", () => {
  const e = evt("github-digdir-repo-97-c123-d1", "github-digdir-repo-97-c123");
  assert.equal(topicFor(e), "github-digdir-repo-97-c123");
});

test("topic faller tilbake på eventets egen id, uten delta-suffiks", () => {
  assert.equal(topicFor(evt("github-digdir-repo-97-c123-d1")), "github-digdir-repo-97-c123");
  assert.equal(topicFor(evt("github-digdir-repo-97-c123-d2")), "github-digdir-repo-97-c123");
  assert.equal(topicFor(evt("slack-C123-1719-99")), "slack-C123-1719-99");
});

test("oppfølgingsevents i samme tråd gir samme topic (samme levende sesjon)", () => {
  const first = evt("github-repo-97-c1-d1", "github-repo-97-c1");
  const followUp = evt("github-repo-97-c1-d2", "github-repo-97-c1");
  assert.equal(topicFor(first), topicFor(followUp));
});

test("kun ett ankret -dN strippes — bug-kompatibelt med agent-runner.ps1", () => {
  // PowerShell-regexen er ankret, så den kan bare matche én gang.
  assert.equal(topicFor(evt("x-d1-d2")), "x-d1");
  // -dN midt i strengen er ikke et suffiks.
  assert.equal(topicFor(evt("x-d1-mer")), "x-d1-mer");
});

test("-outcome-suffikset (debrief) strippes ikke", () => {
  assert.equal(topicFor(evt("github-repo-97-c1-outcome")), "github-repo-97-c1-outcome");
});

test("topic saneres til samme tegnsett som integrations bruker", () => {
  assert.equal(topicFor(evt("slack-C1/2:3 4")), "slack-C1_2_3_4");
  assert.equal(safeId("a b/c"), "a_b_c");
});

test("tom origin.event_id faller tilbake på eventets id", () => {
  const e = evt("slack-C1-9-d1");
  e.payload = { origin: { event_id: "" } };
  assert.equal(topicFor(e), "slack-C1-9");
});

test("event uten payload håndteres", () => {
  const e: QueueEvent = {
    id: "slack-C1-9",
    source: "slack",
    type: "message",
    received_at: "2026-07-29T08:10:33.246Z",
    prompt: "hei",
  };
  assert.equal(topicFor(e), "slack-C1-9");
});

test("instansnavn holdes innenfor maks-lengden", () => {
  const long = "github-digdir-digdir-ai-agents-97-c5114846564";
  const name = instanceNameFor(long, { prefix: "fatdev", maxLength: 40 });
  assert.ok(name.length <= 40, `for langt: ${name} (${name.length})`);
  assert.match(name, /^fatdev-[a-z0-9-]+-[0-9a-f]{8}$/);
});

test("instansnavn er deterministisk — tapt state gjenfinner samme workspace", () => {
  const opts = { prefix: "fatdev", maxLength: 40 };
  const topic = "github-digdir-digdir-ai-agents-97-c123";
  assert.equal(instanceNameFor(topic, opts), instanceNameFor(topic, opts));
});

test("topics som kortes til samme slug får ulike instansnavn", () => {
  const opts = { prefix: "fatdev", maxLength: 32 };
  const a = instanceNameFor("github-digdir-digdir-ai-agents-97-c111", opts);
  const b = instanceNameFor("github-digdir-digdir-ai-agents-97-c222", opts);
  assert.notEqual(a, b);
  assert.ok(a.length <= 32 && b.length <= 32);
});

test("instansnavn er en gyldig DNS-etikett (code-server rutes på det)", () => {
  for (const topic of ["slack-C123-1719.55", "GitHub_Repo/97", "æøå-topic"]) {
    const name = instanceNameFor(topic, { prefix: "fatdev", maxLength: 40 });
    assert.match(name, /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/, `ugyldig etikett: ${name}`);
    assert.ok(name.length <= 63);
  }
});
