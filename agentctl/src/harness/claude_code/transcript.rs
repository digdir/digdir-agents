//! Parsing the Claude Code JSONL transcript into runtime-neutral turns.
//!
//! Claude Code appends one JSON object per line. An operator prompt is a
//! `user` entry whose `message.content` is a string; tool results are `user`
//! entries with a `tool_result` block array; each assistant content block is
//! its own `assistant` entry sharing one `message.id`. The file also records
//! entries that are not conversation: subagent side chains, meta entries,
//! compaction summaries and text the client injects on the user's behalf
//! (slash-command echoes, background task notifications, system reminders,
//! interruption markers).
//! Input the operator sends while a turn is running is not a `user` entry at
//! all: it is an `attachment` of type `queued_command` absorbed into the turn.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    Error,
    sessions::{Message, Part, Question, QuestionOption, Role, Turn, TurnSignals},
};

/// Parses Claude Code JSONL into ordered turns.
///
/// # Errors
///
/// Returns an error when the transcript is not UTF-8. Lines that are not JSON
/// objects are skipped, so a partially written trailing line never fails a read.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Turn>, Error> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| Error::Session(format!("transcript is not UTF-8: {error}")))?;
    let mut builder = Builder::default();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Ok(entry) = serde_json::from_str::<Value>(line) {
            builder.push(&entry);
        }
    }
    Ok(builder.turns)
}

/// Drops records before the first operator prompt in a bounded file suffix.
pub(crate) fn trim_partial(bytes: &[u8]) -> &[u8] {
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if serde_json::from_slice::<Value>(line).is_ok_and(|entry| starts_turn(&entry)) {
            return &bytes[offset..];
        }
        offset += line.len();
    }
    &bytes[bytes.len()..]
}

fn starts_turn(entry: &Value) -> bool {
    if flag(entry, "isSidechain") || flag(entry, "isMeta") || flag(entry, "isCompactSummary") {
        return false;
    }
    entry.get("type").and_then(Value::as_str) == Some("user")
        && user_text(entry).is_some_and(|text| !is_injected(&text))
}

