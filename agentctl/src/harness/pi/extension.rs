//! The Pi extension that reports activity to the platform.
//!
//! Pi has no hook commands; it loads TypeScript extensions in process. The extension subscribes
//! to the lifecycle events that carry the same signals the other harnesses' hooks do, and reports
//! them with the code shared with the hook scripts.

use crate::harness::hook_script;

const TEMPLATE: &str = include_str!("activity_extension.ts");

/// Renders the activity extension, which the launch loads with `pi -e`.
///
/// # Errors
///
/// Returns an error when the endpoint locations cannot be encoded.
pub(super) fn script() -> Result<String, serde_json::Error> {
    Ok(TEMPLATE.replace("__REPORTER__", &hook_script::reporter()?))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::harness::hook_script::test_support::{pairs, summary};

    #[test]
    fn the_rendered_extension_has_no_placeholders_left() {
        let script = super::script().expect("script");
        assert!(!script.contains("__"), "{script}");
        assert!(script.contains("/etc/agent-platform-api-url"));
        assert!(script.contains("/v1/session/hooks"));
    }

    /// Drives the rendered extension under Node with a fake `pi` and returns the reports that
    /// reached a stand-in Platform API, in order.
    fn run(driver: &str) -> Vec<Value> {
        let driver = format!(
            r#"import extension from "./activity.ts";
const handlers = {{}};
extension({{ on: (name, handler) => {{ handlers[name] = handler; }} }});
const ctx = {{ sessionManager: {{ getSessionId: () => "native-1", getSessionFile: () => "/sessions/native-1.jsonl" }} }};
{driver}
"#
        );
        let script = super::script().expect("script");
        crate::harness::hook_script::test_support::run("activity.ts", &script, &driver, &[])
    }

    #[test]
    fn session_start_reports_the_native_conversation_and_its_transcript() {
        let reports = run(r#"await handlers.session_start({ reason: "resume" }, ctx);"#);

        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(reports[0]["event"], "sessionStart");
        assert_eq!(reports[0]["source"], "resume");
        assert_eq!(reports[0]["nativeSessionId"], "native-1");
        assert_eq!(reports[0]["transcriptPath"], "/sessions/native-1.jsonl");
        assert_eq!(reports[0]["sessionId"], "session-1");
    }

    #[test]
    fn only_operator_runs_report_turns_and_an_extension_run_does_not_advance_them() {
        let reports = run(r#"
await handlers.before_agent_start({ messageType: "custom" });
await handlers.agent_settled({ aborted: false });
await handlers.before_agent_start({ messageType: "user" });
await handlers.agent_settled({ aborted: false });
await handlers.agent_settled({ aborted: false });
"#);

        assert_eq!(
            summary(&reports),
            pairs(&[("turnStarted", "user"), ("turnCompleted", "settled")])
        );
    }

    #[test]
    fn escape_reports_an_aborted_completion() {
        let reports = run(r#"
await handlers.before_agent_start({ messageType: "user" });
await handlers.agent_settled({ aborted: true });
"#);

        assert_eq!(
            summary(&reports),
            pairs(&[("turnStarted", "user"), ("turnCompleted", "aborted")])
        );
    }

    #[test]
    fn a_dialog_blocks_the_turn_and_resumes_it_only_inside_a_run() {
        let reports = run(r#"
await handlers.ui_prompt_start({});
await handlers.ui_prompt_end({});
await handlers.before_agent_start({ messageType: "user" });
await handlers.ui_prompt_start({});
await handlers.ui_prompt_end({});
"#);

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("waitingForInput", "ui_prompt"),
                ("turnStarted", "user"),
                ("waitingForInput", "ui_prompt"),
                ("turnStarted", "ui_prompt"),
            ])
        );
        assert!(
            reports
                .iter()
                .all(|report| report["eventId"].is_string() && report["sessionId"] == json!("session-1"))
        );
    }
}
