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

/// The `hooks` value of Claude Code's settings, registering the script for
/// exactly the events in the table.
pub(super) fn configuration(hook_path: &str) -> serde_json::Value {
    let command = serde_json::json!({ "type": "command", "command": format!("node {hook_path}") });
    let hooks = SCRIPT
        .event_names()
        .map(|event| {
            let mut entry = serde_json::json!({ "hooks": [command] });
            if event == "SessionStart" {
                entry["matcher"] = SESSION_START_MATCHER.into();
            }
            (event.to_owned(), serde_json::Value::Array(vec![entry]))
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(hooks)
}
