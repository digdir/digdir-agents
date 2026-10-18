//! Parsing the Pi session JSONL into runtime-neutral turns.
//!
//! A session file starts with a header line and then holds one entry per line, each naming its
//! `parentId`: the entries form a tree, and the conversation is the path from the current leaf
//! (the last entry written) back to the root. Entries off that path are abandoned branches.
//! Messages are whole: an assistant entry holds every content block of one model response, and a
//! tool result is its own `toolResult` entry. Compaction, branch summaries and extension messages
//! are entries of other types and are not conversation.
//!
//! A `user` message starts a turn unless it was steered into a run that is still going, which is
//! when it directly follows a tool result or an assistant message that ended in tool calls.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    Error,
    sessions::{Message, Part, Role, Turn},
};

/// Longest model error shown in a turn; gateways embed whole request diagnostics.
const MAX_ERROR_CHARACTERS: usize = 1000;

/// What Node reports for an operation Escape cancelled; Pi records it as the error of the
/// assistant message the aborted request leaves behind.
const ABORTED_MESSAGE: &str = "This operation was aborted";

/// Parses a Pi session file into ordered turns.
///
/// # Errors
///
/// Returns an error when the transcript is not UTF-8. Lines that are not JSON objects are
/// skipped, so a partially written trailing line never fails a read.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Turn>, Error> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| Error::Session(format!("transcript is not UTF-8: {error}")))?;
    let mut entries: HashMap<String, Value> = HashMap::new();
    let mut leaf = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // The header carries the session's ID but is not part of the tree.
        if entry.get("type").and_then(Value::as_str) == Some("session") {
            continue;
        }
        let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        leaf = Some(id.clone());
        entries.insert(id, entry);
    }
    let mut path = Vec::new();
    let mut next = leaf.as_deref();
    while let Some(id) = next {
        // A suffix of a long file lacks the ancestors before it; the walk ends there.
        let Some(entry) = entries.get(id) else {
            break;
        };
        path.push(entry);
        next = entry.get("parentId").and_then(Value::as_str);
        // A parent cycle only arises from a corrupted file.
        if path.len() > entries.len() {
            break;
        }
    }
    // A context edit without a replacement drops its target from the conversation: Pi records
    // one for each attempt it retried, such as a rejected request or an overflowing response.
    let omitted = path
        .iter()
        .filter(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("context_edit")
                && entry.get("replacement").is_some_and(Value::is_null)
        })
        .filter_map(|entry| entry.get("targetId").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    let mut builder = Builder::default();
    for entry in path.into_iter().rev() {
        let id = entry.get("id").and_then(Value::as_str);
        if id.is_some_and(|id| omitted.contains(id)) && matches!(role(entry), Some("assistant" | "toolResult")) {
            continue;
        }
        builder.push(entry);
    }
    Ok(builder.turns)
}

/// Drops records before the first operator prompt in a bounded file suffix.
pub(crate) fn trim_partial(bytes: &[u8]) -> &[u8] {
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if serde_json::from_slice::<Value>(line).is_ok_and(|entry| role(&entry) == Some("user")) {
            return &bytes[offset..];
        }
        offset += line.len();
    }
    &bytes[bytes.len()..]
}

/// The role of a `message` entry.
fn role(entry: &Value) -> Option<&str> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    entry.pointer("/message/role").and_then(Value::as_str)
}

/// Pi stores `/skill:name args` with the skill's whole file in place of the command; `turns` shows
/// what the operator typed.
fn skill_command(text: &str) -> Option<String> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let (name, rest) = rest.split_once('"')?;
    let (_, arguments) = rest.split_once("\n</skill>")?;
    let arguments = arguments.trim();
    Some(if arguments.is_empty() {
        format!("/skill:{name}")
    } else {
        format!("/skill:{name} {arguments}")
    })
}

#[derive(Default)]
struct Builder {
    turns: Vec<Turn>,
    /// Location of each recorded tool call by Pi's tool call ID, so a later
    /// `toolResult` can mark it failed.
    tool_calls: HashMap<String, (usize, usize, usize)>,
    /// Whether the run is mid-flight: the last conversation entry was a tool result or an
    /// assistant message that asked for tools, so the next user message is steering.
    mid_run: bool,
}

