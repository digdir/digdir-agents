//! agentctl as the browser of a harness login, and where that login sends the
//! person to sign in.

use std::{ffi::OsString, path::PathBuf};

use agent::harness::{SignIn, UrlOpener};

use super::host::{launch_blocked, open_url, set, wsl};

/// Set for a harness that runs agentctl as its browser, so that agentctl only
/// opens the address it is given.
const AS_BROWSER_ENVIRONMENT: &str = "AGENTCTL_AS_BROWSER";

/// Whether agentctl runs as a harness's browser rather than as itself.
pub(crate) fn as_browser(variable: impl Fn(&str) -> Option<OsString>) -> bool {
    set(&variable, AS_BROWSER_ENVIRONMENT)
}

/// Opens the one address agentctl is given when it runs as a harness's browser.
pub(crate) fn open_argument() -> std::process::ExitCode {
    let opened = address_to_open(std::env::args_os().skip(1)).and_then(|url| {
        tokio::runtime::LocalRuntime::new()
            .map_err(|error| format!("could not start: {error}"))
            .and_then(|runtime| runtime.block_on(open_url(&url)))
    });
    opened.map_or_else(
        |error| {
            eprintln!("agentctl: {error}");
            std::process::ExitCode::FAILURE
        },
        |_| std::process::ExitCode::SUCCESS,
    )
}

/// The address a harness asks its browser to open. Only web addresses are
/// accepted: the openers behind it also open files and start programs.
fn address_to_open(arguments: impl IntoIterator<Item = OsString>) -> Result<String, String> {
    let mut arguments = arguments.into_iter();
    match (arguments.next().map(OsString::into_string), arguments.next()) {
        (Some(Ok(url)), None) if url.starts_with("https://") || url.starts_with("http://") => Ok(url),
        _ => Err("expected one http or https address to open".to_owned()),
    }
}

/// Where a harness login sends the person: a browser here when one would reach
/// them, or else a link to open elsewhere. A `BROWSER` of the person's own is
/// the harness's to use; otherwise, under WSL, where a harness's own opener
/// misses the Windows desktop, `agentctl`, this program, stands in as its
/// browser.
pub(crate) fn sign_in(variable: impl Fn(&str) -> Option<OsString>, agentctl: Option<PathBuf>) -> SignIn {
    if let Some(reason) = launch_blocked(&variable) {
        return SignIn::Elsewhere { reason };
    }
    if set(&variable, "BROWSER") || !wsl(&variable) {
        return SignIn::Browser(None);
    }
    agentctl.map_or_else(
        || SignIn::Elsewhere {
            reason: "agentctl could not find itself to open the Windows browser".into(),
        },
        |program| {
            SignIn::Browser(Some(UrlOpener {
                program,
                environment: (AS_BROWSER_ENVIRONMENT, "1"),
            }))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{super::test_support::*, *};

    #[cfg(target_os = "linux")]
    #[test]
    fn a_login_opens_through_agentctl_only_under_wsl_without_a_browser_of_the_persons_own() {
        let windows = windows_path();
        let agentctl = PathBuf::from("/opt/agentctl/agentctl");
        let through_agentctl = SignIn::Browser(Some(UrlOpener {
            program: agentctl.clone(),
            environment: (AS_BROWSER_ENVIRONMENT, "1"),
        }));
        let sign_in_with =
            |pairs, agentctl: Option<&PathBuf>| sign_in(with_path(windows.path(), pairs), agentctl.cloned());
        assert_eq!(sign_in_with(UNDER_WSL, Some(&agentctl)), through_agentctl);
        assert_eq!(
            sign_in_with(&[("WSL_DISTRO_NAME", "Ubuntu"), ("BROWSER", "")], Some(&agentctl)),
            through_agentctl
        );
        assert_eq!(
            sign_in_with(
                &[("WSL_DISTRO_NAME", "Ubuntu"), ("BROWSER", "wslview")],
                Some(&agentctl)
            ),
            SignIn::Browser(None)
        );
        assert!(matches!(sign_in_with(UNDER_WSL, None), SignIn::Elsewhere { .. }));
        assert_eq!(
            sign_in_with(&[("DISPLAY", ":0")], Some(&agentctl)),
            SignIn::Browser(None)
        );
        assert!(matches!(
            sign_in_with(&[("SSH_TTY", "/dev/pts/1")], Some(&agentctl)),
            SignIn::Elsewhere { .. }
        ));
    }

    #[test]
    fn only_a_single_web_address_is_opened_as_a_browser() {
        for (arguments, expected) in [
            (
                &["https://claude.com/authorize?a=1&b=%2F"][..],
                Some("https://claude.com/authorize?a=1&b=%2F"),
            ),
            (
                &["http://localhost:1455/callback"],
                Some("http://localhost:1455/callback"),
            ),
            (&["status"], None),
            (&["C:\\Windows\\notepad.exe"], None),
            (&["https://a.example", "https://b.example"], None),
            (&[], None),
        ] {
            let address = address_to_open(arguments.iter().map(OsString::from));
            assert_eq!(address.ok().as_deref(), expected, "{arguments:?}");
        }
        assert!(!as_browser(only(&[("AGENTCTL_AS_BROWSER", "")])));
        assert!(as_browser(only(&[("AGENTCTL_AS_BROWSER", "1")])));
    }
}
