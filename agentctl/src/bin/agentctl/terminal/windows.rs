//! This terminal's input on Windows, as crossterm reports it, turned into the bytes a terminal sends.
//!
//! The console can send those bytes itself, in its virtual terminal input mode, which would pass mouse reports and
//! keys with modifiers too. Until that is built and tried on a real console, keys go through crossterm, which covers
//! typing, pasting and resizing.

use agent::Error;
use bytes::Bytes;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use sandbox::terminal::TerminalSize;

use super::Local;

/// This console's output interpreting the escape sequences a program draws with, until dropped.
///
/// crossterm's raw mode changes only the console's input, and a console that does not interpret them shows them as
/// text instead.
pub(super) struct OutputMode {
    /// The mode to put back, when this changed it.
    restore: Option<u32>,
}

impl OutputMode {
    #[expect(unsafe_code, reason = "the console's mode is only reachable through the Windows API")]
    pub(super) fn enable() -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode,
        };

        // SAFETY: Both take this process's own output handle, and `mode` outlives the call that writes it.
        unsafe {
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0;
            // Output that is not a console has no mode to change.
            let restore = (GetConsoleMode(output, &raw mut mode) != 0
                && mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0
                && SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0)
                .then_some(mode);
            Self { restore }
        }
    }
}

impl Drop for OutputMode {
    #[expect(unsafe_code, reason = "the console's mode is only reachable through the Windows API")]
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode};

        if let Some(mode) = self.restore {
            // SAFETY: It takes this process's own output handle.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), mode);
            }
        }
    }
}

/// This console's keys, pastes and size changes.
pub(super) struct Input {
    events: EventStream,
}

impl Input {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the relay opens each platform's input alike, and opening a Unix terminal can fail"
    )]
    pub(super) fn open() -> Result<Self, Error> {
        Ok(Self {
            events: EventStream::new(),
        })
    }

    /// What the console does next, or `None` when its input ends. Dropping the call before it completes loses
    /// nothing.
    pub(super) async fn next(&mut self) -> Result<Option<Local>, Error> {
        loop {
            match self.events.next().await {
                Some(Ok(Event::Key(key))) => {
                    if let Some(bytes) = key_bytes(key) {
                        return Ok(Some(Local::Input(Bytes::from(bytes))));
                    }
                }
                Some(Ok(Event::Paste(text))) => return Ok(Some(Local::Input(Bytes::from(text.into_bytes())))),
                Some(Ok(Event::Resize(columns, rows))) => {
                    if let Ok(size) = TerminalSize::new(rows, columns) {
                        return Ok(Some(Local::Resize(size)));
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => return Ok(None),
            }
        }
    }
}

/// The bytes a terminal sends for `key`, as the program on the other side expects them.
fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    // Windows reports a character typed with AltGr, such as `@` on a Norwegian keyboard, as one
    // with Control and Alt held.
    let control = key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
    let alt = key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL);
    let bytes = match key.code {
        KeyCode::Char(character) if control => {
            let byte = u8::try_from(character.to_ascii_lowercase()).ok()?;
            vec![byte & 0x1f]
        }
        KeyCode::Char(character) if alt => format!("\x1b{character}").into_bytes(),
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::key_bytes;

    #[test]
    fn keys_reach_the_program_as_a_terminal_sends_them() {
        let press = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            key_bytes(press(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(b"a".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            key_bytes(press(KeyCode::Enter, KeyModifiers::NONE)),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT)),
            Some(b"@".to_vec()),
            "AltGr on Windows types the character"
        );
        assert_eq!(
            key_bytes(press(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            key_bytes(press(KeyCode::Delete, KeyModifiers::NONE)),
            Some(b"\x1b[3~".to_vec())
        );
        let mut release = press(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_bytes(release), None);
    }
}