impl Builder {
    fn push(&mut self, entry: &Value) {
        let Some(message) = entry.get("message") else {
            return;
        };
        match role(entry) {
            Some("user") => self.push_user(message),
            Some("assistant") => self.push_assistant(message),
            Some("toolResult") => {
                self.record_tool_result(message);
                self.mid_run = true;
            }
            _ => {}
        }
    }

    fn push_user(&mut self, message: &Value) {
        let text = match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let text = skill_command(&text).unwrap_or(text);
        let user = Message {
            role: Role::User,
            parts: vec![Part::Text { text }],
        };
        // Steering joins the running turn, however many messages are steered in.
        if self.mid_run && !self.turns.is_empty() {
            self.current_turn().messages.push(user);
        } else {
            self.turns.push(Turn { messages: vec![user] });
            self.mid_run = false;
        }
    }

    fn push_assistant(&mut self, message: &Value) {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        let mut parts = Vec::new();
        let mut calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Some models emit blank text blocks around tool calls.
                    if let Some(text) = block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| text.trim_matches(['\n', '\r']))
                        .filter(|text| !text.trim().is_empty())
                    {
                        parts.push(Part::Text { text: text.to_owned() });
                    }
                }
                Some("toolCall") => {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            calls.push((id.to_owned(), parts.len()));
                        }
                        parts.push(Part::ToolCall {
                            name: name.to_owned(),
                            failed: false,
                        });
                    }
                }
                _ => {}
            }
        }
        let stop_reason = message.get("stopReason").and_then(Value::as_str);
        self.mid_run = stop_reason == Some("toolUse");
        // A failed model request is stored as an empty message; show the gateway's reason the way
        // the other adapters show API errors, as assistant text. Escape is not a failure: the
        // other adapters leave an interruption out of the turn, and the aborted tool call is
        // already marked failed.
        if stop_reason == Some("error")
            && let Some(error) = message
                .get("errorMessage")
                .and_then(Value::as_str)
                .filter(|error| *error != ABORTED_MESSAGE)
        {
            parts.push(Part::Text {
                text: format!(
                    "Error: {}",
                    error.chars().take(MAX_ERROR_CHARACTERS).collect::<String>()
                ),
            });
        }
        if parts.is_empty() {
            return;
        }
        let turn_index = self.turns.len().saturating_sub(1);
        let turn = self.current_turn();
        turn.messages.push(Message {
            role: Role::Assistant,
            parts,
        });
        let message_index = turn.messages.len() - 1;
        for (id, part) in calls {
            self.tool_calls.insert(id, (turn_index, message_index, part));
        }
    }

    fn record_tool_result(&mut self, message: &Value) {
        if message.get("isError").and_then(Value::as_bool) != Some(true) {
            return;
        }
        let Some(id) = message.get("toolCallId").and_then(Value::as_str) else {
            return;
        };
        if let Some(&(turn, message, part)) = self.tool_calls.get(id)
            && let Some(Part::ToolCall { failed, .. }) = self
                .turns
                .get_mut(turn)
                .and_then(|turn| turn.messages.get_mut(message))
                .and_then(|message| message.parts.get_mut(part))
        {
            *failed = true;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Pi 1.3.0 session against a scripted gateway: a multiline prompt, a tool call, a failing
    /// tool call, an Escape during a tool call, and a prompt steered into a running turn.
    const FIXTURE: &str = include_str!("../../../tests/fixtures/pi-session.jsonl");

    fn text(part: &Part) -> &str {
        match part {
            Part::Text { text } => text,
            Part::ToolCall { .. } => panic!("expected text"),
        }
    }

    fn entry(id: &str, parent: Option<&str>, message: &serde_json::Value) -> String {
        serde_json::json!({"type": "message", "id": id, "parentId": parent, "message": message}).to_string()
    }

    fn user(id: &str, parent: Option<&str>, content: &str) -> String {
        entry(id, parent, &serde_json::json!({"role": "user", "content": content}))
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

    #[test]
    fn recorded_session_yields_operator_turns_only() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let prompts = turns
            .iter()
            .map(|turn| text(&turn.messages[0].parts[0]))
            .collect::<Vec<_>>();
        assert_eq!(
            prompts,
            [
                "line one\nline two",
                "please use a tool",
                "make it fail",
                "run slow thing",
                "run slow thing again"
            ]
        );
    }

    #[test]
    fn input_steered_into_a_running_turn_joins_it() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let last = turns.last().expect("turn");
        let users = last
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| text(&message.parts[0]))
            .collect::<Vec<_>>();
        assert_eq!(users, ["run slow thing again", "steering message"]);
        assert!(
            matches!(last.messages.last().and_then(|message| message.parts.last()), Some(Part::Text { text }) if text == "Echo: steering message")
        );
    }

    #[test]
    fn tool_results_mark_failures_and_an_interrupted_call_is_failed() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(tools(&turns[0]), []);
        assert_eq!(tools(&turns[1]), [("bash", false)]);
        assert_eq!(tools(&turns[2]), [("bash", true)]);
        assert_eq!(tools(&turns[3]), [("bash", true)], "Escape aborts the call");
        assert_eq!(tools(&turns[4]), [("bash", false)]);
    }

    #[test]
    fn an_assistant_entry_is_one_message_and_thinking_is_not_conversation() {
        let lines = [
            user("a", None, "go"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "stopReason": "stop", "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "text", "text": "first"},
                    {"type": "toolCall", "id": "c1", "name": "read", "arguments": {}},
                    {"type": "text", "text": "second"},
                ]}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].messages.len(), 2);
        assert_eq!(turns[0].messages[1].parts.len(), 3);
    }

    #[test]
    fn blank_text_is_dropped_and_surrounding_blank_lines_are_trimmed() {
        let lines = [
            user("a", None, "go"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "stopReason": "toolUse", "content": [
                    {"type": "text", "text": "\n\n"},
                    {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {}},
                ]}),
            ),
            entry(
                "c",
                Some("b"),
                &serde_json::json!({"role": "assistant", "stopReason": "stop", "content": [
                    {"type": "text", "text": "\n\nanswer\n  more\n\n"},
                ]}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns[0].messages.len(), 3);
        assert_eq!(turns[0].messages[1].parts.len(), 1, "only the tool call");
        assert_eq!(text(&turns[0].messages[2].parts[0]), "answer\n  more");
    }

    #[test]
    fn a_recorded_gateway_rejection_is_shown_after_the_prompt() {
        // A real Pi 1.3.0 session whose first request the gateway rejected (a model the key may not use).
        let turns = parse(include_bytes!("../../../tests/fixtures/pi-session-error.jsonl")).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(text(&turns[0].messages[0].parts[0]), "hello");
        assert!(text(&turns[0].messages[1].parts[0]).starts_with("Error: gateway API error (403): {"));
    }

    #[test]
    fn an_interruption_is_not_shown_as_an_error() {
        let lines = [
            user("a", None, "go"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "stopReason": "error", "content": [],
                    "errorMessage": "This operation was aborted"}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].messages.len(), 1, "{turns:?}");
    }

    #[test]
    fn a_failed_model_request_shows_the_gateway_error() {
        let lines = [
            user("a", None, "go"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "stopReason": "error", "content": [],
                    "errorMessage": "gateway API error (400): bad input"}),
            ),
            user("c", Some("b"), "again"),
            entry(
                "d",
                Some("c"),
                &serde_json::json!({"role": "assistant", "stopReason": "error", "content": [],
                    "errorMessage": "x".repeat(5000)}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].messages[1].role, Role::Assistant);
        assert_eq!(
            text(&turns[0].messages[1].parts[0]),
            "Error: gateway API error (400): bad input"
        );
        assert_eq!(
            text(&turns[1].messages[1].parts[0]).chars().count(),
            "Error: ".len() + MAX_ERROR_CHARACTERS
        );
    }

    /// The parts of every message in `turn`, as text, with tool calls by name.
    fn rendered(turn: &Turn) -> Vec<String> {
        turn.messages
            .iter()
            .flat_map(|message| &message.parts)
            .map(|part| match part {
                Part::Text { text } => text.clone(),
                Part::ToolCall { name, .. } => format!("-> {name}"),
            })
            .collect()
    }

    #[test]
    fn an_attempt_pi_retried_is_not_conversation() {
        let lines = [
            serde_json::json!({"type": "session", "version": 3, "id": "header", "cwd": "/w"}).to_string(),
            user("a", None, "hello"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "content": [], "stopReason": "error", "errorMessage": "429 rate limited"}),
            ),
            serde_json::json!({"type": "context_edit", "id": "c", "parentId": "b", "targetId": "b", "replacement": null}).to_string(),
            entry(
                "d",
                Some("c"),
                &serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}], "stopReason": "stop"}),
            ),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns.len(), 1);
        assert_eq!(rendered(&turns[0]), ["hello", "hi"]);
    }

    #[test]
    fn a_skill_command_shows_as_typed() {
        let expanded = "<skill name=\"pr-evidence\" location=\"/s/SKILL.md\">\nReferences are relative to /s.\n\nRecord it.\n</skill>";
        let lines = [
            user("a", None, &format!("{expanded}\n\nrecord the TUI")),
            user("b", Some("a"), expanded),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        let prompts = turns
            .iter()
            .map(|turn| text(&turn.messages[0].parts[0]))
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["/skill:pr-evidence record the TUI", "/skill:pr-evidence"]);
    }

    #[test]
    fn every_message_steered_into_a_running_turn_joins_it() {
        let lines = [
            user("a", None, "start"),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "assistant", "content": [{"type": "toolCall", "id": "t", "name": "bash", "arguments": {}}], "stopReason": "toolUse"}),
            ),
            entry(
                "c",
                Some("b"),
                &serde_json::json!({"role": "toolResult", "toolCallId": "t", "content": [], "isError": false}),
            ),
            user("d", Some("c"), "first steer"),
            user("e", Some("d"), "second steer"),
            entry(
                "f",
                Some("e"),
                &serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}),
            ),
            user("g", Some("f"), "next"),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(turns.len(), 2);
        assert_eq!(
            rendered(&turns[0]),
            ["start", "-> bash", "first steer", "second steer", "done"]
        );
    }

    #[test]
    fn only_the_active_branch_is_conversation() {
        // `c` and `d` hang off `a`; the file ends on `d`, so `b` is an abandoned branch.
        let lines = [
            user("a", None, "first"),
            user("b", Some("a"), "abandoned"),
            user("c", Some("a"), "kept"),
            serde_json::json!({"type": "compaction", "id": "k", "parentId": "c", "summary": "s", "firstKeptEntryId": "a"}).to_string(),
            user("d", Some("k"), "after compaction"),
        ];
        let turns = parse(lines.join("\n").as_bytes()).expect("parse");
        let prompts = turns
            .iter()
            .map(|turn| text(&turn.messages[0].parts[0]))
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["first", "kept", "after compaction"]);
    }

    #[test]
    fn extension_messages_and_images_alone_are_not_operator_input() {
        let lines = [
            serde_json::json!({"type": "custom_message", "id": "a", "parentId": null, "customType": "x", "content": "injected", "display": true}).to_string(),
            entry(
                "b",
                Some("a"),
                &serde_json::json!({"role": "user", "content": [{"type": "image", "data": "", "mimeType": "image/png"}]}),
            ),
            entry("c", Some("b"), &serde_json::json!({"role": "bashExecution", "command": "ls", "output": ""})),
        ];
        assert!(parse(lines.join("\n").as_bytes()).expect("parse").is_empty());
    }

    #[test]
    fn a_bounded_suffix_discards_a_partial_turn() {
        let suffix = format!(
            "{}\n{}\n{}\n",
            entry(
                "x",
                Some("w"),
                &serde_json::json!({"role": "assistant", "stopReason": "stop", "content": [{"type": "text", "text": "partial"}]})
            ),
            user("y", Some("x"), "complete"),
            entry(
                "z",
                Some("y"),
                &serde_json::json!({"role": "assistant", "stopReason": "stop", "content": [{"type": "text", "text": "done"}]})
            ),
        );

        let turns = parse(trim_partial(suffix.as_bytes())).expect("suffix");

        assert_eq!(turns.len(), 1);
        assert_eq!(text(&turns[0].messages[0].parts[0]), "complete");
    }

    #[test]
    fn a_torn_trailing_line_and_a_parent_cycle_do_not_fail_the_read() {
        let torn = format!("{}\n{{\"type\":\"mess", user("a", None, "hi"));
        assert_eq!(parse(torn.as_bytes()).expect("parse").len(), 1);
        let cycle = [user("a", Some("b"), "x"), user("b", Some("a"), "y")].join("\n");
        parse(cycle.as_bytes()).expect("parse");
        assert!(parse(b"\xff").is_err());
    }
}
