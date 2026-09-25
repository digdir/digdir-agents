import assert from "node:assert/strict";
import { test } from "node:test";
import {
  identityMode,
  identityProblem,
  renderAgentConfig,
  type AgentConfigVars,
} from "./agentConfig.ts";

const vars: AgentConfigVars = {
  agentType: "claude",
  runtimeArgs: ["--dangerously-skip-permissions"],
  userMode: "non-root",
  gitProvider: "fatdev-broker",
  brokerProvider: "fatdev-pat",
  targetMatch: "github.com/digdir/*",
  urlMatch: "https://github.com/digdir/",
  identityName: "Digdir Automation",
  identityEmail: "bot@example.invalid",
};

test("malen setter identity.mode: explicit, aldri provider (M0-funn 4)", () => {
  const yaml = renderAgentConfig(vars);
  assert.equal(identityMode(yaml).kind, "explicit");
  // Ingen aktiv mode-linje med provider (kommentaren som forklarer hvorfor
  // ikke, skal derimot stå der).
  assert.doesNotMatch(yaml, /^\s*mode: provider/m);
  assert.match(yaml, /name: "Digdir Automation"/);
  assert.match(yaml, /email: "bot@example\.invalid"/);
});

test("malen setter non-root og bypass-flagget (M0-funn 1)", () => {
  const yaml = renderAgentConfig(vars);
  assert.match(yaml, /user: "non-root"/);
  assert.match(yaml, /- "--dangerously-skip-permissions"/);
});

test("tom argumentliste blir en tom YAML-liste, ikke en tom node", () => {
  const yaml = renderAgentConfig({ ...vars, runtimeArgs: [] });
  assert.match(yaml, /^ {2}args: \[\]$/m);
});

test("broker-provideren og match-reglene kommer fra config", () => {
  const yaml = renderAgentConfig({ ...vars, brokerProvider: "annen-pat" });
  assert.match(yaml, /type: broker/);
  assert.match(yaml, /broker-provider: "annen-pat"/);
  assert.match(yaml, /- "github\.com\/digdir\/\*"/);
});

test("verdier fra env kan ikke bryte ut av sin egen YAML-node", () => {
  const yaml = renderAgentConfig({
    ...vars,
    identityName: 'Bot" \nplugins: []\n#',
  });
  // Alt havner i én double-quoted skalar; ingen ny toppnøkkel oppstår.
  assert.equal(yaml.match(/^plugins:/gm)?.length, 1);
  assert.match(yaml, /name: "Bot\\" \\nplugins: \[\]\\n#"/);
});

test("manglende identitet gir en ærlig feil, ikke en config uten identitet", () => {
  assert.throws(() => renderAgentConfig({ ...vars, identityName: " " }), /identityName/);
  assert.throws(() => renderAgentConfig({ ...vars, brokerProvider: "" }), /brokerProvider/);
});

test("identityMode ser bare mode: under identity:, ikke andre mode-felt", () => {
  const yaml = [
    "code-server:",
    "  settings:",
    "    overwrite: false",
    "preseed:",
    "  files:",
    '    - path: "$HOME/.claude/settings.json"',
    '      mode: "0600"',
  ].join("\n");
  assert.deepEqual(identityMode(yaml), { kind: "absent" });
});

test("identityMode finner provider-modus i en nøstet plugin-config", () => {
  const yaml = [
    "plugins:",
    "  - name: git-credentials",
    "    config:",
    "      credentials:",
    "        - match: https://github.com/digdir/",
    "          identity:",
    "            mode: provider",
  ].join("\n");
  assert.deepEqual(identityMode(yaml), { kind: "provider", line: 7 });
});

test("identityMode bryr seg ikke om kommentarer", () => {
  const yaml = ["identity:", "  # mode: provider  <- gammelt eksempel", "  mode: explicit"].join("\n");
  assert.equal(identityMode(yaml).kind, "explicit");
});

test("flow-style identity fanges: mode: provider på én linje er fortsatt feil", () => {
  const yaml = "        - identity: {mode: provider, name: x}\n";
  assert.deepEqual(identityMode(yaml), { kind: "provider", line: 1 });
  assert.equal(identityProblem(yaml, "/x/agent.yaml")?.level, "error");
});

test("flow-style med explicit godtas, uleselig flow-style gir advarsel", () => {
  assert.equal(identityMode('identity: {mode: "explicit", name: "B"}').kind, "explicit");

  const odd = identityMode("identity: &anchor\nsomething: else");
  assert.deepEqual(odd, { kind: "unreadable", line: 1 });
  const problem = identityProblem("identity: &anchor\nsomething: else", "/x/agent.yaml");
  assert.equal(problem?.level, "warn");
  assert.match(problem!.message, /ikke kan verifisere/);
});

test("identityProblem: provider er feil, manglende identitet er en advarsel", () => {
  const provider = identityProblem("identity:\n  mode: provider\n", "/x/agent.yaml");
  assert.equal(provider?.level, "error");
  assert.match(provider!.message, /M0-funn 4/);
  assert.match(provider!.message, /\/x\/agent\.yaml/);

  const absent = identityProblem("runtime:\n  command: claude\n", "/x/agent.yaml");
  assert.equal(absent?.level, "warn");

  assert.equal(identityProblem(renderAgentConfig(vars), "/x/agent.yaml"), undefined);
});
