//! Line prompts on the controlling terminal, outside the TUI.

use std::io::{IsTerminal as _, Write as _};

use agent::Error;

/// Whether a person is at the terminal: questions go to standard error, and the TUI some answers
/// open draws on standard output, so all three must be a terminal.
pub(crate) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

/// Starts a question after a blank line, behind a `?` marker that is bold and cyan on a terminal
/// unless `NO_COLOR` is set, so questions stand apart from what is reported. Any further `lines`,
/// such as what the question is about, follow indented under it, and `suffix` ends the last line.
fn pose(lines: &[&str], suffix: &str) {
    let marker = if colored() { "\x1b[1;36m?\x1b[0m" } else { "?" };
    let mut text = String::from("\n");
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            text.push_str(marker);
            text.push(' ');
        } else {
            text.push_str("\n  ");
        }
        text.push_str(line);
    }
    eprint!("{text}{suffix}");
    std::io::stderr().flush().ok();
}

fn read_answer() -> Result<String, Error> {
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Err(Error::Invalid("no answer: standard input ended".into()));
    }
    Ok(answer.trim().to_owned())
}

/// Asks a yes-or-no question on standard error, what it is about first and the question last.
/// There is no default: the answer must be `y`, `yes`, `n` or `no`, so pressing Enter by reflex
/// accepts nothing.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn confirm_lines(lines: &[&str]) -> Result<bool, Error> {
    pose(lines, " (y/n) ");
    loop {
        if let Some(answer) = yes_or_no(&read_answer()?) {
            return Ok(answer);
        }
        eprint!("Type y or n: ");
        std::io::stderr().flush().ok();
    }
}

/// What a person answered, that they asked to go back to the question before, or that they
/// cancelled with Ctrl-C or Ctrl-D.
pub(crate) enum Reply<T> {
    Answer(T),
    Back,
    Cancel,
}

/// What an `inquire` prompt's result means here, with the line it left behind redrawn as
/// [`settle`] does. `shown` is how the answer reads. Esc goes back when `back` allows it, and is
/// otherwise asked again, as `None`.
fn reply<T>(
    question: &str,
    result: Result<T, inquire::InquireError>,
    shown: impl FnOnce(&T) -> String,
    back: bool,
) -> Result<Option<Reply<T>>, Error> {
    match result {
        Ok(answer) => {
            let text = shown(&answer);
            settle(
                &format!("✔ {question} {text}"),
                Some(("\x1b[32m✔\x1b[0m", question, &text)),
            );
            Ok(Some(Reply::Answer(answer)))
        }
        Err(inquire::InquireError::OperationCanceled) => {
            let left = format!("? {question} {CANCELED}");
            if back {
                settle(&left, Some(("\x1b[2m↩\x1b[0m", question, "went back")));
                Ok(Some(Reply::Back))
            } else {
                settle(&left, None);
                Ok(None)
            }
        }
        Err(inquire::InquireError::OperationInterrupted) => Ok(Some(Reply::Cancel)),
        Err(error) => Err(Error::Invalid(error.to_string())),
    }
}

/// What `inquire` writes after a question left with Esc.
const CANCELED: &str = "<canceled>";

/// Redraws the line `left`, the one `inquire` leaves for a finished question, as one that steps
/// back from the question asked next: its mark, such as a check mark for an answer, the question
/// dimmed and what became of it in colour. `None` removes the line. Without colour, `inquire`'s own
/// line stays.
fn settle(left: &str, finished: Option<(&str, &str, &str)>) {
    if !colored() {
        return;
    }
    let columns = crossterm::terminal::size()
        .ok()
        .filter(|(columns, _)| *columns > 0)
        .map_or(80, |(columns, _)| usize::from(columns));
    let rows = left.chars().count().max(1).div_ceil(columns);
    let finished = finished.map_or_else(String::new, |(mark, question, outcome)| {
        format!("{mark} \x1b[2m{question}\x1b[0m \x1b[36m{outcome}\x1b[0m\n")
    });
    eprint!("\x1b[{rows}A\r\x1b[J{finished}");
    std::io::stderr().flush().ok();
}

/// Whether questions are drawn in colour: on a terminal, unless `NO_COLOR` asks for none.
fn colored() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

/// Starts a question after a blank line, which sets it apart from the answers above it, and
/// shows the lines it is about, all but its last, which the prompt shows itself.
fn context(lines: &[&str]) -> String {
    eprintln!();
    let Some((question, about)) = lines.split_last() else {
        return String::new();
    };
    for line in about {
        eprintln!("  {line}");
    }
    (*question).to_owned()
}

/// The look of every question: the one being asked stands out, with its question bold and the
/// option it is on marked in colour, while answered ones show a check mark and the answer. Without
/// colour when `NO_COLOR` asks for none.
fn render_config() -> inquire::ui::RenderConfig<'static> {
    use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};

    if !colored() {
        return RenderConfig::empty()
            .with_answered_prompt_prefix(Styled::new("✔"))
            .with_highlighted_option_prefix(Styled::new("❯"));
    }
    let mut config = RenderConfig::default_colored()
        .with_prompt_prefix(Styled::new("?").with_fg(Color::LightCyan).with_attr(Attributes::BOLD))
        .with_answered_prompt_prefix(Styled::new("✔").with_fg(Color::LightGreen))
        .with_highlighted_option_prefix(Styled::new("❯").with_fg(Color::LightCyan))
        .with_selected_option(Some(
            StyleSheet::new().with_fg(Color::LightCyan).with_attr(Attributes::BOLD),
        ))
        .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
        .with_answer(StyleSheet::new().with_fg(Color::LightCyan));
    config.prompt = StyleSheet::new().with_attr(Attributes::BOLD);
    config
}

