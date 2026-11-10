//! Canonical paths as a person reads them.
//!
//! On Windows, `std::fs::canonicalize` returns a verbatim path, such as `\\?\D:\work`, which people
//! do not recognize, and which other programs and paths that a person typed do not compare equal
//! to. [`canonicalize`] leaves out the `\\?\` prefix whenever the path means the same without it.

use std::{
    io,
    path::{Path, PathBuf},
};

/// `path` absolute, with every link and `.` or `..` resolved, as [`std::fs::canonicalize`] gives
/// it, but in the form [`simplified`] gives.
///
/// # Errors
///
/// Returns the error of [`std::fs::canonicalize`], such as when `path` does not exist.
#[expect(clippy::disallowed_methods, reason = "this is what callers use instead")]
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplified)
}

/// [`canonicalize`] for async code, run on Tokio's blocking threads.
///
/// # Errors
///
/// Returns the error of [`std::fs::canonicalize`], such as when `path` does not exist.
#[expect(clippy::disallowed_methods, reason = "this is what callers use instead")]
pub async fn canonicalize_async(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    tokio::fs::canonicalize(path).await.map(simplified)
}

/// `path` without the Windows verbatim prefix `\\?\`, when it means the same without it. A path
/// recorded before agentctl simplified its paths is compared with this. Elsewhere it is `path`.
#[must_use]
pub fn simplified(path: PathBuf) -> PathBuf {
    if cfg!(windows)
        && let Some(simple) = path.to_str().and_then(without_verbatim_prefix)
    {
        return PathBuf::from(simple);
    }
    path
}

/// The longest path Windows takes without the verbatim prefix, which lifts that limit.
const MAX_PATH: usize = 260;

/// `path`, a verbatim Windows path to a drive or a network share, without its verbatim prefix,
/// when Windows would read it as the same path without it. Without the prefix, Windows parses the
/// path: it limits its length, drops a trailing dot or space from a name, and reads a name such as
/// `CON` as a device.
fn without_verbatim_prefix(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    // What follows the drive, or the server and share and what follows them.
    let (simple, names, least) = if let Some(share) = rest.strip_prefix(r"UNC\") {
        (format!(r"\\{share}"), share, 2)
    } else {
        let [letter, b':', b'\\', ..] = rest.as_bytes() else {
            return None;
        };
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        (rest.to_owned(), rest.get(3..)?, 0)
    };
    let names = if names.is_empty() {
        Vec::new()
    } else {
        names.split('\\').collect()
    };
    (names.len() >= least && names.into_iter().all(plain_name) && simple.len() < MAX_PATH).then_some(simple)
}

/// Whether Windows reads `name` as it is in a path without the verbatim prefix.
fn plain_name(name: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    const NUMBERED: [&str; 2] = ["COM", "LPT"];
    if name.is_empty() || name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return false;
    }
    if name
        .chars()
        .any(|character| character.is_control() || r#"<>:"/|?*"#.contains(character))
    {
        return false;
    }
    // A device name is one with any extension, such as `nul.txt`.
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let numbered = NUMBERED.iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|number| {
            let mut digits = number.chars();
            matches!(
                (digits.next(), digits.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
        })
    });
    !DEVICES.contains(&stem.as_str()) && !numbered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_path_loses_its_prefix_when_it_means_the_same_without_it() {
        for (verbatim, simple) in [
            (r"\\?\D:\work\agents\.env", r"D:\work\agents\.env"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\work", r"\\server\share\work"),
            (r"\\?\UNC\server\share", r"\\server\share"),
            (r"\\?\D:\console\nullable.txt", r"D:\console\nullable.txt"),
        ] {
            assert_eq!(without_verbatim_prefix(verbatim).as_deref(), Some(simple), "{verbatim}");
        }
    }

    #[test]
    fn a_verbatim_path_keeps_its_prefix_when_windows_would_read_it_otherwise() {
        let long = format!(r"\\?\C:\{}", "a".repeat(MAX_PATH));
        for verbatim in [
            r"D:\work",
            r"\\?\C:",
            r"\\?\D:\work\nul",
            r"\\?\D:\work\COM1.txt",
            r"\\?\D:\work\lpt²",
            r"\\?\D:\work\name.",
            r"\\?\D:\work\name ",
            r"\\?\D:\work\..\other",
            r"\\?\D:\work\\twice",
            r"\\?\D:\a/b",
            r"\\?\Volume{0b5ba2a5-0000-0000-0000-100000000000}\work",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
            r"\\?\UNC\server",
            long.as_str(),
        ] {
            assert_eq!(without_verbatim_prefix(verbatim), None, "{verbatim}");
        }
    }

    #[test]
    fn a_canonical_path_is_absolute_and_simple() {
        let directory = std::env::temp_dir();
        let canonical = canonicalize(&directory).expect("canonical");
        assert!(canonical.is_absolute());
        assert!(
            !canonical.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            canonical.display()
        );
    }
}
