//! Codex's hook events and what each one tells the platform.

use crate::{harness::hook_script::HookScript, sessions::ActivityEvent};

/// Every Codex hook event the platform folds. Codex 0.153 has no
/// `Notification`; Stop and Interrupt report a turn ending, not task success.
const EVENTS: &[(&str, ActivityEvent)] = &[
    ("SessionStart", ActivityEvent::SessionStart),
    ("UserPromptSubmit", ActivityEvent::TurnStarted),
    ("Stop", ActivityEvent::TurnCompleted),
    ("Interrupt", ActivityEvent::TurnCompleted),
    ("PermissionRequest", ActivityEvent::WaitingForInput),
];

/// Seconds Codex waits for the hook before continuing; a slow report must not
/// stall a turn.
const HOOK_TIMEOUT_SECONDS: u32 = 3;

const SCRIPT: HookScript<'static> = HookScript {
    events: EVENTS,
    waiting_notifications: &[],
};

/// Renders Codex's activity hook script.
///
/// # Errors
///
/// Returns an error when the event table cannot be encoded.
pub(super) fn script() -> Result<String, serde_json::Error> {
    SCRIPT.render()
}

/// The contents of Codex's `hooks.json`, registering the script for exactly
/// the events in the table.
pub(super) fn configuration(hook_path: &str) -> serde_json::Value {
    let command = serde_json::json!({
        "type": "command",
        "command": format!("node {hook_path}"),
        "timeout": HOOK_TIMEOUT_SECONDS,
    });
    let hooks = SCRIPT
        .event_names()
        .map(|event| {
            let entry = serde_json::json!({ "hooks": [command] });
            (event.to_owned(), serde_json::Value::Array(vec![entry]))
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({ "hooks": hooks })
}
