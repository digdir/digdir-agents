//! Claude Code's hook events and what each one tells the platform.

use crate::{harness::hook_script::HookScript, sessions::ActivityEvent};

/// Every Claude Code hook event the platform folds. `SessionStart` also carries
/// the native session ID and transcript location; the rest are activity signals
/// that keep a working session from looking idle and let an orchestrator wait.
const EVENTS: &[(&str, ActivityEvent)] = &[
    ("SessionStart", ActivityEvent::SessionStart),
    ("UserPromptSubmit", ActivityEvent::TurnStarted),
    ("Stop", ActivityEvent::TurnCompleted),
    ("StopFailure", ActivityEvent::TurnCompleted),
    ("PermissionRequest", ActivityEvent::WaitingForInput),
    ("Notification", ActivityEvent::WaitingForInput),
    // Fires once every tool call of a model step has resolved, so after the operator answers a
    // question or approves a call, and not while another call in the same step still waits.
    ("PostToolBatch", ActivityEvent::TurnContinued),
];

/// `Notification` types that mean Claude Code is blocked on the operator;
/// authentication and dialog notifications carry no activity signal.
const WAITING_NOTIFICATIONS: &[&str] = &["permission_prompt", "idle_prompt"];

/// `SessionStart` sources that begin a conversation the platform tracks.
const SESSION_START_MATCHER: &str = "startup|resume|clear|compact";

// Claude's deletion checks can still ask in bypass mode. The agent's sandbox is the isolation
// boundary, so answer those Bash permission requests without waiting for the operator. In any other
// mode a person chose to be asked, so leave the request to them.
const BASH_APPROVAL_COMMAND: &str = r#"node -e 'if (JSON.parse(require("node:fs").readFileSync(0, "utf8")).permission_mode === "bypassPermissions") console.log(JSON.stringify({ hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } } }))'"#;

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

/// The `PermissionRequest` entry that answers Bash requests in bypass mode without the operator.
/// It needs no session, so Claude Code processes the platform does not launch get it too.
pub(super) fn bash_approval() -> serde_json::Value {
    serde_json::json!({
        "matcher": "^Bash$",
        "hooks": [{ "type": "command", "command": BASH_APPROVAL_COMMAND }]
    })
}

/// The `hooks` value of Claude Code's settings. Automatically approve Bash requests in bypass mode;
/// notify agentd about other permission prompts and session activity.
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

#[cfg(test)]
mod tests {
    use crate::harness::hook_script::tests::report;

    /// An answered question resumes the turn, so the session must leave `WaitingForInput` before
    /// Claude Code ends the turn.
    #[tokio::test]
    async fn an_answered_question_reports_the_turn_going_on() {
        let script = super::script().expect("script");
        let asked = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "AskUserQuestion",
        });
        assert_eq!(report(&script, &asked).await["event"], "waitingForInput");
        let answered = serde_json::json!({
            "hook_event_name": "PostToolBatch",
            "tool_calls": [{ "tool_name": "AskUserQuestion" }],
        });
        assert_eq!(report(&script, &answered).await["event"], "turnContinued");
    }
}
