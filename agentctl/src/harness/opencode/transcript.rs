//! Reading an `OpenCode` session from its database, and parsing it into runtime-neutral turns.
//!
//! `OpenCode` keeps every session in one `SQLite` database, as rows of `session_message`. The read
//! script prints one session's rows as JSON lines, `{"seq", "type", "data"}` each, in order: a
//! `user` row is a prompt, an `assistant` row holds every content part of one model step (text,
//! reasoning and tool calls, each tool call with its own result state), and an `idle` row closes
//! each turn with its outcome. A prompt submitted while a turn runs is steered into it, so it
//! belongs to that turn. Synthetic, system, skill, compaction and switch rows are not conversation.

use serde_json::Value;

use crate::{
    Error,
    sessions::{Message, Part, Role, Turn},
};

/// Prints the session `$3` from the database `$1`, at most its last `$2` bytes. The binary runs
/// the reader as Bun, so the image needs neither Node.js nor `sqlite3`. A session `OpenCode` has
/// not reported yet has no rows to read.
pub(crate) const READ_SCRIPT: &str = concat!(
    "[ -f \"$1\" ] || exit 0; [ -n \"$3\" ] || exit 0; BUN_BE_BUN=1 exec opencode -e '",
    include_str!("transcript_reader.ts"),
    "' \"$1\" \"$3\" \"$2\""
);

/// Longest model error shown in a turn; gateways embed whole request diagnostics.
const MAX_ERROR_CHARACTERS: usize = 1000;

/// Parses an `OpenCode` session's rows into ordered turns.
///
/// # Errors
///
/// Returns an error when the transcript is not UTF-8. Lines that are not JSON objects are
/// skipped, so a partially printed trailing line never fails a read.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Turn>, Error> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| Error::Session(format!("transcript is not UTF-8: {error}")))?;
    let mut builder = Builder::default();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        builder.push(&row);
    }
    Ok(builder.turns)
}

/// Drops rows up to and including the first `idle` row in a bounded suffix, which ends the turn
/// the suffix starts inside.
pub(crate) fn trim_partial(bytes: &[u8]) -> &[u8] {
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        offset += line.len();
        if serde_json::from_slice::<Value>(line).is_ok_and(|row| kind(&row) == Some("idle")) {
            return &bytes[offset..];
        }
    }
    &bytes[bytes.len()..]
}

fn kind(row: &Value) -> Option<&str> {
    row.get("type").and_then(Value::as_str)
}

#[derive(Default)]
struct Builder {
    turns: Vec<Turn>,
    /// Whether the last turn has not reached its `idle` row, so a prompt steers into it.
    open: bool,
}

impl Builder {
    fn push(&mut self, row: &Value) {
        let Some(data) = row.get("data") else {
            return;
        };
        match kind(row) {
            Some("user") => self.push_user(data),
            Some("assistant") => self.push_assistant(data),
            Some("idle") => self.push_idle(data),
            // `shell` is the operator's own `!command`, which the model does not answer; Pi's
            // transcript leaves its equivalent out too.
            _ => {}
        }
    }

    fn push_user(&mut self, data: &Value) {
        let Some(text) = data.get("text").and_then(Value::as_str).filter(|text| !text.is_empty()) else {
            return;
        };
        let user = Message {
            role: Role::User,
            parts: vec![Part::Text { text: text.to_owned() }],
        };
        if self.open && !self.turns.is_empty() {
            self.current_turn().messages.push(user);
        } else {
            self.turns.push(Turn { messages: vec![user] });
        }
        self.open = true;
    }

