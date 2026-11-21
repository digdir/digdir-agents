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
//! Input the operator sends while a turn is running is queued. During a tool
//! call it is absorbed into the turn as an `attachment` of type
//! `queued_command`. Otherwise it waits for the turn to end and starts a turn
//! of its own as a `user` entry.

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

/// Follows a transcript as it grows, for what Claude Code fires no hook for: a turn the operator
/// ended, and operator input queued to start a turn of its own.
///
/// Claude Code writes `queue-operation` lines at once and conversation lines in batches, so the
/// `dequeue` that takes queued input sits above the end of the turn before it. What it writes
/// after that end shows whether the input started a turn.
#[derive(Debug, Default)]
pub(crate) struct TurnTracker {
    /// Operator input queued and not yet seen to start a turn: `enqueue`s less the operations that
    /// take it out of the queue without one, such as a `remove` when it is absorbed into the
    /// running turn.
    queued: u32,
    /// What the input the latest `dequeue` took started. A `dequeue` doesn't say whose input it
    /// takes, and Claude Code takes its own input and the operator's in the order they came.
    taken: Taken,
    /// The latest turn the conversation shows has ended. Input is queued only while a turn runs,
    /// so a turn is taken to run at the mark.
    idle: bool,
    /// The operator ended the latest turn that ended.
    interrupted: bool,
    /// The prompt has started its turn or left the queue. Input queued after that is someone
    /// else's, such as a prompt sent without waiting, and the prompt's turn does not wait for it.
    resolved: bool,
    /// The prompt as [`prompt_key`] compares it. A prompt sent just before it can start its turn
    /// below the mark, so a turn that starts with nothing queued is the prompt's only when it holds
    /// the prompt.
    prompt: String,
}

/// What input taken from the queue started.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Taken {
    /// Nothing that has not ended, or a turn for Claude Code's own input.
    #[default]
    Nothing,
    /// Not known yet, since the turn that ran when the input was taken has not ended.
    BeforeEnd,
    /// Not known yet. The turn before has ended, so the next line of the conversation shows it.
    AfterEnd,
    /// A turn that has not ended yet.
    Turn,
}

impl TurnTracker {
    /// A tracker for what follows the mark set before `prompt` is sent.
    pub(crate) fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt_key(prompt),
            ..Self::default()
        }
    }

    /// Folds complete transcript lines. Lines that are not JSON objects are skipped.
    pub(crate) fn push(&mut self, lines: &[u8]) {
        for line in lines.split(|byte| *byte == b'\n') {
            if let Ok(entry) = serde_json::from_slice::<Value>(line) {
                self.fold(&entry);
            }
        }
    }

    fn fold(&mut self, entry: &Value) {
        if entry.get("type").and_then(Value::as_str) == Some("queue-operation") {
            self.fold_queue_operation(entry);
        } else if let Some(origin) = turn_origin(entry) {
            self.idle = false;
            if origin == "human" {
                if self.queued > 0 {
                    // Claude Code may take all the queued input into the one turn.
                    self.queued = 0;
                    self.taken = Taken::Turn;
                    self.resolved = true;
                } else if user_text(entry).is_some_and(|text| prompt_key(&text) == self.prompt) {
                    // The prompt was not queued and started its turn at once.
                    self.resolved = true;
                }
            } else if matches!(self.taken, Taken::BeforeEnd | Taken::AfterEnd) {
                // Claude Code took its own input, such as a notification that a background task
                // finished or a scheduled prompt, and the operator's stays queued.
                self.taken = Taken::Nothing;
            }
        } else if let Some(interrupted) = ends_turn(entry) {
            self.idle = true;
            self.taken = match self.taken {
                Taken::BeforeEnd => Taken::AfterEnd,
                Taken::Turn => Taken::Nothing,
                taken => taken,
            };
            self.interrupted = interrupted;
        } else if self.taken == Taken::AfterEnd && is_conversation(entry) {
            // The input started no turn, such as a slash command Claude Code warns it doesn't know.
            self.leave_queue();
            self.taken = Taken::Nothing;
        }
    }

    fn fold_queue_operation(&mut self, entry: &Value) {
        // Claude Code queues its own input too, and runs it in a turn of its own that the
        // operator's wait should not follow.
        let content = entry.get("content");
        if content.and_then(Value::as_str).is_some_and(is_injected) {
            return;
        }
        match entry.get("operation").and_then(Value::as_str) {
            Some("enqueue") => {
                // Claude Code leaves the text out at times, such as while it screens the input.
                if content.is_some() && !self.resolved {
                    self.queued = self.queued.saturating_add(1);
                }
            }
            Some("dequeue") => {
                if self.queued > 0 {
                    self.taken = if self.idle { Taken::AfterEnd } else { Taken::BeforeEnd };
                }
            }
            _ => self.leave_queue(),
        }
    }

    /// Operator input left the queue without a turn of its own.
    const fn leave_queue(&mut self) {
        // Input queued before the mark can be taken too, so the count stops at zero.
        if self.queued > 0 {
            self.queued -= 1;
            self.resolved |= self.queued == 0;
        }
    }

    pub(crate) const fn signals(&self) -> TurnSignals {
        TurnSignals {
            interrupted: self.interrupted,
            queued: self.queued > 0 || matches!(self.taken, Taken::Turn),
        }
    }
}