/// The text of a `user` entry's message, without its tool results. An entry that holds only tool
/// results has none.
fn user_text(entry: &Value) -> Option<String> {
    match entry.get("message")?.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Whether `entry` is the [marker](is_interruption_marker) Claude Code writes where the operator
/// stopped a turn. The turn ends there, and no hook says so.
fn is_interruption(entry: &Value) -> bool {
    !flag(entry, "isSidechain")
        && entry.get("type").and_then(Value::as_str) == Some("user")
        && user_text(entry).is_some_and(|text| is_interruption_marker(&text))
}

/// Follows a transcript as it grows, for the turn endings Claude Code fires no hook for.
#[derive(Debug, Default)]
pub(crate) struct TurnTracker {
    interrupted: bool,
}

impl TurnTracker {
    /// Folds complete transcript lines. Lines that are not JSON objects are skipped.
    pub(crate) fn push(&mut self, lines: &[u8]) {
        for line in lines.split(|byte| *byte == b'\n') {
            if let Ok(entry) = serde_json::from_slice::<Value>(line)
                && is_interruption(&entry)
            {
                self.interrupted = true;
            }
        }
    }

    pub(crate) const fn signals(&self) -> TurnSignals {
        TurnSignals {
            interrupted: self.interrupted,
        }
    }
}

#[derive(Default)]
struct Builder {
    turns: Vec<Turn>,
    /// Location of each recorded tool call by Claude's `tool_use` ID, so a
    /// later `tool_result` can mark it failed.
    tool_calls: HashMap<String, (usize, usize, usize)>,
    /// `message.id` of the assistant message the last assistant entry belongs
    /// to; consecutive entries with the same ID are one message.
    assistant_message: Option<String>,
}

impl Builder {
    fn push(&mut self, entry: &Value) {
        if flag(entry, "isSidechain") || flag(entry, "isMeta") || flag(entry, "isCompactSummary") {
            return;
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(message) = entry.get("message") {
                    self.push_user(message);
                }
            }
            Some("assistant") => {
                if let Some(message) = entry.get("message") {
                    self.push_assistant(message);
                }
            }
            Some("attachment") => {
                if let Some(attachment) = entry.get("attachment") {
                    self.push_attachment(attachment);
                }
            }
            _ => {}
        }
    }

    /// Operator input absorbed into the running turn joins that turn as a user
    /// message; other attachments (environment, reminders) are not conversation.
    fn push_attachment(&mut self, attachment: &Value) {
        if attachment.get("type").and_then(Value::as_str) != Some("queued_command")
            || attachment
                .get("origin")
                .and_then(|origin| origin.get("kind"))
                .and_then(Value::as_str)
                != Some("human")
        {
            return;
        }
        let Some(prompt) = attachment.get("prompt").and_then(Value::as_str) else {
            return;
        };
        self.assistant_message = None;
        self.current_turn().messages.push(Message {
            role: Role::User,
            parts: vec![Part::Text {
                text: prompt.to_owned(),
            }],
        });
    }

    fn current_turn(&mut self) -> &mut Turn {
        if self.turns.is_empty() {
            self.turns.push(Turn::default());
        }
        let last = self.turns.len() - 1;
        &mut self.turns[last]
    }

    fn push_user(&mut self, message: &Value) {
        match message.get("content") {
            Some(Value::String(text)) => self.push_prompt(text),
            Some(Value::Array(blocks)) => {
                let mut prompt = String::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_result") => self.record_tool_result(block),
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                prompt.push_str(text);
                            }
                        }
                        _ => {}
                    }
                }
                if !prompt.is_empty() {
                    self.push_prompt(&prompt);
                }
            }
            _ => {}
        }
    }

    /// Starts a turn on an operator prompt; injected client text is not one.
    fn push_prompt(&mut self, text: &str) {
        if is_injected(text) {
            return;
        }
        self.assistant_message = None;
        self.turns.push(Turn {
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text { text: text.to_owned() }],
            }],
        });
    }

    fn record_tool_result(&mut self, block: &Value) {
        if block.get("is_error").and_then(Value::as_bool) != Some(true) {
            return;
        }
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
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

    fn push_assistant(&mut self, message: &Value) {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        let id = message.get("id").and_then(Value::as_str);
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        self.append_assistant_part(id, Part::Text { text: text.to_owned() });
                    }
                }
                Some("tool_use") => {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        let location = self.append_assistant_part(
                            id,
                            Part::ToolCall {
                                name: name.to_owned(),
                                failed: false,
                                questions: questions(name, block.get("input")),
                            },
                        );
                        if let Some(tool_use) = block.get("id").and_then(Value::as_str) {
                            self.tool_calls.insert(tool_use.to_owned(), location);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Appends a part to the current assistant message, opening a new message
    /// when the entry belongs to a different API response, and a new turn when
    /// the transcript starts mid-conversation.
    fn append_assistant_part(&mut self, id: Option<&str>, part: Part) -> (usize, usize, usize) {
        if self.turns.is_empty() {
            self.turns.push(Turn::default());
        }
        let turn_index = self.turns.len() - 1;
        let turn = &mut self.turns[turn_index];
        let continues = id.is_some() && id == self.assistant_message.as_deref();
        if !continues || turn.messages.last().is_none_or(|last| last.role != Role::Assistant) {
            turn.messages.push(Message {
                role: Role::Assistant,
                parts: Vec::new(),
            });
            self.assistant_message = id.map(str::to_owned);
        }
        let message_index = turn.messages.len() - 1;
        let message = &mut turn.messages[message_index];
        message.parts.push(part);
        (turn_index, message_index, message.parts.len() - 1)
    }
}

/// The questions an `AskUserQuestion` call puts to the operator. Its input is Claude Code's own
/// and may change shape, so a question without text, or an option without a label, is left out,
/// and a call with no question left shows as the bare tool call.
fn questions(tool: &str, input: Option<&Value>) -> Vec<Question> {
    if tool != "AskUserQuestion" {
        return Vec::new();
    }
    let text = |value: &Value, field| value.get(field).and_then(Value::as_str).map(str::to_owned);
    input
        .and_then(|input| input.get("questions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| {
            Some(Question {
                question: text(question, "question")?,
                header: text(question, "header").unwrap_or_default(),
                multi_select: flag(question, "multiSelect"),
                options: question
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        Some(QuestionOption {
                            label: text(option, "label")?,
                            description: text(option, "description").unwrap_or_default(),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

fn flag(entry: &Value, name: &str) -> bool {
    entry.get(name).and_then(Value::as_bool) == Some(true)
}

/// Recognizes the harness's local-command and notification envelopes, and its
/// [interruption marker](is_interruption_marker). Ordinary XML and bracketed
/// text are operator input; explicitly human queued attachments bypass this
/// filter.
fn is_injected(text: &str) -> bool {
    let trimmed = text.trim();
    [
        "command-name",
        "local-command-stdout",
        "local-command-stderr",
        "task-notification",
        "system-reminder",
    ]
    .iter()
    .any(|tag| trimmed.starts_with(&format!("<{tag}>")) && trimmed.contains(&format!("</{tag}>")))
        || is_interruption_marker(trimmed)
}

/// Whether `text` is the marker Claude Code writes where the operator stopped a
/// turn, such as `[Request interrupted by user]` after Esc while it answers, or
/// `[Request interrupted by user for tool use]` after Esc during a tool call or
/// a refusal of one that says nothing more. Each such marker Claude Code writes
/// as user text starts with this prefix. Text after the closing bracket makes it
/// the operator's own prompt.
fn is_interruption_marker(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("[Request interrupted") && text.find(']') == Some(text.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/claude-code-transcript.jsonl");

    #[test]
    fn a_bounded_suffix_discards_a_partial_turn() {
        let suffix = concat!(
            r#"{"type":"assistant","message":{"id":"old","content":[{"type":"text","text":"partial"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"complete"}}"#,
            "\n",
        );

        let turns = parse(trim_partial(suffix.as_bytes())).expect("suffix");

        assert!(matches!(turns[0].messages[0].parts[0], Part::Text { ref text } if text == "complete"));
    }

    #[test]
    fn recorded_transcript_yields_operator_turns_only() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let prompts = turns
            .iter()
            .map(|turn| match &turn.messages[0].parts[0] {
                Part::Text { text } => text.as_str(),
                Part::ToolCall { .. } => panic!("a turn starts with the prompt"),
            })
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["Rename the helper and run the tests.", "Now commit it."]);
    }

    #[test]
    fn input_absorbed_mid_turn_joins_the_running_turn() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let second = &turns[1];
        let users = second
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| match &message.parts[0] {
                Part::Text { text } => text.as_str(),
                Part::ToolCall { .. } => panic!("user text"),
            })
            .collect::<Vec<_>>();
        assert_eq!(users, ["Now commit it.", "Use a conventional commit message."]);
        assert_eq!(turns.len(), 2, "absorbed input does not start a turn");
    }

    #[test]
    fn tool_results_mark_failures_and_the_final_message_follows_the_last_tool_call() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let first = &turns[0];
        let tools = first
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                Part::ToolCall { name, failed, .. } => Some((name.as_str(), *failed)),
                Part::Text { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tools, [("Edit", false), ("Bash", true), ("Bash", false)]);
        assert!(
            matches!(first.messages.last().and_then(|message| message.parts.last()), Some(Part::Text { text }) if text == "Renamed and the tests pass.")
        );
    }

    #[test]
    fn assistant_blocks_of_one_response_form_one_message() {
        let turns = parse(FIXTURE.as_bytes()).expect("parse");
        let assistant_messages = turns[0]
            .messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count();
        // Response 1: commentary + Edit; response 2: Bash; response 3: Bash; response 4: answer.
        assert_eq!(assistant_messages, 4);
        assert_eq!(turns[0].messages[1].parts.len(), 2);
    }

    #[test]
    fn a_question_carries_its_text_and_options() {
        let question = |input: serde_json::Value| {
            let record = serde_json::json!({"type": "assistant", "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "t", "name": "AskUserQuestion", "input": input}
            ]}});
            let turns = parse(record.to_string().as_bytes()).expect("parse");
            match &turns[0].messages[0].parts[0] {
                Part::ToolCall { name, questions, .. } if name == "AskUserQuestion" => questions.clone(),
                part => panic!("unexpected part {part:?}"),
            }
        };

        let asked = question(serde_json::json!({"questions": [{
            "question": "Which role?",
            "header": "Role",
            "multiSelect": false,
            "options": [{"label": "Developer", "description": "Writes code"}, {"description": "no label"}]
        }]}));
        assert_eq!(
            asked,
            [Question {
                question: "Which role?".into(),
                header: "Role".into(),
                multi_select: false,
                options: vec![QuestionOption {
                    label: "Developer".into(),
                    description: "Writes code".into(),
                }],
            }]
        );
        for unknown in [
            serde_json::json!({"questions": "Which role?"}),
            serde_json::json!({"questions": [{"prompt": "Which role?"}]}),
            serde_json::json!(null),
        ] {
            assert_eq!(question(unknown.clone()), [], "{unknown}");
        }
    }

    /// Turns recorded with Claude Code 2.1.321, separated by a blank line: one the operator
    /// refused a tool call in, one interrupted while a tool ran, one interrupted while the answer
    /// was written, and one answered.
    const INTERRUPTIONS: &str = include_str!("../../../tests/fixtures/claude-code-interruptions.jsonl");

    fn signals(lines: &str) -> TurnSignals {
        let mut tracker = TurnTracker::default();
        tracker.push(lines.as_bytes());
        tracker.signals()
    }

    #[test]
    fn a_refused_or_interrupted_turn_has_ended() {
        let turns = INTERRUPTIONS.split("\n\n").collect::<Vec<_>>();
        let [refused, during_tool, during_answer, answered] = turns[..] else {
            panic!("four recorded turns");
        };
        for (case, turn) in [
            ("refused", refused),
            ("interrupted during a tool call", during_tool),
            ("interrupted during the answer", during_answer),
        ] {
            assert!(signals(turn).interrupted, "{case}");
        }
        assert!(!signals(answered).interrupted, "answered");
    }

    const INTERRUPTED_THEN_PROMPTED: &str = r#"{"type":"api-request-blob","sessionId":"484733e9-d6e4-4788-a6a8-496b294e8969","hash":"5f805a933bb67ac44d78e1bb00fb8470f737f3d02d3e10c10827277839977be4","message":{"role":"user","content":[{"type":"tool_result","content":"The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.","is_error":true,"tool_use_id":"toolu_01JT6UL3E2BqrvKB7LnXA4Uv"},{"type":"text","text":"[Request interrupted by user for tool use]\n"},{"type":"text","text":"Write a 2000-word story about a harbour pilot, as plain text, without using any tools."}]}}"#;

    #[test]
    fn only_claude_codes_own_marker_is_an_interruption() {
        for entry in [
            serde_json::json!({"type": "user", "message": {"content": "[Request interrupted by user] Go on."}}),
            serde_json::json!({"type": "user", "message": {"content": "Why did it say [Request interrupted by user]?"}}),
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
            serde_json::json!({"type": "user", "isSidechain": true, "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}}),
            // Claude Code 2.1.321 writes this after the prompt that follows an interruption, past the
            // mark that prompt's wait took.
            serde_json::from_str(INTERRUPTED_THEN_PROMPTED).expect("JSON"),
        ] {
            assert!(!signals(&entry.to_string()).interrupted, "{entry}");
        }
        let marker = serde_json::json!({"type": "user", "message": {"content": "[Request interrupted by user]"}});
        assert!(signals(&marker.to_string()).interrupted, "{marker}");
    }

    #[test]
    fn an_interruption_marker_does_not_start_a_turn() {
        let fixture = include_str!("../../../tests/fixtures/claude-code-interrupted.jsonl");
        let turns = parse(fixture.as_bytes()).expect("parse");
        let users = turns
            .iter()
            .map(|turn| {
                turn.messages
                    .iter()
                    .filter(|message| message.role == Role::User)
                    .map(|message| match &message.parts[0] {
                        Part::Text { text } => text.as_str(),
                        Part::ToolCall { .. } => panic!("user text"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let story = "Write a 700-word story about a lighthouse keeper, as plain text, without using any tools.";
        assert_eq!(
            users,
            [
                vec!["Run the shell command `timeout 90 tail -f /dev/null` in the foreground with the Bash tool."],
                vec![story],
                vec!["Reply with the word ok."],
            ]
        );
        let last_part = |turn: &Turn| turn.messages.last().and_then(|message| message.parts.last()).cloned();
        assert!(
            matches!(last_part(&turns[0]), Some(Part::ToolCall { name, failed: true, .. }) if name == "Bash"),
            "Esc during a tool call ends the turn at the stopped call"
        );
        assert!(
            matches!(last_part(&turns[1]), Some(Part::Text { text }) if text.starts_with("The lamp had not gone dark")),
            "Esc during the answer ends the turn at the text written so far"
        );

        let markers = fixture
            .match_indices("[Request interrupted")
            .map(|(offset, _)| offset)
            .collect::<Vec<_>>();
        assert_eq!(markers.len(), 2, "both forms of the marker");
        for (marker, next) in markers.into_iter().zip([story, "Reply with the word ok."]) {
            let from_marker = &fixture[fixture[..marker].rfind('\n').map_or(0, |newline| newline + 1)..];
            let kept = std::str::from_utf8(trim_partial(from_marker.as_bytes())).expect("UTF-8");
            let first = kept
                .lines()
                .next()
                .and_then(|line| serde_json::from_str::<Value>(line).ok());
            assert_eq!(
                first.as_ref().and_then(|entry| entry["message"]["content"].as_str()),
                Some(next),
                "a bounded read from the marker starts at the next prompt"
            );
        }
        let prompt = r#"{"type":"user","message":{"role":"user","content":"next"}}"#;
        let marker = r#"{"type":"user","message":{"role":"user","content":"[Request interrupted by user]"}}"#;
        let suffix = format!("{marker}\n{prompt}\n");
        assert_eq!(
            std::str::from_utf8(trim_partial(suffix.as_bytes())),
            Ok(format!("{prompt}\n").as_str()),
            "a marker written as plain string content does not start a bounded read either"
        );
    }

    #[test]
    fn injected_markup_is_not_a_prompt() {
        for injected in [
            "<command-name>/clear</command-name>\n<command-message>clear</command-message>",
            "<task-notification>\n<task-id>x</task-id>\n</task-notification>",
            "<system-reminder>\nnote\n</system-reminder>",
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
            "[Request interrupted: the session ended before this agent finished. Nobody asked it to stop.]",
        ] {
            assert!(is_injected(injected), "{injected}");
        }
        assert!(!is_injected("<div>hi"));
        assert!(!is_injected("Compare <a> and <b>"));
        assert!(!is_injected("plain prompt"));
        assert!(!is_injected("[WIP] fix the build"));
        assert!(!is_injected(
            "[Request interrupted by user] Go on from where you stopped."
        ));
    }
}

#[cfg(test)]
mod input_preservation_tests {
    #[test]
    fn human_xml_prompts_survive_normal_and_queued_records() {
        let prompt = "<task>Reply exactly DONE</task>";
        let records = [
            serde_json::json!({"type":"user", "message":{"content":prompt}}),
            serde_json::json!({"type":"attachment", "attachment":{"type":"queued_command", "origin":{"kind":"human"}, "prompt":prompt}}),
        ];
        for record in records {
            let turns = super::parse(record.to_string().as_bytes()).expect("parse");
            assert_eq!(turns.len(), 1);
            assert_eq!(
                turns[0].messages[0].parts[0],
                crate::sessions::Part::Text { text: prompt.into() }
            );
        }
    }
}
