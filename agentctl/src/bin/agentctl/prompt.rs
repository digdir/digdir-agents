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

/// Asks a yes-or-no question on standard error. There is no default: the answer must be `y`,
/// `yes`, `n` or `no`, so pressing Enter by reflex accepts nothing.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn confirm(question: &str) -> Result<bool, Error> {
    confirm_lines(&[question])
}

/// Like [`confirm`], for a question of several lines: what it is about first, the question last.
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

/// The answer `y`, `yes`, `n` or `no` gives, in any case; anything else is no answer.
fn yes_or_no(answer: &str) -> Option<bool> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// Opens `files` in `$VISUAL` or `$EDITOR` and waits for it, or, without one, asks for them to be
/// edited elsewhere and waits for Enter.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn edit_files(files: &[&std::path::Path]) -> Result<(), Error> {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()));
    if let Some(editor) = editor {
        let mut words = editor.split_whitespace();
        let program = words.next().unwrap_or_default();
        let status = std::process::Command::new(program).args(words).args(files).status();
        if !status.is_ok_and(|status| status.success()) {
            eprintln!("The editor `{editor}` did not finish cleanly.");
        }
        return Ok(());
    }
    let names: Vec<String> = files.iter().map(|file| file.display().to_string()).collect();
    wait_for_enter(&format!(
        "Edit {} in your editor and save, then press Enter…",
        names.join(" and ")
    ))
}

/// Shows `message` after a blank line and waits for Enter; Ctrl-C ends the command as usual.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn wait_for_enter(message: &str) -> Result<(), Error> {
    eprint!("\n{message}");
    std::io::stderr().flush().ok();
    read_answer().map(drop)
}

/// Asks for one line of text, returned trimmed.
///
/// # Errors
///
/// Returns an error when standard input cannot be read.
pub(crate) fn ask(question: &str) -> Result<String, Error> {
    ask_lines(&[question])
}

/// Like [`ask`], for a question of several lines: the question first, then what it is about,
/// and last the label of the answer.
///
/// # Errors
///
/// Returns an error when standard input cannot be read.
pub(crate) fn ask_lines(lines: &[&str]) -> Result<String, Error> {
    pose(lines, ": ");
    read_answer()
}

/// Asks `question` with numbered `options`, returning the chosen index. Pressing Enter picks
/// `default`, or cancels with `None` when there is none.
///
/// # Errors
///
/// Returns an error when standard input cannot be read.
pub(crate) fn choose(question: &str, options: &[String], default: Option<usize>) -> Result<Option<usize>, Error> {
    choose_lines(&[question], options, default)
}

/// Like [`choose`], for a question of several lines: the question first, then what it is about.
///
/// # Errors
///
/// Returns an error when standard input cannot be read.
pub(crate) fn choose_lines(lines: &[&str], options: &[String], default: Option<usize>) -> Result<Option<usize>, Error> {
    pose(lines, "\n");
    for (number, option) in options.iter().enumerate() {
        eprintln!("  {}. {option}", number + 1);
    }
    let hint = default.map_or_else(
        || format!("Choose 1-{}, or press Enter to cancel: ", options.len()),
        |default| format!("Choose 1-{} [{}]: ", options.len(), default + 1),
    );
    loop {
        eprint!("{hint}");
        std::io::stderr().flush().ok();
        let answer = read_answer()?;
        if answer.is_empty() {
            return Ok(default);
        }
        if let Some(index) = numbered(&answer, options.len()) {
            return Ok(Some(index));
        }
        eprintln!("Type a number from 1 to {}.", options.len());
    }
}

/// The option `answer` picks among `count`, numbered from 1.
fn numbered(answer: &str, count: usize) -> Option<usize> {
    answer
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|number| (1..=count).contains(number))
        .map(|number| number - 1)
}

/// Reads a secret typed or pasted on the terminal without echoing it.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read, the user presses Ctrl-C, or nothing was entered.
pub(crate) fn secret(label: &str) -> Result<Zeroizing<String>, Error> {
    pose(&[label], ": ");
    crossterm::terminal::enable_raw_mode()?;
    let read = read_secret();
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    let value = read?;
    if value.trim().is_empty() {
        return Err(Error::Invalid("nothing was entered".into()));
    }
    Ok(value)
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
        if kind == KeyEventKind::Release {
            continue;
        }
        match code {
            KeyCode::Enter => return Ok(value),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(Error::Invalid("cancelled".into()));
            }
            KeyCode::Char(character) => value.push(character),
            KeyCode::Backspace => {
                value.pop();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{numbered, yes_or_no};

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

    #[test]
    fn a_choice_is_a_listed_number() {
        assert_eq!(numbered("1", 3), Some(0));
        assert_eq!(numbered(" 3 ", 3), Some(2));
        for answer in ["0", "4", "digdir", "-1"] {
            assert_eq!(numbered(answer, 3), None, "{answer:?}");
        }
    }
}