/// What [`TurnTracker`] compares of a prompt and of the line that starts a turn. Claude Code writes
/// a pasted prompt of several lines wrapped in `<pasted_content>` tags, with its tabs and line
/// breaks changed, and a slash command as its echo. A prompt that is not recognized only makes the
/// wait follow input queued behind it, so this need not catch every change.
fn prompt_key(text: &str) -> String {
    let between = |open: &str, close: &str| {
        let start = text.find(open)? + open.len();
        text[start..].find(close).map(|end| &text[start..start + end])
    };
    let text = between("<command-name>", "</command-name>").map_or_else(
        || without_tags(text, "pasted_content"),
        |name| {
            format!(
                "{name}{}",
                between("<command-args>", "</command-args>").unwrap_or_default()
            )
        },
    );
    text.chars()
        .filter(|character| !character.is_whitespace() && *character != '\u{200b}')
        .collect()
}

/// `text` without the opening and closing tags named `tag`, whatever their attributes.
fn without_tags(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}"), format!("</{tag}"));
    let mut rest = text;
    let mut kept = String::with_capacity(text.len());
    while let Some(start) = [rest.find(&open), rest.find(&close)].into_iter().flatten().min() {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        kept.push_str(&rest[..start]);
        rest = &rest[start + end + 1..];
    }
    kept.push_str(rest);
    kept
}

/// Who started the turn of the main conversation that `entry` starts, if it starts one: `human`
/// for the operator's prompt or slash command, or such as `task_notification` or `scheduled` for
/// input Claude Code queued itself.
fn turn_origin(entry: &Value) -> Option<&str> {
    if flag(entry, "isSidechain") || entry.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    entry.get("turnOrigin").and_then(Value::as_str)
}

/// Whether `entry` is a line of the main conversation that can show what queued input started.
/// Claude Code writes how long a turn took after its end, and that a scheduled task fired before
/// the scheduled prompt's turn, so neither shows it.
fn is_conversation(entry: &Value) -> bool {
    !flag(entry, "isSidechain")
        && match entry.get("type").and_then(Value::as_str) {
            Some("user" | "assistant") => true,
            Some("system") => !matches!(
                entry.get("subtype").and_then(Value::as_str),
                Some("turn_duration" | "scheduled_task_fire")
            ),
            _ => false,
        }
}

