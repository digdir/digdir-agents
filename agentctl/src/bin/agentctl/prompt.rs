//! Line prompts on the controlling terminal, outside the TUI.

use std::io::{IsTerminal as _, Write as _};
use std::time::Duration;

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

/// The index a single key picks among `count` options: a digit numbering one, or Enter for the first.
fn option_key(code: KeyCode, count: usize) -> Option<usize> {
    match code {
        KeyCode::Enter => option_number("", count),
        KeyCode::Char(digit @ '0'..='9') => option_number(digit.encode_utf8(&mut [0; 4]), count),
        _ => None,
    }
}

/// Whether a menu of `count` options takes a single key; from ten on, a `1` could start `12`.
fn single_key_menu(count: usize) -> bool {
    (1..=9).contains(&count)
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

/// Says what happens next on standard error, laid out like a question, and waits for Enter to
/// `action`.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends, or Ctrl-C cancels.
pub(crate) fn proceed(lines: &[&str], action: &str) -> Result<(), Error> {
    pose(lines, &format!("\n  Press Enter to {action}, or Ctrl-C to cancel. "));
    if std::io::stdin().is_terminal() {
        return read_key(enter_key, |()| String::new());
    }
    read_answer().map(drop)
}

/// The answer a single key gives to a request to press Enter: Enter only.
fn enter_key(code: KeyCode) -> Option<()> {
    (code == KeyCode::Enter).then_some(())
}

/// Asks for a line of text on standard error, what it is about first and the label last; Enter
/// alone takes `default`, shown in brackets, when there is one.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends.
pub(crate) fn ask(lines: &[&str], default: Option<&str>) -> Result<String, Error> {
    pose(
        lines,
        &default.map_or_else(|| ": ".to_owned(), |default| format!(" [{default}]: ")),
    );
    let answer = read_answer()?;
    Ok(match default {
        Some(default) if answer.is_empty() => default.to_owned(),
        _ => answer,
    })
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
/// Pressing Enter picks the first. At a terminal, with up to nine options, a single digit answers.
///
/// # Errors
///
/// Returns an error when standard input cannot be read or ends, or Ctrl-C cancels.
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
    let count = options.len();
    if single_key_menu(count) && std::io::stdin().is_terminal() {
        return read_key(|code| option_key(code, count), |index| (index + 1).to_string());
    }
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
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::{Pressed, enter_key, option_key, option_number, pressed, single_key_menu, yes_or_no, yes_or_no_key};

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
    fn only_enter_proceeds() {
        assert_eq!(pressed(&key(KeyCode::Enter), enter_key), Pressed::Answer(()));
        for code in [KeyCode::Char('y'), KeyCode::Char(' '), KeyCode::Esc] {
            assert_eq!(pressed(&key(code), enter_key), Pressed::Ignore, "{code:?}");
        }
        assert_eq!(pressed(&control('c'), enter_key), Pressed::Cancel);
    }

    #[test]
    fn a_single_digit_picks_an_option_and_enter_the_first() {
        let three = |code| option_key(code, 3);
        assert_eq!(pressed(&key(KeyCode::Enter), three), Pressed::Answer(0));
        assert_eq!(pressed(&key(KeyCode::Char('1')), three), Pressed::Answer(0));
        assert_eq!(pressed(&key(KeyCode::Char('3')), three), Pressed::Answer(2));
        for code in [
            KeyCode::Char('0'),
            KeyCode::Char('4'),
            KeyCode::Char(' '),
            KeyCode::Char('y'),
        ] {
            assert_eq!(pressed(&key(code), three), Pressed::Ignore, "{code:?}");
        }
    }

    #[test]
    fn releases_and_held_modifiers_answer_nothing_and_ctrl_c_and_ctrl_d_end_the_question() {
        let mut release = key(KeyCode::Char('y'));
        release.kind = KeyEventKind::Release;
        assert_eq!(pressed(&release, yes_or_no_key), Pressed::Ignore);
        release.code = KeyCode::Enter;
        assert_eq!(pressed(&release, |code| option_key(code, 2)), Pressed::Ignore);
        let alt = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT);
        assert_eq!(pressed(&alt, yes_or_no_key), Pressed::Ignore);
        assert_eq!(pressed(&control('y'), yes_or_no_key), Pressed::Ignore);
        assert_eq!(pressed(&control('c'), yes_or_no_key), Pressed::Cancel);
        assert_eq!(pressed(&control('d'), |code| option_key(code, 2)), Pressed::Ended);
    }

    #[test]
    fn menus_of_ten_or_more_options_are_answered_by_line() {
        assert!(!single_key_menu(0));
        assert!(single_key_menu(1));
        assert!(single_key_menu(9));
        assert!(!single_key_menu(10));
    }

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
