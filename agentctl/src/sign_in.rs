//! Sign-ins that run in a short-lived sandbox, so the host needs none of the programs they run.
//!
//! A sign-in's program talks to the person through a terminal that the host relays, and the person
//! signs in with a browser on the host, at the pages the program links to. What it signs in to is
//! the sign-in's own business: each one names its program, the pages it may open and where its
//! credential is left.

use zeroize::Zeroizing;

use crate::Error;

/// A sign-in that runs in a short-lived sandbox.
///
/// Once its program has exited successfully, [`Self::credential`] takes the credential from its
/// output and artifact.
#[derive(Clone, Debug)]
pub struct SandboxSignIn {
    /// The program to start in a terminal Execution.
    pub execution: sandbox::execution::ExecutionSpec,
    /// The file that holds the credential once the sign-in has exited, for the host to read back.
    pub artifact: Option<sandbox::SandboxPath>,
    opens: OpensLink,
    credential: ReadCredential,
}

/// Whether a link the sign-in printed, as a terminal `hyperlinked` one or not, is one of its own
/// sign-in pages.
type OpensLink = fn(&reqwest::Url, bool) -> bool;

/// Reads a finished sign-in's credential from its terminal output and its artifact.
type ReadCredential = fn(&[u8], Option<&[u8]>) -> Result<Zeroizing<String>, Error>;

impl SandboxSignIn {
    pub(crate) const fn new(
        execution: sandbox::execution::ExecutionSpec,
        artifact: Option<sandbox::SandboxPath>,
        opens: OpensLink,
        credential: ReadCredential,
    ) -> Self {
        Self {
            execution,
            artifact,
            opens,
            credential,
        }
    }

    /// The credential the finished sign-in produced, from all of its terminal `output` and the
    /// contents of its [`Self::artifact`].
    ///
    /// # Errors
    ///
    /// Returns an error when the sign-in left no credential.
    pub fn credential(&self, output: &[u8], artifact: Option<&[u8]>) -> Result<Zeroizing<String>, Error> {
        (self.credential)(output, artifact)
    }

    /// The sign-in pages that the sign-in's terminal `output` links to, for the host to open in a
    /// browser.
    ///
    /// Only the pages the sign-in names as its own count, so a program cannot send the person's
    /// browser to another site. What a page is asked for, in its query, is the program's.
    ///
    /// Call it again as output arrives: it returns every link so far, so the caller remembers
    /// which it has opened, and leaves out a link until the output shows where it ends.
    #[must_use]
    pub fn links(&self, output: &[u8]) -> Vec<reqwest::Url> {
        let (text, hyperlinks) = terminal_text(output);
        let mut candidates = Vec::<&str>::new();
        for (start, _) in text.windows(8).enumerate().filter(|(_, window)| *window == b"https://") {
            let rest = &text[start..];
            let Some(end) = rest.iter().position(|&byte| !url_byte(byte)) else {
                continue;
            };
            let link = std::str::from_utf8(&rest[..end])
                .unwrap_or_default()
                .trim_end_matches(['.', ',', ':', ';', '!', '?']);
            if !candidates.contains(&link) {
                candidates.push(link);
            }
        }
        // A terminal wraps a long link over lines, which cuts its text short. Its hyperlink, when the
        // program prints one, holds all of it, so a link that another one starts with is cut short.
        candidates
            .iter()
            .filter(|link| {
                !candidates
                    .iter()
                    .any(|other| other.len() > link.len() && other.starts_with(**link))
            })
            .filter_map(|link| {
                let url = reqwest::Url::parse(link).ok()?;
                let hyperlinked = hyperlinks.contains(&link.as_bytes());
                (self.opens)(&url, hyperlinked).then_some(url)
            })
            .collect()
    }
}

/// The text of the artifact a sign-in of `service` leaves its credential in.
pub(crate) fn artifact_text(service: &str, artifact: Option<&[u8]>) -> Result<Zeroizing<String>, Error> {
    artifact
        .and_then(|artifact| std::str::from_utf8(artifact).ok())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| Zeroizing::new(text.to_owned()))
        .ok_or_else(|| Error::Invalid(format!("the {service} sign-in left no credential")))
}

fn url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~:/?#@!$&*+,;=%".contains(&byte)
}

/// The text a terminal shows for `output`, without its escape sequences.
///
/// A colour change is dropped, so that it cannot split a word. Any other sequence becomes a
/// space, because it may stand for one, as a cursor movement does, or end a line before a redraw.
/// A hyperlink's target becomes text of its own, and is also returned on its own. A sequence that
/// `output` ends in the middle of is dropped with the rest, so that text still arriving never looks
/// finished.
fn terminal_text(output: &[u8]) -> (Vec<u8>, Vec<&[u8]>) {
    const ESCAPE: u8 = 0x1b;
    const BELL: u8 = 0x07;
    let mut text = Vec::with_capacity(output.len());
    let mut hyperlinks = Vec::new();
    let mut rest = output;
    while let Some((&byte, after)) = rest.split_first() {
        if byte != ESCAPE {
            text.push(byte);
            rest = after;
            continue;
        }
        match after.first() {
            Some(b'[') => {
                let Some(end) = after.iter().skip(1).position(|byte| (0x40..=0x7e).contains(byte)) else {
                    break;
                };
                if after[end + 1] != b'm' {
                    text.push(b' ');
                }
                rest = &after[end + 2..];
            }
            Some(&kind @ (b']' | b'P' | b'X' | b'^' | b'_')) => {
                let body = &after[1..];
                let Some(end) = body.iter().position(|&byte| byte == BELL || byte == ESCAPE) else {
                    break;
                };
                let terminator = if body[end] == BELL { 1 } else { 2 };
                if body.len() < end + terminator {
                    break;
                }
                text.push(b' ');
                if kind == b']'
                    && let Some(target) = hyperlink_target(&body[..end])
                {
                    text.extend_from_slice(target);
                    text.push(b' ');
                    hyperlinks.push(target);
                }
                rest = &body[end + terminator..];
            }
            Some(_) => {
                let Some(end) = after.iter().position(|byte| (0x30..=0x7e).contains(byte)) else {
                    break;
                };
                text.push(b' ');
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    (text, hyperlinks)
}

/// The target of an OSC 8 hyperlink sequence, `8;parameters;target`, unless it ends one.
fn hyperlink_target(sequence: &[u8]) -> Option<&[u8]> {
    let rest = sequence.strip_prefix(b"8;")?;
    let parameters = rest.iter().position(|&byte| byte == b';')?;
    Some(&rest[parameters + 1..]).filter(|target| !target.is_empty())
}
