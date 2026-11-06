//! Line prompts on the controlling terminal, outside the TUI.

use std::io::{IsTerminal as _, Write as _};
use std::time::Duration;

use agent::Error;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Whether a person is at the terminal: questions go to standard error, and the TUI some answers
/// open draws on standard output, so all three must be a terminal.
pub(crate) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

/// Starts a question after a blank line, behind a `?` marker that is bold and cyan on a terminal
/// unless `NO_COLOR` is set, so questions stand apart from what is reported. Any further `lines`,
/// such as what the question is about, follow indented under it, and `suffix` ends the last line.
fn pose(lines: &[&str], suffix: &str) {
    let colored = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let marker = if colored { "\x1b[1;36m?\x1b[0m" } else { "?" };
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

/// What a key pressed at a single-key question does.
#[derive(Debug, PartialEq, Eq)]
enum Pressed<T> {
    Answer(T),
    Ignore,
    Cancel,
    Ended,
}

/// What `event` does at a question `answer` maps keys to answers for: Ctrl-C cancels, Ctrl-D ends
/// the input, releases and other keys held with Ctrl or Alt are ignored.
fn pressed<T>(event: &KeyEvent, answer: impl FnOnce(KeyCode) -> Option<T>) -> Pressed<T> {
    if event.kind == KeyEventKind::Release {
        return Pressed::Ignore;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('c') => Pressed::Cancel,
            KeyCode::Char('d') => Pressed::Ended,
            _ => Pressed::Ignore,
        };
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        return Pressed::Ignore;
    }
    answer(event.code).map_or(Pressed::Ignore, Pressed::Answer)
}

/// The answer a single key gives to a yes-or-no question: `y` or `n` in either case, never Enter.
fn yes_or_no_key(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char(character) => yes_or_no(character.encode_utf8(&mut [0; 4])),
        _ => None,
    }
}

/// Reads one key at the terminal that `answer` accepts, ignoring keys typed before the question,
/// and echoes what it answered as `shown` gives it, so the scrollback keeps the answer.
fn read_key<T>(answer: impl Fn(KeyCode) -> Option<T>, shown: impl FnOnce(&T) -> String) -> Result<T, Error> {
    crossterm::terminal::enable_raw_mode()?;
    let read = next_key(answer);
    let _ = crossterm::terminal::disable_raw_mode();
    match &read {
        Ok(value) => eprintln!("{}", shown(value)),
        Err(_) => eprintln!(),
    }
    read
}

fn next_key<T>(answer: impl Fn(KeyCode) -> Option<T>) -> Result<T, Error> {
    while crossterm::event::poll(Duration::ZERO)? {
        crossterm::event::read()?;
    }
    loop {
        let Event::Key(event) = crossterm::event::read()? else {
            continue;
        };
        match pressed(&event, &answer) {
            Pressed::Answer(value) => return Ok(value),
            Pressed::Ignore => {}
            Pressed::Cancel => return Err(Error::Invalid("cancelled".into())),
            Pressed::Ended => return Err(Error::Invalid("no answer: standard input ended".into())),
        }
    }
}

/// Asks a yes-or-no question on standard error, what it is about first and the question last.
/// At a terminal a single `y` or `n` answers; otherwise the answer is a line of `y`, `yes`, `n` or
/// `no`. There is no default, so pressing Enter by reflex accepts nothing.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends, or Ctrl-C cancels.
pub(crate) fn confirm_lines(lines: &[&str]) -> Result<bool, Error> {
    pose(lines, " (y/n) ");
    if std::io::stdin().is_terminal() {
        return read_key(yes_or_no_key, |&answer| if answer { "yes" } else { "no" }.to_owned());
    }
    loop {
        if let Some(answer) = yes_or_no(&read_answer()?) {
            return Ok(answer);
        }
        eprint!("Type y or n: ");
        std::io::stderr().flush().ok();
    }
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
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::{Pressed, pressed, yes_or_no, yes_or_no_key};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn control(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_single_y_or_n_answers_and_enter_does_not() {
        for (character, answer) in [('y', true), ('Y', true), ('n', false), ('N', false)] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::SHIFT);
            assert_eq!(pressed(&event, yes_or_no_key), Pressed::Answer(answer), "{character:?}");
            assert_eq!(
                pressed(&key(KeyCode::Char(character)), yes_or_no_key),
                Pressed::Answer(answer)
            );
        }
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('1'),
            KeyCode::Char('q'),
            KeyCode::Esc,
        ] {
            assert_eq!(pressed(&key(code), yes_or_no_key), Pressed::Ignore, "{code:?}");
        }
    }

    #[test]
    fn releases_and_held_modifiers_answer_nothing_and_ctrl_c_and_ctrl_d_end_the_question() {
        let mut release = key(KeyCode::Char('y'));
        release.kind = KeyEventKind::Release;
        assert_eq!(pressed(&release, yes_or_no_key), Pressed::Ignore);
        let alt = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT);
        assert_eq!(pressed(&alt, yes_or_no_key), Pressed::Ignore);
        assert_eq!(pressed(&control('y'), yes_or_no_key), Pressed::Ignore);
        assert_eq!(pressed(&control('c'), yes_or_no_key), Pressed::Cancel);
    }

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
