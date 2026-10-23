//! Line prompts on the controlling terminal, outside the TUI.

use std::io::{IsTerminal as _, Write as _};

use agent::Error;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use zeroize::Zeroizing;

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

/// Reads a secret typed or pasted at the terminal, without echoing it, until Enter.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read, or Ctrl-C cancels.
pub(crate) fn secret(label: &str) -> Result<Zeroizing<String>, Error> {
    pose(&[label], ": ");
    crossterm::terminal::enable_raw_mode()?;
    let read = read_secret();
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    read
}

fn read_secret() -> Result<Zeroizing<String>, Error> {
    let mut value = Zeroizing::new(String::new());
    loop {
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = crossterm::event::read()?
        else {
            continue;
        };
        match code {
            _ if kind == KeyEventKind::Release => {}
            KeyCode::Enter => return Ok(value),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(Error::Invalid("cancelled; nothing was stored".into()));
            }
            KeyCode::Char(character) => value.push(character),
            KeyCode::Backspace => {
                value.pop();
            }
            _ => {}
        }
    }
}

/// Asks which of `options` to use, numbered from 1, on standard error, and returns its index.
/// Pressing Enter picks the first.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn choose(question: &str, options: &[&str]) -> Result<usize, Error> {
    let mut lines = vec![question.to_owned()];
    lines.extend(
        options
            .iter()
            .enumerate()
            .map(|(index, option)| format!("{}. {option}", index + 1)),
    );
    pose(
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        "\n  Number [1]: ",
    );
    loop {
        if let Some(index) = option_number(&read_answer()?, options.len()) {
            return Ok(index);
        }
        eprint!("Type a number from 1 to {}: ", options.len());
        std::io::stderr().flush().ok();
    }
}

/// The index of the option `answer` numbers among `count`, the first for no answer.
fn option_number(answer: &str, count: usize) -> Option<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return (count > 0).then_some(0);
    }
    answer
        .parse::<usize>()
        .ok()
        .filter(|number| (1..=count).contains(number))
        .map(|number| number - 1)
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
    use super::{option_number, yes_or_no};

    #[test]
    fn an_option_is_chosen_by_its_number_or_enter_for_the_first() {
        assert_eq!(option_number("", 2), Some(0));
        assert_eq!(option_number(" 2 ", 2), Some(1));
        for answer in ["0", "3", "-1", "one", "1.0"] {
            assert_eq!(option_number(answer, 2), None, "{answer:?}");
        }
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
