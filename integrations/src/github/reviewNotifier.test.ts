import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import type { PullRequestCandidate } from "./client.ts";
import type { CodeownersRule } from "./codeowners.ts";
import { keyFor, ReviewNotifier, selectForNotification, type ReviewGithubClient, type SlackDm } from "./reviewNotifier.ts";
import type { ReviewNotifyConfig } from "../config.ts";

function candidate(overrides: Partial<PullRequestCandidate> = {}): PullRequestCandidate {
  return {
    owner: "digdir",
    repo: "digdir-ai-agents",
    number: 1,
    title: "Some PR",
    url: "https://github.com/digdir/digdir-ai-agents/pull/1",
    author: "some-author",
    isDraft: false,
    reviewDecision: "REVIEW_REQUIRED",
    labels: [],
    ...overrides,
  };
}

test("selectForNotification: draft-PR-er varsles aldri", () => {
  const c = candidate({ isDraft: true });
  assert.deepEqual(selectForNotification([c], () => false), []);
});

test("selectForNotification: kun reviewDecision REVIEW_REQUIRED varsles", () => {
  const approved = candidate({ reviewDecision: "APPROVED" });
  const none = candidate({ number: 2, reviewDecision: null });
  const required = candidate({ number: 3, reviewDecision: "REVIEW_REQUIRED" });
  const selected = selectForNotification([approved, none, required], () => false);
  assert.deepEqual(selected.map((c) => c.number), [3]);
});

test("selectForNotification: auto-merge-label uten CODEOWNERS-treff undertrykkes", () => {
  const c = candidate({ labels: ["auto-merge"] });
  assert.deepEqual(selectForNotification([c], () => false), []);
});

test("selectForNotification: auto-merge-label MED CODEOWNERS-treff varsles fortsatt", () => {
  const c = candidate({ labels: ["auto-merge"] });
  assert.deepEqual(selectForNotification([c], () => true), [c]);
});

test("selectForNotification: PR uten auto-merge-label varsles uavhengig av CODEOWNERS", () => {
  const c = candidate({ labels: [] });
  assert.deepEqual(selectForNotification([c], () => false), [c]);
});

test("keyFor: dedupe-nøkkelen er owner/repo#nummer", () => {
  assert.equal(keyFor(candidate({ number: 42 })), "digdir/digdir-ai-agents#42");
});

/** Fake Slack client that records DMs and lets a test force the next call to fail. */
class FakeSlack implements SlackDm {
  opened: string[] = [];
  sent: Array<{ channel: string; text: string }> = [];
  failNext = false;

  async openDm(userId: string): Promise<string> {
    this.opened.push(userId);
    return `D-${userId}`;
  }

  async postMessage(channel: string, text: string): Promise<void> {
    if (this.failNext) {
      this.failNext = false;
      throw new Error("slack down");
    }
    this.sent.push({ channel, text });
  }
}

/** Fake GitHub client returning fixed candidates from an in-memory map. */
class FakeGithub implements ReviewGithubClient {
  private readonly byRepo: Map<string, PullRequestCandidate[]>;
  private readonly codeowners: CodeownersRule[] | null;
  private readonly filesByPr: Map<number, string[]>;

  constructor(
    byRepo: Map<string, PullRequestCandidate[]>,
    codeowners: CodeownersRule[] | null = null,
    filesByPr: Map<number, string[]> = new Map(),
  ) {
    this.byRepo = byRepo;
    this.codeowners = codeowners;
    this.filesByPr = filesByPr;
  }

  async listOpenPullRequestCandidates(owner: string, repo: string): Promise<PullRequestCandidate[]> {
    return this.byRepo.get(`${owner}/${repo}`) ?? [];
  }

  async listPullRequestFiles(_owner: string, _repo: string, number: number): Promise<string[]> {
    return this.filesByPr.get(number) ?? [];
  }

  async getCodeowners(): Promise<CodeownersRule[] | null> {
    return this.codeowners;
  }
}

async function tempConfig(overrides: Partial<ReviewNotifyConfig> = {}): Promise<ReviewNotifyConfig> {
  const stateDir = await mkdtemp(path.join(tmpdir(), "review-notify-notifier-"));
  return {
    enabled: true,
    notifyUserId: "U123",
    repos: ["digdir/digdir-ai-agents"],
    pollIntervalSeconds: 60,
    stateDir,
    ...overrides,
  };
}

test("ReviewNotifier: sender én DM for en PR som venter på review", async () => {
  const pr = candidate();
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]));
  const slack = new FakeSlack();
  const notifier = new ReviewNotifier(await tempConfig(), github, slack);

  await notifier["pollOnce"]();

  assert.equal(slack.sent.length, 1);
  assert.match(slack.sent[0].text, /Some PR/);
  assert.match(slack.sent[0].text, /some-author/);
  assert.match(slack.sent[0].text, new RegExp(pr.url.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
});