/// `label` cut to fit a row of the terminal beside an option's prefix, ending in `…` when cut, so a
/// long option never wraps over rows mid-word.
fn fitted(label: &str) -> String {
    let columns = crossterm::terminal::size()
        .ok()
        .filter(|(columns, _)| *columns > 0)
        .map_or(80, |(columns, _)| usize::from(columns));
    let room = columns.saturating_sub(4).max(20);
    if label.chars().count() <= room {
        return label.to_owned();
    }
    let mut cut: String = label.chars().take(room - 1).collect();
    cut.push('…');
    cut
}

const fn hint(back: bool) -> &'static str {
    if back {
        "↑/↓ to choose, Enter to confirm, Esc to go back"
    } else {
        "↑/↓ to choose, Enter to confirm"
    }
}

/// Asks to choose one of `options`, with the arrow keys and Enter, starting at `default`. An
/// option's lines after its first, such as a description, follow it on one line. Esc goes back
/// when `back` allows it.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read.
pub(crate) fn menu(question: &str, options: &[String], default: usize, back: bool) -> Result<Reply<usize>, Error> {
    let lines: Vec<&str> = question.lines().collect();
    let question = context(&lines);
    let labels: Vec<String> = options
        .iter()
        .map(|option| fitted(&option.lines().collect::<Vec<_>>().join(" — ")))
        .collect();
    // An answered menu shows the option's first line only, such as a standard agent's title
    // without its description.
    let answered = |chosen: inquire::list_option::ListOption<&String>| {
        options[chosen.index].lines().next().unwrap_or_default().to_owned()
    };
    loop {
        let result = inquire::Select::new(&question, labels.clone())
            .with_starting_cursor(default.min(labels.len().saturating_sub(1)))
            .with_help_message(hint(back))
            .with_formatter(&answered)
            .with_render_config(render_config())
            .raw_prompt()
            .map(|chosen| chosen.index);
        let shown = |index: &usize| options[*index].lines().next().unwrap_or_default().to_owned();
        if let Some(reply) = reply(&question, result, shown, back)? {
            return Ok(reply);
        }
    }
}

/// Asks for a line of text. Enter alone gives `default` when there is one, and an empty answer
/// otherwise. Esc goes back when `back` allows it.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read.
pub(crate) fn line(lines: &[&str], default: Option<&str>, back: bool) -> Result<Reply<String>, Error> {
    let question = context(lines);
    let help = if back { "Esc to go back" } else { "" };
    loop {
        let mut prompt = inquire::Text::new(&question).with_render_config(render_config());
        if let Some(default) = default.filter(|default| !default.is_empty()) {
            prompt = prompt.with_default(default);
        }
        if back {
            prompt = prompt.with_help_message(help);
        }
        let result = prompt.prompt().map(|answer| answer.trim().to_owned());
        if let Some(reply) = reply(&question, result, String::clone, back)? {
            return Ok(reply);
        }
    }
}

/// Asks a yes-or-no question, what it is about first and the question last. There is no default,
/// so pressing Enter by reflex accepts nothing. Esc goes back when `back` allows it.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read.
pub(crate) fn yes_no(lines: &[&str], back: bool) -> Result<Reply<bool>, Error> {
    let question = context(lines);
    loop {
        let mut prompt = inquire::Confirm::new(&question).with_render_config(render_config());
        if back {
            prompt = prompt.with_help_message("Esc to go back");
        }
        let shown = |answer: &bool| if *answer { "Yes" } else { "No" }.to_owned();
        if let Some(reply) = reply(&question, prompt.prompt(), shown, back)? {
            return Ok(reply);
        }
    }
}

/// Asks for a secret, such as a token, without showing what is typed or pasted. A paste counts
/// whole, without its line breaks, so a token copied over several lines of a terminal arrives as
/// one; Enter after it ends the answer. Esc goes back when `back` allows it.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read.
pub(crate) fn secret(lines: &[&str], back: bool) -> Result<Reply<zeroize::Zeroizing<String>>, Error> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    pose(lines, " (hidden): ");
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stderr(), crossterm::event::EnableBracketedPaste).ok();
    let mut value = zeroize::Zeroizing::new(String::new());
    let reply = loop {
        let event = match crossterm::event::read() {
            Ok(event) => event,
            Err(error) => break Err(error.into()),
        };
        if let Event::Paste(pasted) = &event {
            value.extend(pasted.chars().filter(|character| !matches!(character, '\r' | '\n')));
            continue;
        }
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = event
        else {
            continue;
        };
        if kind == KeyEventKind::Release {
            continue;
        }
        match code {
            KeyCode::Enter => break Ok(Reply::Answer(std::mem::take(&mut value))),
            KeyCode::Esc if back => break Ok(Reply::Back),
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Char('c' | 'd') if modifiers.contains(KeyModifiers::CONTROL) => break Ok(Reply::Cancel),
            KeyCode::Char(character) => value.push(character),
            _ => {}
        }
    };
    crossterm::execute!(std::io::stderr(), crossterm::event::DisableBracketedPaste).ok();
    crossterm::terminal::disable_raw_mode().ok();
    eprintln!();
    reply
}

/// The answer `y`, `yes`, `n` or `no` gives, in any case; anything else is no answer.
fn yes_or_no(answer: &str) -> Option<bool> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::yes_or_no;

    #[test]
    fn yes_and_no_must_be_typed() {
        for answer in ["y", "Y", "yes", " YES "] {
            assert_eq!(yes_or_no(answer), Some(true), "{answer:?}");
        }
        for answer in ["n", "No", "no "] {
            assert_eq!(yes_or_no(answer), Some(false), "{answer:?}");
        }
        for answer in ["", "  ", "yep", "1", "q"] {
            assert_eq!(yes_or_no(answer), None, "{answer:?}");
        }
    }
}
