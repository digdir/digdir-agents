//! What a terminal shows for a program's output, as rows of text.
//!
//! A program that draws its interface, as Claude Code's does, does not print lines one after the
//! other: it moves the cursor and overwrites what it drew before. It breaks a long value over
//! rows with a carriage return and a cursor movement as often as with a newline. Reading the
//! output as lines then cuts values apart, or joins text the screen never shows together. Replaying
//! the movements onto a grid gives the rows as the person saw them.

use zeroize::Zeroizing;

/// The width the output is replayed at: wider than any terminal a sign-in runs in, so a row the
/// program draws is never wrapped again, while the breaks it makes itself, with a newline or a
/// cursor movement, are kept.
const COLUMNS: std::num::NonZeroU16 = std::num::NonZeroU16::new(1000).expect("non-zero");

/// The rows kept: the screen a sign-in ends on holds what it printed last.
const ROWS: std::num::NonZeroU16 = std::num::NonZeroU16::new(200).expect("non-zero");

/// The rows a terminal shows after `output`, each without trailing blanks, and without the blank
/// rows below the last one written. A newline also returns to the first column, as a terminal with
/// output translation does.
pub(super) fn rows(output: &[u8]) -> Zeroizing<Vec<String>> {
    let mut translated = Zeroizing::new(Vec::with_capacity(output.len()));
    for (index, &byte) in output.iter().enumerate() {
        if byte == b'\n' && (index == 0 || output[index - 1] != b'\r') {
            translated.push(b'\r');
        }
        translated.push(byte);
    }
    let mut parser = vt100::Parser::new(ROWS, COLUMNS, 0);
    parser.process(&translated);
    let mut rows: Vec<String> = parser
        .screen()
        .rows(0, COLUMNS.get())
        .map(|row| row.trim_end().to_owned())
        .collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    Zeroizing::new(rows)
}

#[cfg(test)]
mod tests {
    use super::rows;

    fn shown(output: &str) -> Vec<String> {
        rows(output.as_bytes()).to_vec()
    }

    #[test]
    fn movements_and_erases_draw_as_a_terminal_does() {
        assert_eq!(shown("one\r\ntwo\r\n"), ["one", "two"]);
        assert_eq!(shown("typed\x1b[2K\rfinal"), ["final"]);
        assert_eq!(shown("abc\x1b[1B\x1b[1A\rX"), ["Xbc"]);
        assert_eq!(shown("first\r\x1b[1C\x1b[1Bsecond"), ["first", " second"]);
        assert_eq!(shown("\x1b[5Gx\x1b[1Gy"), ["y   x"]);
        assert_eq!(shown("line\r\nnext\x1b[1A\r\x1b[K"), ["", "next"]);
        assert_eq!(shown("a\x1b7b\r\nc\x1b8d"), ["ad", "c"]);
    }

    #[test]
    fn colours_hyperlinks_and_modes_draw_nothing() {
        assert_eq!(
            shown("\x1b[?25l\x1b[38;5;174mred\x1b[39m \x1b]8;id=1;https://example.com\x07link\x1b]8;;\x07\x1b(B\x0f"),
            ["red link"]
        );
        assert_eq!(shown("cut \x1b[38;5"), ["cut"]);
    }
}
