//! Signing in to GitHub through the browser, with the GitHub CLI in a sandbox.
//!
//! Unlike the harnesses' sign-ins, it never runs a `gh` on the host, even one that is there. A
//! configuration directory of its own doesn't isolate `gh`: it keeps tokens in the system keyring
//! under the host and user only, and a login replaces the keyring's active token, or activates a
//! token the person kept there for the same user.

use crate::sign_in::{SandboxSignIn, artifact_text};

/// Where `gh` keeps its configuration, and the token, in the sign-in agent, which is deleted
/// with the agent.
const CONFIGURATION: &str = "/tmp/gh-sign-in";
/// The scopes `gh auth login` asks for without being told; [`super::SCOPES`] adds to them.
const GH_DEFAULT_SCOPES: [&str; 3] = ["repo", "read:org", "gist"];
/// Runs the device sign-in, then writes the token where the host reads it back. With its input
/// away from the terminal, `gh` shows the code and the link and waits for the browser at once,
/// rather than for Enter to open a browser the sandbox doesn't have.
const SCRIPT: &str = r#"umask 077 && mkdir -p "$GH_CONFIG_DIR" \
&& gh auth login --hostname github.com --web --git-protocol https --clipboard=false --expiring=false \
   --insecure-storage --scopes "$1" </dev/null \
&& gh auth token --hostname github.com >"$GH_CONFIG_DIR/token""#;

/// Where the account a browser sign-in acted as revokes its token, which also revokes that
/// account's other GitHub CLI logins.
pub const REVOKE_PAGE: &str = super::TokenKind::Oauth.revoke_page();

/// The GitHub CLI's browser sign-in, `gh auth login --web`, run in a sandbox.
///
/// The token is one of the GitHub CLI's own OAuth app, with the [`super::SCOPES`] agents need. It
/// does not expire: `gh` would refresh an expiring one, which agents cannot.
#[must_use]
pub fn sandbox_sign_in() -> SandboxSignIn {
    let scopes = additional_scopes();
    SandboxSignIn::new(
        sandbox::execution::ExecutionSpec::command(
            sandbox::SandboxPath::new("/bin/sh"),
            ["-c", SCRIPT, "sh", &scopes].map(str::to_owned),
        )
        .with_environment([
            ("GH_CONFIG_DIR".into(), CONFIGURATION.into()),
            ("GH_NO_UPDATE_NOTIFIER".into(), "1".into()),
            // Otherwise `gh` asks the terminal for its background colour, and the relay doesn't
            // pass the answer back, so it waits five seconds before showing the code.
            ("GLAMOUR_STYLE".into(), "dark".into()),
        ]),
        Some(sandbox::SandboxPath::new(format!("{CONFIGURATION}/token"))),
        |url, _hyperlinked| is_sign_in_link(url),
        |_output, artifact| artifact_text("GitHub", artifact),
    )
}

/// The [`super::SCOPES`] that `gh auth login` must be asked for.
fn additional_scopes() -> String {
    super::SCOPES
        .iter()
        .filter(|scope| !GH_DEFAULT_SCOPES.contains(scope))
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `url` is the page where a GitHub device sign-in takes its one-time code.
fn is_sign_in_link(url: &reqwest::Url) -> bool {
    url.host_str() == Some("github.com") && url.path() == "/login/device"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_asks_gh_for_the_scopes_it_does_not_request_by_default() {
        assert_eq!(additional_scopes(), "workflow,read:project");
        let sign_in = sandbox_sign_in();
        let arguments = format!("{:?}", sign_in.execution);
        assert!(arguments.contains("workflow,read:project"), "{arguments}");
        assert_eq!(
            sign_in
                .credential(b"", Some(b"gho_FAKE\n"))
                .expect("credential")
                .as_str(),
            "gho_FAKE"
        );
        assert!(
            sign_in.credential(b"gho_FAKE", None).is_err(),
            "the screen is never read"
        );
    }

    #[test]
    fn only_the_device_page_on_github_opens() {
        let link = |text: &str| reqwest::Url::parse(text).expect("url");
        assert!(is_sign_in_link(&link("https://github.com/login/device")));
        for other in [
            "https://github.com/login/device/extra",
            "https://github.com/settings/tokens",
            "https://github.com.example.org/login/device",
            "https://gist.github.com/login/device",
        ] {
            assert!(!is_sign_in_link(&link(other)), "{other}");
        }
    }

    #[test]
    fn the_device_page_is_found_in_what_gh_prints() {
        let output = b"\x1b[0;33m!\x1b[0m First copy your one-time code: \x1b[0;1;39mABCD-1234\x1b[0m\r\n\
            \x1b[0;1;39mOpen this URL\x1b[0m to continue in your web browser: https://github.com/login/device\r\n";
        let links = sandbox_sign_in().links(output);
        assert_eq!(
            links.iter().map(reqwest::Url::as_str).collect::<Vec<_>>(),
            ["https://github.com/login/device"]
        );
    }
}
