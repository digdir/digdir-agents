import assert from "node:assert/strict";
import { test } from "node:test";
import type { GithubConfig } from "../config.ts";
import { GithubClient } from "./client.ts";
import { selectForNotification } from "./reviewNotifier.ts";

function testConfig(): GithubConfig {
  return {
    enabled: true,
    token: "test-token",
    notificationsToken: "test-notifications-token",
    reaction: "eyes",
    workingReaction: "hourglass",
    ackReaction: "white_check_mark",
    pollIntervalSeconds: 60,
    apiBaseUrl: "https://api.github.com",
    allowedUsers: [],
  };
}

/** A GraphQL request captured by the fetch stub. */
interface Captured {
  query: string;
  variables: Record<string, unknown>;
}

/**
 * Replaces global fetch with a stub that answers the queued GraphQL payloads in
 * order, and records every request. Returns the recorded list plus a restore fn.
 */
function stubGraphql(payloads: unknown[]): { captured: Captured[]; restore: () => void } {
  const original = globalThis.fetch;
  const captured: Captured[] = [];
  let call = 0;

  globalThis.fetch = (async (_url: string | URL | Request, init?: RequestInit) => {
    captured.push(JSON.parse(String(init?.body)) as Captured);
    const data = payloads[call++];
    assert.ok(data !== undefined, `uventet ekstra GraphQL-kall nr. ${call}`);
    return new Response(JSON.stringify({ data }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;

  return { captured, restore: () => { globalThis.fetch = original; } };
}

function prNode(labelPage: unknown) {
  return {
    number: 7,
    title: "Some PR",
    url: "https://github.com/digdir/digdir-ai-agents/pull/7",
    isDraft: false,
    reviewDecision: "REVIEW_REQUIRED",
    author: { login: "some-author" },
    labels: labelPage,
  };
}

test("listOpenPullRequestCandidates: labels hentes komplett når de spenner flere sider", async () => {
  const { captured, restore } = stubGraphql([
    {
      repository: {
        pullRequests: {
          nodes: [
            prNode({
              nodes: [{ name: "kind/chore" }, { name: "area/integrations" }],
              pageInfo: { hasNextPage: true, endCursor: "LABELCURSOR1" },
            }),
          ],
          pageInfo: { hasNextPage: false, endCursor: null },
        },
      },
    },
    {
      repository: {
        pullRequest: {
          labels: {
            nodes: [{ name: "auto-merge" }],
            pageInfo: { hasNextPage: false, endCursor: null },
          },
        },
      },
    },
  ]);

  try {
    const candidates = await new GithubClient(testConfig()).listOpenPullRequestCandidates(
      "digdir",
      "digdir-ai-agents",
    );

    assert.equal(candidates.length, 1);
    assert.deepEqual(candidates[0].labels, ["kind/chore", "area/integrations", "auto-merge"]);
    assert.equal(captured.length, 2, "andre side med labels skal hentes");
    assert.deepEqual(captured[1].variables, {
      owner: "digdir",
      repo: "digdir-ai-agents",
      number: 7,
      cursor: "LABELCURSOR1",
    });
  } finally {
    restore();
  }
});

test("auto-merge-label på side 2 undertrykker varsling som om den lå på side 1", async () => {
  const { restore } = stubGraphql([
    {
      repository: {
        pullRequests: {
          nodes: [
            prNode({
              nodes: [{ name: "kind/chore" }],
              pageInfo: { hasNextPage: true, endCursor: "LABELCURSOR1" },
            }),
          ],
          pageInfo: { hasNextPage: false, endCursor: null },
        },
      },
    },
    {
      repository: {
        pullRequest: {
          labels: {
            nodes: [{ name: "auto-merge" }],
            pageInfo: { hasNextPage: false, endCursor: null },
          },
        },
      },
    },
  ]);

  try {
    const candidates = await new GithubClient(testConfig()).listOpenPullRequestCandidates(
      "digdir",
      "digdir-ai-agents",
    );
    assert.deepEqual(selectForNotification(candidates, () => false), []);
  } finally {
    restore();
  }
});

test("listOpenPullRequestCandidates: ingen ekstra kall når labels får plass på første side", async () => {
  const { captured, restore } = stubGraphql([
    {
      repository: {
        pullRequests: {
          nodes: [
            prNode({
              nodes: [{ name: "kind/chore" }],
              pageInfo: { hasNextPage: false, endCursor: null },
            }),
          ],
          pageInfo: { hasNextPage: false, endCursor: null },
        },
      },
    },
  ]);

  try {
    const candidates = await new GithubClient(testConfig()).listOpenPullRequestCandidates(
      "digdir",
      "digdir-ai-agents",
    );
    assert.deepEqual(candidates[0].labels, ["kind/chore"]);
    assert.equal(captured.length, 1);
  } finally {
    restore();
  }
});
