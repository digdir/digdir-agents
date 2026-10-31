//! GitHub sign-in with `gh`, run in a sandbox like the harness sign-ins.

use crate::Error;

use super::SandboxSignIn;

/// The `gh` configuration of a sign-in in a sandbox, which exists only for that sign-in.
const SIGN_IN_CONFIGURATION: &str = "/tmp/agentctl-github-sign-in";
/// Without a terminal to read from, `gh` asks nothing and waits for the browser once it has shown
/// its link and code. The sandbox has no keyring and is deleted afterwards, so it stores the token
/// in its configuration, and the token goes to a file rather than to the person's screen. It has
/// no clipboard either, so `gh` does not try to copy the code; the host copies it instead. GitHub
/// refuses a push that changes a workflow file without the `workflow` scope, which `gh` does not ask
/// for by default.
const SIGN_IN_SCRIPT: &str = r#"umask 077 && mkdir -p "$GH_CONFIG_DIR" && gh config set clipboard disabled && gh auth login --web --hostname github.com --git-protocol https --skip-ssh-key --insecure-storage --scopes workflow </dev/null && gh auth token --hostname github.com >"$GH_CONFIG_DIR/token""#;

/// Signs in to GitHub with `gh auth login --web` in a sandbox, which shows a link and a one-time
/// code to enter there, and leaves the token in its artifact.
#[must_use]
pub fn sign_in() -> SandboxSignIn {
    SandboxSignIn {
        execution: sandbox::execution::ExecutionSpec::command(
            sandbox::SandboxPath::new("/bin/sh"),
            ["-c".into(), SIGN_IN_SCRIPT.into()],
        )
        .with_environment([
            ("GH_CONFIG_DIR".into(), SIGN_IN_CONFIGURATION.into()),
            // Uncoloured, `gh` does not query the terminal, whose answer nothing would read.
            ("NO_COLOR".into(), "1".into()),
        ]),
        artifact: Some(sandbox::SandboxPath::new(format!("{SIGN_IN_CONFIGURATION}/token"))),
        credential: |_output, artifact| {
            let token = super::artifact_text("GitHub", artifact)?;
            if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(Error::Invalid(
                    "the GitHub sign-in left something other than a token".into(),
                ));
            }
            Ok(token)
        },
    }
}

/// Where and with which one-time code the person completes a GitHub sign-in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceCode {
    pub url: reqwest::Url,
    pub code: String,
}

/// The device page and one-time code that a GitHub sign-in's terminal `output` shows, once it
/// shows both in full.
#[must_use]
pub fn device_code(output: &[u8]) -> Option<DeviceCode> {
    const LABEL: &[u8] = b"one-time code";
    let url = super::sign_in_links(output).into_iter().find(is_sign_in_link)?;
    let text = super::terminal_text(output).to_ascii_lowercase();
    let label = text.windows(LABEL.len()).rposition(|window| window == LABEL)?;
    // `gh` shows the code after a colon, or in parentheses when it copied it to the clipboard.
    let rest = &text[label + LABEL.len()..];
    let start = rest.iter().position(|&byte| !matches!(byte, b' ' | b':' | b'('))?;
    let rest = &rest[start..];
    let end = rest
        .iter()
        .position(|&byte| !(byte.is_ascii_alphanumeric() || byte == b'-'))?;
    let code = std::str::from_utf8(&rest[..end]).ok()?.to_ascii_uppercase();
    let (first, second) = code.split_once('-')?;
    (first.len() == 4 && second.len() == 4 && !second.contains('-')).then_some(DeviceCode { url, code })
}

/// Whether `url` is the page where a GitHub device sign-in takes its one-time code.
pub(super) fn is_sign_in_link(url: &reqwest::Url) -> bool {
    url.host_str() == Some("github.com") && url.path() == "/login/device"
}

#[cfg(test)]
mod tests {
    /// What `gh auth login --web` 2.103 prints, without and with a clipboard to copy the code to.
    const SIGN_IN_OUTPUTS: [&str; 2] = [
        "\r\n! First copy your one-time code: 535F-72BF\r\nOpen this URL to continue in your web browser: https://github.com/login/device\r\n",
        "\x1b]11;?\x1b\\\x1b[6n\r\n\x1b[0;33m!\x1b[0m One-time code (\x1b[0;1;39m342E-CF9C\x1b[0m) copied to clipboard\r\n\
         \x1b[0;1;39mOpen this URL\x1b[0m to continue in your web browser: https://github.com/login/device\r\n",
    ];

    #[test]
    fn the_device_code_is_found_once_it_has_arrived_in_full() {
        for (output, code) in SIGN_IN_OUTPUTS.into_iter().zip(["535F-72BF", "342E-CF9C"]) {
            let found = super::device_code(output.as_bytes()).expect("device code");
            assert_eq!(found.url.as_str(), "https://github.com/login/device");
            assert_eq!(found.code, code);
            for end in 0..output.len() {
                if let Some(found) = super::device_code(&output.as_bytes()[..end]) {
                    assert_eq!(found.code, code, "after {end} bytes");
                }
            }
        }
    }

    #[test]
    fn the_token_is_taken_from_the_file_it_leaves() {
        let sign_in = super::sign_in();
        let token = sign_in.credential(b"", Some(b"gho_abc123\n")).expect("token");
        assert_eq!(token.as_str(), "gho_abc123");
        for artifact in [None, Some(b"".as_slice()), Some(b"no token here".as_slice())] {
            sign_in
                .credential(b"", artifact)
                .expect_err("a sign-in that left no token has no credential");
        }
    }
}
