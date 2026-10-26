// Answers Claude Code permission requests as Claude Code would with the settings on standard input:
// `{ "session": <Session launch settings>, "managed": <managed settings> }`. Matchers use Claude's
// JavaScript regular expressions, and the matching hook commands run in a shell.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const { session, managed } = JSON.parse(readFileSync(0, "utf8"));
const matching = (settings, tool) =>
  settings.hooks.PermissionRequest.filter((entry) => new RegExp(entry.matcher).test(tool)).flatMap(
    (entry) => entry.hooks,
  );
const decide = (hook, permission_mode, agent_id) => {
  const result = spawnSync("sh", ["-c", hook.command], {
    input: JSON.stringify({
      hook_event_name: "PermissionRequest",
      permission_mode,
      agent_id,
      tool_name: "Bash",
      tool_input: { command: "exit 1" },
    }),
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout === "" ? undefined : JSON.parse(result.stdout);
};

for (const settings of [session, managed]) {
  const [approval, ...others] = matching(settings, "Bash");
  assert.deepEqual(others, []);
  // No platform identity is needed, including for requests from background subagents.
  for (const agent_id of [undefined, "background-agent"]) {
    assert.deepEqual(decide(approval, "bypassPermissions", agent_id), {
      hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } },
    });
  }
  // A person who chose another mode, such as one of T3 Code's approval modes, is asked.
  for (const mode of ["default", "acceptEdits", "plan", "dontAsk"]) {
    assert.equal(decide(approval, mode), undefined, mode);
  }
}
// Sessions report other permission prompts to agentd. Other processes have no session to report to.
for (const tool of ["Write", "AskUserQuestion", "ExitPlanMode", "mcp__test__Bash"]) {
  assert.deepEqual(matching(session, tool), session.hooks.UserPromptSubmit[0].hooks, tool);
  assert.deepEqual(matching(managed, tool), [], tool);
}
