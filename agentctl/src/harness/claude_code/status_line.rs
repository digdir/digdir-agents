//! Claude Code status-line configuration for managed agent sessions.

/// The status-line script, which also reports how much of the session's login is used.
///
/// # Errors
///
/// Returns an error when the Platform API locations cannot be encoded as JSON.
pub(super) fn script() -> Result<String, serde_json::Error> {
    Ok(include_str!("status_line.mjs").replace("__REPORTER__", &super::super::hook_script::reporter()?))
}

pub(super) fn configuration(script_path: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": format!("node {script_path}")
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use tokio::io::AsyncWriteExt as _;

    /// Runs the status line once for `state` against `url_file`, with its markers in `tmp`, and
    /// returns what it printed.
    async fn render(script: &str, state: &str, tmp: &std::path::Path) -> String {
        let mut child = tokio::process::Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(script)
            .env("AGENT_SESSION_TOKEN", "token")
            .env("AGENT_SESSION_ID", "6a3c1f0e-2b7d-4e59-9c84-0f1d2e3a4b5c")
            // Where `os.tmpdir()` looks on each platform.
            .env("TMPDIR", tmp)
            .env("TEMP", tmp)
            .env("TMP", tmp)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("Node.js runs the status line, as it does in every sandbox and CI host");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(state.as_bytes()).await.expect("write state");
        drop(stdin);
        let output = tokio::time::timeout(std::time::Duration::from_mins(1), child.wait_with_output())
            .await
            .expect("status line finished")
            .expect("status line ran");
        String::from_utf8(output.stdout).expect("UTF-8 output")
    }

    #[tokio::test(flavor = "local")]
    async fn a_new_reading_of_the_logins_limits_is_reported_once() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let url_file = directory.path().join("platform-api-url");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let port = listener.local_addr().expect("address").port();
        std::fs::write(&url_file, format!("http://127.0.0.1:{port}\n")).expect("URL file");
        let script = super::script().expect("script").replacen(
            &serde_json::to_string(crate::sandbox::platform::PLATFORM_API_URL_FILE).expect("path"),
            &serde_json::to_string(&url_file).expect("path"),
            1,
        );
        // A busy test host can take longer to answer than a status line waits, and an unanswered
        // reading is sent again.
        let waits = "windows }), 300)";
        assert!(script.contains(waits), "the status line waits for its report");
        let script = script.replacen(waits, "windows }), 30_000)", 1);
        let state = r#"{"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":4102444800},"seven_day":{"used_percentage":40,"resets_at":4103049600}}}"#;
        let received = tokio::task::spawn_local(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let request = crate::harness::read_test_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .expect("respond");
            (request, listener)
        });

        let printed = render(&script, state, directory.path()).await;
        let (request, listener) = received.await.expect("report received");

        assert!(printed.contains("5h 76% left"), "{printed}");
        assert!(request.starts_with("POST /v1/session/usage HTTP/1.1"), "{request}");
        assert!(
            request.contains(r#""usedPercent":23.5,"resetsAt":4102444800"#),
            "{request}"
        );
        assert!(
            request.contains(r#""usedPercent":40,"resetsAt":4103049600"#),
            "{request}"
        );
        render(&script, state, directory.path()).await;
        let resent = tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept()).await;
        assert!(resent.is_err(), "an unchanged reading is not sent again");
    }
}