    fn push_assistant(&mut self, data: &Value) {
        let Some(content) = data.get("content").and_then(Value::as_array) else {
            return;
        };
        let mut parts = Vec::new();
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Models emit blank text around tool calls and reasoning.
                    if let Some(text) = part
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| text.trim_matches(['\n', '\r']))
                        .filter(|text| !text.trim().is_empty())
                    {
                        parts.push(Part::Text { text: text.to_owned() });
                    }
                }
                Some("tool") => {
                    if let Some(name) = part.get("name").and_then(Value::as_str) {
                        parts.push(Part::ToolCall {
                            name: name.to_owned(),
                            failed: tool_failed(part),
                        });
                    }
                }
                _ => {}
            }
        }
        // A failed model request leaves an empty step; the `idle` row that follows carries the
        // error.
        self.push_assistant_parts(parts);
    }

    fn push_idle(&mut self, data: &Value) {
        // Show the reason a turn failed, as the other adapters show API errors, as assistant text.
        // An interruption is not a failure, and the aborted tool call is already marked failed.
        if data.get("outcome").and_then(Value::as_str) == Some("failed")
            && let Some(error) = data
                .pointer("/error/message")
                .and_then(Value::as_str)
                .filter(|error| !error.is_empty())
        {
            self.push_assistant_parts(vec![Part::Text {
                text: format!(
                    "Error: {}",
                    error.chars().take(MAX_ERROR_CHARACTERS).collect::<String>()
                ),
            }]);
        }
        self.open = false;
    }

    fn push_assistant_parts(&mut self, parts: Vec<Part>) {
        if parts.is_empty() {
            return;
        }
        self.current_turn().messages.push(Message {
            role: Role::Assistant,
            parts,
        });
    }

    /// The running turn; a transcript that starts mid-conversation opens one.
    fn current_turn(&mut self) -> &mut Turn {
        if self.turns.is_empty() {
            self.turns.push(Turn::default());
        }
        let last = self.turns.len() - 1;
        &mut self.turns[last]
    }
}

