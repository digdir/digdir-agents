//! Claude Code's hook events and what each one tells the platform.

use crate::{harness::hook_script::HookScript, sessions::ActivityEvent};

/// Every Claude Code hook event the platform folds. `SessionStart` also carries
/// the native session ID and transcript location; the rest are activity signals
/// that keep a working Session from looking idle and let an orchestrator wait.
const EVENTS: &[(&str, ActivityEvent)] = &[
    ("SessionStart", ActivityEvent::SessionStart),
    ("UserPromptSubmit", ActivityEvent::TurnStarted),
    ("Stop", ActivityEvent::TurnCompleted),
    ("StopFailure", ActivityEvent::TurnCompleted),
    ("PermissionRequest", ActivityEvent::WaitingForInput),
    ("Notification", ActivityEvent::WaitingForInput),
];

/// `Notification` types that mean Claude Code is blocked on the operator;
/// authentication and dialog notifications carry no activity signal.
const WAITING_NOTIFICATIONS: &[&str] = &["permission_prompt", "idle_prompt"];

/// `SessionStart` sources that begin a conversation the platform tracks.
const SESSION_START_MATCHER: &str = "startup|resume|clear|compact";

// Claude's deletion checks can still ask in bypass mode. The Agent's Sandbox is the
// isolation boundary, so answer Bash permission requests without waiting for the operator.
const BASH_APPROVAL_COMMAND: &str =
    r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'"#;

const SCRIPT: HookScript<'static> = HookScript {
    events: EVENTS,
    waiting_notifications: WAITING_NOTIFICATIONS,
};

/// Renders Claude Code's activity hook script.
///
/// # Errors
///
/// Returns an error when the event table cannot be encoded.
pub(super) fn script() -> Result<String, serde_json::Error> {
    SCRIPT.render()
}

/// The `PermissionRequest` entry that answers Bash requests without the operator. It needs no
/// Session, so Claude Code processes the platform does not launch get it too.
pub(super) fn bash_approval() -> serde_json::Value {
    serde_json::json!({
        "matcher": "^Bash$",
        "hooks": [{ "type": "command", "command": BASH_APPROVAL_COMMAND }]
    })
}

/// The `hooks` value of Claude Code's settings. Automatically approve Bash requests;
/// notify agentd about other permission prompts and Session activity.
pub(super) fn configuration(hook_path: &str) -> serde_json::Value {
    let command = serde_json::json!({ "type": "command", "command": format!("node {hook_path}") });
    let hooks = SCRIPT
        .event_names()
        .map(|event| {
            if event == "PermissionRequest" {
                return (
                    event.to_owned(),
                    serde_json::json!([
                        bash_approval(),
                        // Reporting an auto-approved call as WaitingForInput would end a
                        // prompt --wait before Claude has finished the turn.
                        { "matcher": "^(?!Bash$)", "hooks": [command] }
                    ]),
                );
            }
            let mut entry = serde_json::json!({ "hooks": [command] });
            if event == "SessionStart" {
                entry["matcher"] = SESSION_START_MATCHER.into();
            }
            (event.to_owned(), serde_json::Value::Array(vec![entry]))
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(hooks)
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn bash_permission_requests_are_approved_without_reporting_waiting() {
        use std::{io::Write as _, process::Stdio};

        // Use Claude's JavaScript regexp matching and execute the generated shell command.
        // No platform identity is needed, including for requests from background subagents.
        let mut child = std::process::Command::new("node")
            .args([
                "-e",
                r"
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { spawnSync } = require('node:child_process');
const hooks = JSON.parse(readFileSync(0, 'utf8'));
const matching = (name) => hooks.PermissionRequest
  .filter((entry) => new RegExp(entry.matcher).test(name))
  .flatMap((entry) => entry.hooks);
assert.equal(matching('Bash').length, 1);
for (const agent_id of [undefined, 'background-agent']) {
  const result = spawnSync('sh', ['-c', matching('Bash')[0].command], {
    input: JSON.stringify({
      hook_event_name: 'PermissionRequest', tool_name: 'Bash', agent_id,
      tool_input: { command: 'exit 1' }
    }), encoding: 'utf8'
  });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    hookSpecificOutput: {
      hookEventName: 'PermissionRequest', decision: { behavior: 'allow' }
    }
  });
}
for (const tool of ['Write', 'AskUserQuestion', 'ExitPlanMode', 'mcp__test__Bash']) {
  assert.deepEqual(matching(tool), hooks.UserPromptSubmit[0].hooks);
}
",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .expect("Node.js runs Claude hook matchers");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(super::configuration("/unused/activity-hook.mjs").to_string().as_bytes())
            .expect("write hook configuration");
        assert!(child.wait().expect("hook test exits").success());
    }
}