/// Whether `entry` ends a turn of the main conversation, and if so whether the operator ended it.
/// Claude Code writes `stop_hook_summary` after the `Stop` hooks, which agentctl always registers,
/// an API error message when the turn fails, and the interruption marker when the operator ends it.
fn ends_turn(entry: &Value) -> Option<bool> {
    if flag(entry, "isSidechain") {
        return None;
    }
    match entry.get("type").and_then(Value::as_str) {
        Some("system") => (entry.get("subtype").and_then(Value::as_str) == Some("stop_hook_summary")).then_some(false),
        Some("assistant") => flag(entry, "isApiErrorMessage").then_some(false),
        Some("user") => is_interruption(entry).then_some(true),
        _ => None,
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

    /// Turns recorded with Claude Code 2.1.321, separated by a blank line: a prompt queued behind
    /// an answer being written, one absorbed while a tool ran, and one queued behind an answer the
    /// operator then interrupted. Each turn is followed by the one its input started, if any.
    const QUEUED: &str = include_str!("../../../tests/fixtures/claude-code-queued.jsonl");

    #[test]
    fn a_prompt_queued_behind_the_running_turn_starts_a_turn_of_its_own() {
        let sections = QUEUED.split("\n\n").collect::<Vec<_>>();
        let [queued, absorbed, queued_then_interrupted] = sections[..] else {
            panic!("three recorded sections");
        };
        // What the signals are once each line is written, in the order the file holds them. Each
        // section starts with the turn the prompt is queued behind, as when its line is written
        // after the mark.
        for (case, section, prompt, expected) in [
            // The prompt is queued while the essay is written. The essay's turn ends, and the
            // queued prompt's turn starts and ends.
            (
                "queued behind the essay",
                queued,
                "Reply with exactly SECOND.",
                "-qqqqqqq--",
            ),
            (
                "absorbed during a tool call",
                absorbed,
                "Also tell me how many lines it printed.",
                "--q------",
            ),
            // The operator interrupts the essay, and the queued prompt's turn follows.
            (
                "queued behind an interrupted essay",
                queued_then_interrupted,
                "Reply with exactly THIRD.",
                "-qqqIII--",
            ),
        ] {
            let mut tracker = TurnTracker::new(prompt);
            let seen = section
                .lines()
                .map(|line| {
                    tracker.push(format!("{line}\n").as_bytes());
                    match tracker.signals() {
                        TurnSignals {
                            interrupted: false,
                            queued: false,
                        } => '-',
                        TurnSignals {
                            interrupted: false,
                            queued: true,
                        } => 'q',
                        TurnSignals {
                            interrupted: true,
                            queued: true,
                        } => 'I',
                        TurnSignals {
                            interrupted: true,
                            queued: false,
                        } => 'i',
                    }
                })
                .collect::<String>();
            assert_eq!(seen, expected, "{case}");
        }
    }

    #[test]
    fn input_claude_code_queues_itself_starts_no_turn_to_follow() {
        // Claude Code 2.1.321 queues the notification that a background task finished, and takes
        // it into a turn of its own.
        let notification =
            "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>";
        let turn = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": notification}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            serde_json::json!({"type": "user", "promptSource": "system", "turnOrigin": "task_notification", "message": {"role": "user", "content": notification}}),
        ]
        .map(|entry| format!("{entry}\n"))
        .concat();
        assert_eq!(signals(&turn), TurnSignals::default());

        // A notification taken into the running turn does not take the operator's input with it.
        let queued = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": notification}),
            serde_json::json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn", "content": notification}),
        ]
        .map(|entry| format!("{entry}\n"))
        .concat();
        assert!(signals(&queued).queued);

        // Nor does a notification's turn that runs first. Claude Code 2.1.321 takes its own input
        // and the operator's in the order they were queued.
        let ended = serde_json::json!({"type": "system", "subtype": "stop_hook_summary"});
        let took = serde_json::json!({"type": "system", "subtype": "turn_duration"});
        let dequeued = serde_json::json!({"type": "queue-operation", "operation": "dequeue"});
        let answer = |text: &str| serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}});
        let lines = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": notification}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly THIRD."}),
            dequeued.clone(),
            answer("Volcanoes are openings in the crust."),
            ended.clone(),
            took.clone(),
            serde_json::json!({"type": "user", "promptSource": "system", "turnOrigin": "task_notification", "message": {"role": "user", "content": notification}}),
            dequeued,
            answer("The background `sleep 3` finished."),
            ended.clone(),
            took,
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly THIRD."}}),
            answer("THIRD"),
            ended,
        ];
        let mut tracker = TurnTracker::default();
        let seen = lines
            .iter()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        assert_eq!(seen, "-qqqqqqqqqqqq-");
    }

    /// The transcript lines that hold `entries`.
    fn jsonl(entries: &[Value]) -> String {
        entries
            .iter()
            .map(|entry| format!("{entry}\n"))
            .collect::<Vec<_>>()
            .concat()
    }

    #[test]
    fn the_turn_queued_input_starts_ends_as_any_turn_does() {
        let running = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "/hello"}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
        ];
        let ended = serde_json::json!({"type": "system", "subtype": "stop_hook_summary"});
        let queued = jsonl(&[running[0].clone(), running[1].clone(), ended.clone()]);
        // Claude Code 2.1.321 starts the turn of a queued prompt with the prompt, and the turn of a
        // queued slash command with the command's echo, which has no `promptSource`.
        let prompt = serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}});
        let command = serde_json::json!({"type": "user", "turnOrigin": "human", "message": {"role": "user", "content": "<command-message>hello</command-message>\n<command-name>/hello</command-name>"}});
        for start in [prompt.clone(), command] {
            let started = format!("{queued}{}", jsonl(std::slice::from_ref(&start)));
            assert!(signals(&started).queued, "{start}");
            for (end, expected) in [
                (ended.clone(), TurnSignals::default()),
                (
                    serde_json::json!({"type": "assistant", "isApiErrorMessage": true, "message": {"role": "assistant", "content": [{"type": "text", "text": "API Error: 529"}]}}),
                    TurnSignals::default(),
                ),
                (
                    serde_json::json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
                    TurnSignals {
                        interrupted: true,
                        queued: false,
                    },
                ),
            ] {
                let lines = format!("{started}{}", jsonl(std::slice::from_ref(&end)));
                assert_eq!(signals(&lines), expected, "{start} {end}");
            }
        }

        // A subagent's lines neither start nor end the main conversation's turns.
        let subagent_started = serde_json::json!({"type": "user", "isSidechain": true, "turnOrigin": "human", "message": {"role": "user", "content": "Explore the repository."}});
        let subagent_ended = serde_json::json!({"type": "system", "isSidechain": true, "subtype": "stop_hook_summary"});
        let lines = jsonl(&[
            running[0].clone(),
            subagent_started.clone(),
            subagent_ended.clone(),
            ended,
        ]);
        assert!(signals(&lines).queued, "a subagent's turn in the running turn");
        let lines = format!("{queued}{}", jsonl(&[prompt, subagent_started, subagent_ended]));
        assert!(signals(&lines).queued, "a subagent's turn in the queued turn");
    }

    #[test]
    fn queued_input_that_starts_no_turn_is_not_followed() {
        // Recorded with Claude Code 2.1.321: a slash command it doesn't know, queued behind an
        // essay. It is taken from the queue, and Claude Code warns about it instead of starting a
        // turn.
        let enqueued = serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "/otters"});
        let dequeued = serde_json::json!({"type": "queue-operation", "operation": "dequeue"});
        let essay = serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Ocean tides are the regular rising and falling of sea levels."}]}});
        let ended = serde_json::json!({"type": "system", "subtype": "stop_hook_summary"});
        let took = serde_json::json!({"type": "system", "subtype": "turn_duration", "durationMs": 5102});
        let warned = serde_json::json!({"type": "system", "subtype": "informational", "content": "Unknown command: /otters", "level": "warning"});
        let prompt = serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}});

        let lines = [&enqueued, &dequeued, &essay, &ended, &took];
        for count in 1..=lines.len() {
            let lines = lines[..count].iter().copied().cloned().collect::<Vec<_>>();
            assert!(
                signals(&jsonl(&lines)).queued,
                "until the essay's turn has ended: {lines:?}"
            );
        }
        let lines = [&enqueued, &dequeued, &essay, &ended, &took, &warned].map(Clone::clone);
        assert_eq!(signals(&jsonl(&lines)), TurnSignals::default(), "the warning");

        // Input that arrives as the turn ends is queued and taken below its end.
        let lines = [&essay, &ended, &took, &enqueued, &dequeued].map(Clone::clone);
        assert!(signals(&jsonl(&lines)).queued, "taken after the end");
        let lines = [&essay, &ended, &took, &enqueued, &dequeued, &warned].map(Clone::clone);
        assert_eq!(
            signals(&jsonl(&lines)),
            TurnSignals::default(),
            "taken after the end, then the warning"
        );
        let lines = [&essay, &ended, &took, &enqueued, &dequeued, &prompt].map(Clone::clone);
        assert!(signals(&jsonl(&lines)).queued, "taken after the end, then its turn");
    }

    #[test]
    fn input_sent_after_the_prompts_turn_started_is_not_followed() {
        // The waited prompt reaches an idle Claude Code and starts its turn at once. Another prompt,
        // such as one sent without --wait or typed into the attached terminal, is queued behind it.
        // The waited prompt's turn ends where its own answer ends, not where the later one's does.
        let lines = jsonl(&[
            serde_json::json!({"type": "user", "promptSource": "typed", "turnOrigin": "human", "message": {"role": "user", "content": "Write a 300-word essay about ocean tides. Use no tools."}}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Ocean tides are the regular rising and falling of sea levels."}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
            serde_json::json!({"type": "system", "subtype": "turn_duration"}),
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}}),
        ]);
        let mut tracker = TurnTracker::new("Write a 300-word essay about ocean tides. Use no tools.");
        let seen = lines
            .lines()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        assert_eq!(seen, "-------");
    }

    #[test]
    fn a_prompt_queued_behind_a_turn_started_just_before_the_mark_is_followed() {
        // A prompt sent without --wait reaches an idle Claude Code just before the waited one.
        // `prompt` returns once it is typed, before Claude Code has run the UserPromptSubmit hooks
        // and written the prompt, so its line comes after the waited prompt's mark. The waited
        // prompt is pasted while that turn runs and is queued behind it.
        let lines = jsonl(&[
            serde_json::json!({"type": "user", "promptSource": "typed", "turnOrigin": "human", "message": {"role": "user", "content": "Write a 300-word essay about ocean tides. Use no tools."}}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Ocean tides are the regular rising and falling of sea levels."}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
            serde_json::json!({"type": "system", "subtype": "turn_duration"}),
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "SECOND"}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
        ]);
        let mut tracker = TurnTracker::new("Reply with exactly SECOND.");
        let seen = lines
            .lines()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        // The waited prompt's own turn is followed to its end.
        assert_eq!(seen, "-qqqqqqq-");
    }

    #[test]
    fn a_prompt_queued_before_the_turn_ahead_of_it_is_written_is_followed() {
        // A prompt sent without --wait reaches an idle Claude Code just before the waited one, and
        // Claude Code is still running its UserPromptSubmit hooks when the waited prompt is pasted.
        // The waited prompt is queued, and its `enqueue` is written at once, above the line of the
        // turn ahead of it. That line was typed, not taken from the queue.
        let lines = jsonl(&[
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
            serde_json::json!({"type": "user", "promptSource": "typed", "turnOrigin": "human", "message": {"role": "user", "content": "Write a 300-word essay about ocean tides. Use no tools."}}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Ocean tides are the regular rising and falling of sea levels."}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
            serde_json::json!({"type": "system", "subtype": "turn_duration"}),
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "SECOND"}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
        ]);
        let mut tracker = TurnTracker::new("Reply with exactly SECOND.");
        let seen = lines
            .lines()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        // The waited prompt's own turn is followed to its end.
        assert_eq!(seen, "qqqqqqqq-");
    }

    #[test]
    fn input_sent_after_a_pasted_multiline_prompts_turn_started_is_not_followed() {
        // Recorded with Claude Code 2.1.321: a prompt of several lines, pasted with bracketed paste
        // as agentctl delivers it, is written wrapped in `<pasted_content>`.
        let prompt = "Reply with exactly OK.\nLine two\n\nLine four";
        let lines = jsonl(&[
            serde_json::json!({"type": "user", "promptSource": "typed", "turnOrigin": "human", "message": {"role": "user", "content": "\n\n<pasted_content id=\"344d\">\nReply with exactly OK.\nLine two\n\nLine four\n</pasted_content id=\"344d\">\n"}}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
            serde_json::json!({"type": "queue-operation", "operation": "dequeue"}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "OK"}]}}),
            serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}),
            serde_json::json!({"type": "system", "subtype": "turn_duration"}),
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly SECOND."}}),
        ]);
        let mut tracker = TurnTracker::new(prompt);
        let seen = lines
            .lines()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        assert_eq!(seen, "-------");
    }

    #[test]
    fn input_sent_after_the_prompt_was_absorbed_is_not_followed() {
        // The waited prompt is absorbed into the running turn while a tool runs, and another prompt
        // is queued behind that turn.
        let lines = jsonl(&[
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Also tell me how many lines it printed."}),
            serde_json::json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn", "content": "Also tell me how many lines it printed."}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
        ]);
        assert_eq!(signals(&lines), TurnSignals::default());
    }

    #[test]
    fn a_scheduled_prompts_turn_does_not_take_the_operators_input() {
        // A scheduled task fired while the essay was written, before the operator's prompt was
        // queued, and Claude Code 2.1.321 takes it first. It writes that the task fired, then
        // starts the scheduled prompt's turn.
        let ended = serde_json::json!({"type": "system", "subtype": "stop_hook_summary"});
        let took = serde_json::json!({"type": "system", "subtype": "turn_duration"});
        let dequeued = serde_json::json!({"type": "queue-operation", "operation": "dequeue"});
        let answer = |text: &str| serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}});
        let lines = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly THIRD."}),
            dequeued.clone(),
            answer("Volcanoes are openings in the crust."),
            ended.clone(),
            took.clone(),
            serde_json::json!({"type": "system", "subtype": "scheduled_task_fire", "content": "Running scheduled task"}),
            serde_json::json!({"type": "user", "turnOrigin": "scheduled", "message": {"role": "user", "content": "Check the build."}}),
            dequeued,
            answer("The build passes."),
            ended.clone(),
            took,
            serde_json::json!({"type": "user", "promptSource": "queued", "turnOrigin": "human", "message": {"role": "user", "content": "Reply with exactly THIRD."}}),
            answer("THIRD"),
            ended,
        ];
        let mut tracker = TurnTracker::default();
        let seen = lines
            .iter()
            .map(|line| {
                tracker.push(format!("{line}\n").as_bytes());
                if tracker.signals().queued { 'q' } else { '-' }
            })
            .collect::<String>();
        assert_eq!(seen, "qqqqqqqqqqqqq-");
    }

    #[test]
    fn an_enqueue_without_its_text_is_not_followed() {
        // Without the text the tracker can't tell the operator's input from Claude Code's own, and
        // following Claude Code's could hold the wait until its timeout.
        let lines = jsonl(&[serde_json::json!({"type": "queue-operation", "operation": "enqueue"})]);
        assert_eq!(signals(&lines), TurnSignals::default());
    }

    #[test]
    fn a_prompt_is_known_in_the_forms_claude_code_writes_it() {
        // Recorded with Claude Code 2.1.321, pasted as agentctl delivers prompts.
        for (sent, written) in [
            ("Reply with exactly OK.", "Reply with exactly OK."),
            ("a\tb", "a    b"),
            ("a\r\nb", "a\n\nb"),
            (
                "Reply with exactly OK.\nLine two",
                "\n\n<pasted_content id=\"344d\">\nReply with exactly OK.\nLine two\n</pasted_content id=\"344d\">\n",
            ),
            (
                "/review src",
                "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>src</command-args>",
            ),
        ] {
            assert_eq!(prompt_key(written), prompt_key(sent), "{written:?}");
        }
        assert_ne!(
            prompt_key("Reply with exactly SECOND."),
            prompt_key("Write a 300-word essay about ocean tides.")
        );
    }

    #[test]
    fn queued_input_taken_back_starts_no_turn() {
        // Claude Code 2.1.321 moves queued input back to the prompt for editing, all of it or the
        // latest.
        for operation in ["popAll", "popOne"] {
            let lines = [
                serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "Reply with exactly SECOND."}),
                serde_json::json!({"type": "queue-operation", "operation": operation, "content": "Reply with exactly SECOND."}),
            ]
            .map(|entry| format!("{entry}\n"))
            .concat();
            assert_eq!(signals(&lines), TurnSignals::default(), "{operation}");
        }
    }

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