test("ReviewNotifier: dedupe — samme PR varsles ikke to ganger på tvers av poll-sykluser", async () => {
  const pr = candidate();
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]));
  const slack = new FakeSlack();
  const config = await tempConfig();
  const notifier = new ReviewNotifier(config, github, slack);

  await notifier["pollOnce"]();
  await notifier["pollOnce"]();

  assert.equal(slack.sent.length, 1, "andre poll-syklus skal ikke sende en ny DM for samme PR");
});

test("ReviewNotifier: dedupe overlever restart via state-fil på disk", async () => {
  const pr = candidate();
  const config = await tempConfig();

  const first = new ReviewNotifier(config, new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]])), new FakeSlack());
  await first["pollOnce"]();

  const slack2 = new FakeSlack();
  const second = new ReviewNotifier(config, new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]])), slack2);
  await second["pollOnce"]();

  assert.equal(slack2.sent.length, 0, "en ny ReviewNotifier-instans skal lese den persisterte dedupe-tilstanden");
});

test("ReviewNotifier: feilet Slack-DM logges/svelges, ikke markert varslet — prøves igjen neste syklus", async () => {
  const pr = candidate();
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]));
  const slack = new FakeSlack();
  slack.failNext = true;
  const notifier = new ReviewNotifier(await tempConfig(), github, slack);

  await notifier["pollOnce"](); // feiler stille
  assert.equal(slack.sent.length, 0);

  await notifier["pollOnce"](); // prøver på nytt og lykkes
  assert.equal(slack.sent.length, 1);
});

test("ReviewNotifier: DM lykkes men markNotified feiler — ingen duplikat ved neste syklus eller restart", async () => {
  const pr = candidate();
  const config = await tempConfig();
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]));
  const slack = new FakeSlack();
  const notifier = new ReviewNotifier(config, github, slack);

  // Simulerer disk-feil i det bekreftende skrivet, etter at DM-en er sendt.
  const store = notifier["store"];
  store.markNotified = async () => {
    throw new Error("disk full");
  };

  await notifier["pollOnce"]();
  assert.equal(slack.sent.length, 1, "DM-en skal faktisk ha blitt sendt");

  await notifier["pollOnce"]();
  assert.equal(slack.sent.length, 1, "samme syklus-instans skal ikke sende på nytt");

  const slack2 = new FakeSlack();
  const afterRestart = new ReviewNotifier(
    config,
    new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]])),
    slack2,
  );
  await afterRestart["pollOnce"]();
  assert.equal(slack2.sent.length, 0, "pending-markøren på disk skal hindre duplikat etter restart");
});

test("ReviewNotifier: feiler markPending (før sending) sendes ingen DM — fail closed", async () => {
  const pr = candidate();
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]));
  const slack = new FakeSlack();
  const notifier = new ReviewNotifier(await tempConfig(), github, slack);

  const store = notifier["store"];
  const real = store.markPending.bind(store);
  store.markPending = async () => {
    throw new Error("disk read-only");
  };

  await notifier["pollOnce"]();
  assert.equal(slack.sent.length, 0, "uten varig markør skal ingen DM sendes");

  // Når disken er tilbake, varsles PR-en som normalt (nøyaktig én gang).
  store.markPending = real;
  await notifier["pollOnce"]();
  assert.equal(slack.sent.length, 1);
});

test("ReviewNotifier: korrupt state-fil utsetter varsling i stedet for å re-sende", async () => {
  const pr = candidate();
  const config = await tempConfig();

  const first = new ReviewNotifier(
    config,
    new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]])),
    new FakeSlack(),
  );
  await first["pollOnce"]();

  // State-fila blir ulesbar (halvskrevet fil, disk-korrupsjon, feil format).
  await writeFile(path.join(config.stateDir, "review-notified.json"), "{ikke json", "utf-8");

  const slack2 = new FakeSlack();
  const second = new ReviewNotifier(
    config,
    new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]])),
    slack2,
  );
  await second["pollOnce"]();

  assert.equal(slack2.sent.length, 0, "varsling skal utsettes, ikke gjenta seg fra tom state");
});

test("ReviewNotifier: auto-merge-PR uten CODEOWNERS-treff varsles ikke", async () => {
  const pr = candidate({ labels: ["auto-merge"] });
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr]]]), null);
  const slack = new FakeSlack();
  const notifier = new ReviewNotifier(await tempConfig(), github, slack);

  await notifier["pollOnce"]();

  assert.equal(slack.sent.length, 0);
});

test("ReviewNotifier: to ulike PR-er i samme syklus gir to DM-er, ikke duplikater", async () => {
  const pr1 = candidate({ number: 1 });
  const pr2 = candidate({ number: 2, title: "Another PR" });
  const github = new FakeGithub(new Map([["digdir/digdir-ai-agents", [pr1, pr2]]]));
  const slack = new FakeSlack();
  const notifier = new ReviewNotifier(await tempConfig(), github, slack);

  await notifier["pollOnce"]();

  assert.equal(slack.sent.length, 2);
  assert.equal(slack.opened.length, 1, "samme DM-kanal skal gjenbrukes, ikke åpnes på nytt per PR");
});