/// A tool call failed when `OpenCode` recorded an error, which an interruption also is, or when the
/// command it ran exited non-zero, which `OpenCode` records as a completed call.
fn tool_failed(part: &Value) -> bool {
    let state = part.get("state");
    state.and_then(|state| state.get("status")).and_then(Value::as_str) == Some("error")
        || state
            .and_then(|state| state.pointer("/metadata/exit"))
            .and_then(Value::as_i64)
            .is_some_and(|exit| exit != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `OpenCode` 2.0.32 sessions on the aivar gateway, as the read script prints them.
    ///
    /// A multiline, XML-shaped prompt with a request heading, a 40-line pasted prompt, and a
    /// foreground tool call interrupted with Escape.
    const TERMINAL: &str = include_str!("../../../tests/fixtures/opencode-session.jsonl");
    /// A failing and a succeeding shell call in one step.
    const TOOLS: &str = include_str!("../../../tests/fixtures/opencode-session-tools.jsonl");
    /// A model request a hosted provider rejected.
    const ERROR: &str = include_str!("../../../tests/fixtures/opencode-session-error.jsonl");
    /// The question tool, answered by the operator.
    const QUESTION: &str = include_str!("../../../tests/fixtures/opencode-session-question.jsonl");

    fn text(part: &Part) -> &str {
        match part {
            Part::Text { text } => text,
            Part::ToolCall { .. } => panic!("expected text"),
        }
    }

    fn row(seq: u64, kind: &str, data: &Value) -> String {
        serde_json::json!({"seq": seq, "type": kind, "data": data}).to_string()
    }

    fn user(seq: u64, text: &str) -> String {
        row(seq, "user", &serde_json::json!({"text": text, "files": []}))
    }

    fn assistant(seq: u64, content: &Value) -> String {
        row(
            seq,
            "assistant",
            &serde_json::json!({"agent": "build", "model": {"id": "m", "providerID": "p"}, "content": content}),
        )
    }

    fn idle(seq: u64, outcome: &str) -> String {
        row(seq, "idle", &serde_json::json!({"outcome": outcome}))
    }

    fn tools(turn: &Turn) -> Vec<(&str, bool)> {
        turn.messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                Part::ToolCall { name, failed } => Some((name.as_str(), *failed)),
                Part::Text { .. } => None,
            })
            .collect()
    }

    fn prompts(turns: &[Turn]) -> Vec<&str> {
        turns.iter().map(|turn| text(&turn.messages[0].parts[0])).collect()
    }

    #[test]
    fn recorded_sessions_keep_operator_input_intact() {
        let turns = parse(TERMINAL.as_bytes()).expect("parse");
        assert_eq!(turns.len(), 3);
        assert_eq!(
            text(&turns[0].messages[0].parts[0]),
            "line one of a prompt\nline two <tag attr=\"x\">xml</tag>\n\n## Request\nreply with the word ok "
        );
        let pasted = text(&turns[1].messages[0].parts[0]);
        assert!(
            pasted.starts_with("filler line 1 of a long pasted prompt\n"),
            "{pasted}"
        );
        assert!(pasted.contains("filler line 40 of a long pasted prompt"), "{pasted}");
        assert_eq!(
            prompts(&turns)[2],
            "Run the shell command `timeout 90 tail -f /dev/null` in the foreground and wait for it."
        );
    }

    #[test]
    fn answers_skip_reasoning_and_trim_the_blank_lines_around_them() {
        let turns = parse(TERMINAL.as_bytes()).expect("parse");
        assert_eq!(turns[0].messages.len(), 2);
        assert_eq!(turns[0].messages[1].role, Role::Assistant);
        assert_eq!(turns[0].messages[1].parts.len(), 1);
        assert_eq!(text(&turns[0].messages[1].parts[0]), "ok");
        assert_eq!(text(&turns[1].messages[1].parts[0]), "large");
    }

    #[test]
    fn an_interrupted_tool_call_is_failed_and_the_interruption_is_no_error() {
        let turns = parse(TERMINAL.as_bytes()).expect("parse");
        assert_eq!(tools(&turns[2]), [("shell", true)]);
        assert!(
            turns[2]
                .messages
                .iter()
                .flat_map(|message| &message.parts)
                .all(|part| !matches!(part, Part::Text { text } if text.starts_with("Error:"))),
            "{turns:?}"
        );
    }

    #[test]
    fn a_command_that_exits_non_zero_is_a_failed_call() {
        let turns = parse(TOOLS.as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(tools(&turns[0]), [("shell", true), ("shell", false)]);
        let last = turns[0].messages.last().expect("answer");
        assert_eq!(text(&last.parts[0]), "done");
    }

    #[test]
    fn a_recorded_model_error_is_shown_after_the_prompt() {
        let turns = parse(ERROR.as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(text(&turns[0].messages[0].parts[0]), "hi");
        assert_eq!(turns[0].messages.len(), 2, "{turns:?}");
        assert_eq!(
            text(&turns[0].messages[1].parts[0]),
            "Error: OpenCode's free tier can only be used from within OpenCode. Download it at https://opencode.ai and keep building."
        );
    }

    #[test]
    fn a_long_error_is_truncated() {
        let lines = [
            user(1, "go"),
            row(
                2,
                "idle",
                &serde_json::json!({"outcome": "failed", "error": {"type": "x", "message": "x".repeat(5000)}}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(
            text(&turns[0].messages[1].parts[0]).chars().count(),
            "Error: ".len() + MAX_ERROR_CHARACTERS
        );
    }

    #[test]
    fn an_answered_question_stays_in_its_turn() {
        let turns = parse(QUESTION.as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(tools(&turns[0]), [("question", false)]);
        let last = turns[0].messages.last().expect("answer");
        assert_eq!(text(&last.parts[0]), "You said you prefer **tea**!");
    }

    #[test]
    fn a_prompt_steered_into_a_running_turn_joins_it() {
        let lines = [
            user(1, "first"),
            assistant(
                2,
                &serde_json::json!([{"type": "tool", "id": "c", "name": "shell", "state": {"status": "running", "input": {}}}]),
            ),
            user(3, "steering"),
            assistant(4, &serde_json::json!([{"type": "text", "text": "both"}])),
            idle(5, "succeeded"),
            user(6, "next"),
            idle(7, "succeeded"),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(prompts(&turns), ["first", "next"]);
        let users = turns[0]
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| text(&message.parts[0]))
            .collect::<Vec<_>>();
        assert_eq!(users, ["first", "steering"]);
        assert_eq!(tools(&turns[0]), [("shell", false)]);
    }

    #[test]
    fn rows_that_are_not_conversation_are_left_out() {
        let lines = [
            row(1, "synthetic", &serde_json::json!({"text": "injected"})),
            row(2, "system", &serde_json::json!({"text": "update"})),
            row(
                3,
                "skill",
                &serde_json::json!({"skill": "s", "name": "s", "text": "body"}),
            ),
            row(
                4,
                "shell",
                &serde_json::json!({"command": "ls", "status": "completed", "exit": 0}),
            ),
            row(
                5,
                "compaction",
                &serde_json::json!({"status": "completed", "reason": "auto", "summary": "s", "recent": ""}),
            ),
            row(
                6,
                "model-switched",
                &serde_json::json!({"model": {"id": "m", "providerID": "p"}}),
            ),
            user(7, ""),
        ];
        assert!(parse(lines.join("\n").as_bytes()).expect("parse").is_empty());
    }

    #[test]
    fn a_bounded_suffix_starts_after_the_turn_it_cuts() {
        let suffix = [
            assistant(2, &serde_json::json!([{"type": "text", "text": "partial"}])),
            idle(3, "succeeded"),
            user(4, "complete"),
            assistant(5, &serde_json::json!([{"type": "text", "text": "done"}])),
            idle(6, "succeeded"),
        ]
        .join("\n");

        let turns = parse(trim_partial(suffix.as_bytes())).expect("suffix");

        assert_eq!(prompts(&turns), ["complete"]);
        assert!(trim_partial(user(1, "running").as_bytes()).is_empty());
    }

    #[test]
    fn a_torn_trailing_line_does_not_fail_the_read() {
        let torn = format!("{}\n{{\"seq\":2,\"type\":\"assi", user(1, "hi"));
        assert_eq!(parse(torn.as_bytes()).expect("parse").len(), 1);
        assert!(parse(b"\xff").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn the_read_script_runs_the_reader_as_bun_on_the_reported_session() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("directory");
        let database = directory.path().join("opencode.db");
        std::fs::write(&database, "").expect("database");
        let database = database.to_str().expect("UTF-8 path");
        // A stub that prints what it runs with, NUL-separated.
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let stub = bin.join("opencode");
        std::fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s\\0' \"BUN_BE_BUN=$BUN_BE_BUN\"; for argument in \"$@\"; do printf '%s\\0' \"$argument\"; done\n",
        )
        .expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("permissions");
        let read = |arguments| {
            let output = crate::harness::tests::read_transcript(crate::Harness::OpenCode, Some(&bin), arguments);
            String::from_utf8(output).expect("UTF-8")
        };

        let ran = read([database, "2097153", "ses_1"]);
        let ran = ran.split_terminator('\0').collect::<Vec<_>>();
        assert_eq!(
            ran,
            [
                "BUN_BE_BUN=1",
                "-e",
                include_str!("transcript_reader.ts"),
                database,
                "ses_1",
                "2097153"
            ]
        );
        // Nothing to read before OpenCode reports a session, or creates its database.
        assert_eq!(read([database, "100", ""]), "");
        assert_eq!(read(["/nonexistent/opencode.db", "100", "ses_1"]), "");
    }

    #[test]
    fn the_reader_can_be_single_quoted_into_the_script() {
        assert!(!include_str!("transcript_reader.ts").contains('\''));
        assert!(READ_SCRIPT.starts_with("[ -f \"$1\" ] || exit 0; [ -n \"$3\" ] || exit 0; "));
        assert!(READ_SCRIPT.ends_with("' \"$1\" \"$3\" \"$2\""));
    }
}
