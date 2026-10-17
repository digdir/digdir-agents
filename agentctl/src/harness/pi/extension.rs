//! The Pi extension that reports activity to the platform.
//!
//! Pi has no hook commands; it loads TypeScript extensions in process. The extension subscribes
//! to the lifecycle events that carry the same signals the other harnesses' hooks do, and reports
//! them with the code shared with the hook scripts.

use crate::harness::hook_script;

const TEMPLATE: &str = include_str!("activity_extension.ts");

/// Renders the activity extension, which the launch loads with `pi -e`. It adds the file at
/// `instructions_file`, when there is one, to Pi's system prompt.
///
/// # Errors
///
/// Returns an error when the endpoint locations cannot be encoded.
pub(super) fn script(instructions_file: &str) -> Result<String, serde_json::Error> {
    Ok(TEMPLATE
        .replace("__REPORTER__", &hook_script::reporter()?)
        .replace("__INSTRUCTIONS_FILE__", &serde_json::to_string(instructions_file)?))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        process::Command,
    };

    use serde_json::{Value, json};

    #[test]
    fn the_rendered_extension_has_no_placeholders_left() {
        let script = super::script("/x/instructions.md").expect("script");
        assert!(!script.contains("__"), "{script}");
        assert!(script.contains("/etc/agent-platform-api-url"));
        assert!(script.contains("/v1/session/hooks"));
    }

    /// Reads one request and answers it, returning its body.
    fn serve_one(stream: &mut std::net::TcpStream) -> Option<String> {
        let mut received = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).ok()?;
            if read == 0 {
                return None;
            }
            received.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&received).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>())
                })
                .and_then(Result::ok)
                .unwrap_or(0);
            if body.len() < length {
                continue;
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .ok()?;
            return Some(body.to_owned());
        }
    }

    /// Drives the rendered extension under Node with a fake `pi` and returns the reports that
    /// reached a stand-in Platform API, in order.
    fn run(driver: &str) -> Vec<Value> {
        run_with_instructions(driver, None)
    }

    /// As [`run`], with the platform's instructions file holding `instructions`, if any.
    fn run_with_instructions(driver: &str, instructions: Option<&str>) -> Vec<Value> {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let instructions_file = directory.path().join("instructions.md");
        if let Some(instructions) = instructions {
            std::fs::write(&instructions_file, instructions).expect("instructions");
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let port = listener.local_addr().expect("address").port();
        let url_file = directory.path().join("platform-api-url");
        std::fs::write(&url_file, format!("http://127.0.0.1:{port}\n")).expect("URL file");
        // The guest path is not readable here; point the script at a file the test owns.
        let script = super::script(&instructions_file.to_string_lossy())
            .expect("script")
            .replacen(
                &serde_json::to_string(crate::sandbox::platform::PLATFORM_API_URL_FILE).expect("path"),
                &serde_json::to_string(&url_file).expect("path"),
                1,
            );
        std::fs::write(directory.path().join("activity.ts"), script).expect("extension");
        let driver_path = directory.path().join("driver.mjs");
        std::fs::write(
            &driver_path,
            format!(
                r#"import extension from "./activity.ts";
const handlers = {{}};
extension({{ on: (name, handler) => {{ handlers[name] = handler; }} }});
const ctx = {{ sessionManager: {{ getSessionId: () => "native-1", getSessionFile: () => "/sessions/native-1.jsonl" }} }};
{driver}
"#
            ),
        )
        .expect("driver");
        let reports = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            // The driver exits after its last report; the listener then sees no more connections.
            listener.set_nonblocking(false).expect("blocking");
            while let Ok((mut stream, _)) = listener.accept() {
                let Some(body) = serve_one(&mut stream) else {
                    break;
                };
                if body == "stop" {
                    break;
                }
                bodies.push(serde_json::from_str::<Value>(&body).expect("JSON report"));
            }
            bodies
        });
        let status = Command::new("node")
            .arg("--experimental-strip-types")
            .arg(&driver_path)
            .env("AGENT_SESSION_TOKEN", "token")
            .env("AGENT_SESSION_ID", "session-1")
            .output()
            .expect("Node.js runs the extension, as it does in every Sandbox and CI host");
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
        // Unblock the accept loop.
        let mut stop = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stop.write_all(b"POST / HTTP/1.1\r\ncontent-length: 4\r\n\r\nstop")
            .expect("stop");
        reports.join().expect("reports")
    }

    fn summary(reports: &[Value]) -> Vec<(String, String)> {
        reports
            .iter()
            .map(|report| {
                (
                    report["event"].as_str().expect("event").to_owned(),
                    report["source"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
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
    fn instructions_join_the_system_prompt_as_a_section_beside_the_agents_own() {
        // The driver exits non-zero when an assertion fails.
        let reports = run_with_instructions(
            r#"
import assert from "node:assert";
const options = { sections: { existing: "kept" } };
await handlers.before_agent_start({ messageType: "user", systemPromptOptions: options });
assert.deepStrictEqual(options.sections, { existing: "kept", agent_platform: "be brief" });
const bare = { sections: {} };
await handlers.before_agent_start({ messageType: "custom", systemPromptOptions: bare });
assert.deepStrictEqual(bare.sections, { agent_platform: "be brief" });
"#,
            Some("be brief\n"),
        );
        assert_eq!(summary(&reports), pairs(&[("turnStarted", "user")]));
    }

    #[test]
    fn without_instructions_the_system_prompt_is_left_alone() {
        let reports = run(r#"
import assert from "node:assert";
const options = { sections: { existing: "kept" } };
await handlers.before_agent_start({ messageType: "user", systemPromptOptions: options });
assert.deepStrictEqual(options.sections, { existing: "kept" });
"#);
        assert_eq!(summary(&reports), pairs(&[("turnStarted", "user")]));
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
