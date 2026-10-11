//! Renderer for the harness activity report hook.
//!
//! Both supported harnesses run hook commands with a JSON description of the
//! event on stdin, so one script shape serves both. Everything harness-specific
//! (which `hook_event_name`s exist, what each one means to the platform, which
//! notification types matter) is a table the adapter owns and hands to
//! [`HookScript::render`]; this module knows only the wire contract with the
//! Platform API.

use crate::sessions::ActivityEvent;

/// The harness-specific table an activity hook script is rendered from.
pub(super) struct HookScript<'a> {
    /// Harness hook event names and the platform signal each one carries.
    pub(super) events: &'a [(&'a str, ActivityEvent)],
    /// `Notification` types that mean the harness is blocked on the operator;
    /// other notifications carry no signal. Empty when the harness has no
    /// notification hook.
    pub(super) waiting_notifications: &'a [&'a str],
}

const TEMPLATE: &str = r#"import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";

// agentd's port changes when it restarts, so the endpoint is read on every report.
function platformUrl() {
  try {
    const base = readFileSync(__PLATFORM_API_URL_FILE__, "utf8").trim();
    return base === "" ? null : base + __SESSION_HOOKS_PATH__;
  } catch {
    return null;
  }
}

const url = platformUrl();
const token = process.env.AGENT_SESSION_TOKEN;
const sessionId = process.env.AGENT_SESSION_ID;

// Harness hook event -> platform activity signal. Rendered from the adapter's
// table; events not listed carry no signal and are ignored.
const EVENTS = __EVENTS__;
const WAITING_NOTIFICATIONS = __WAITING_NOTIFICATIONS__;

function read(stream) {
  // Codex 0.156 keeps stdin open while waiting for the hook to exit.
  // Resolve on a complete object without waiting for EOF or stream cleanup.
  return new Promise((resolve) => {
    let data = "";
    const finish = (value) => {
      stream.off("data", onData);
      stream.off("end", onEnd);
      stream.off("error", onEnd);
      resolve(value);
    };
    const parse = () => {
      try {
        const value = JSON.parse(data);
        return value !== null && typeof value === "object" && !Array.isArray(value) ? value : null;
      } catch {
        return null;
      }
    };
    const onData = (chunk) => {
      data += chunk;
      if (data.length > 1048576) {
        finish(null);
      } else {
        const value = parse();
        if (value !== null) finish(value);
      }
    };
    const onEnd = () => finish(parse());
    stream.setEncoding("utf8");
    stream.on("data", onData);
    stream.on("end", onEnd);
    stream.on("error", onEnd);
  });
}

const input = await read(process.stdin);
if (!url || !token || !sessionId || input === null) process.exit(0);
const event = EVENTS[input.hook_event_name];
if (!event) process.exit(0);
// A nested Agent's own reports carry an agent_id; never forward those.
if (input.agent_id) process.exit(0);
// Only notifications that block on the operator are activity.
if (
  input.hook_event_name === "Notification" &&
  typeof input.notification_type === "string" &&
  !WAITING_NOTIFICATIONS.includes(input.notification_type)
) {
  process.exit(0);
}

const body = { sessionId, eventId: randomUUID(), event, source: typeof input.source === "string" ? input.source : "" };
if (event === "sessionStart") {
  if (typeof input.session_id !== "string" || input.session_id === "") process.exit(0);
  body.nativeSessionId = input.session_id;
  if (typeof input.transcript_path === "string" && input.transcript_path !== "") {
    body.transcriptPath = input.transcript_path;
  }
}
const payload = JSON.stringify(body);

// Start and terminal reports unblock callers, so they retry within a strict budget.
// The payload keeps the same event ID across retries, including a lost response.
// Other activity uses one short best-effort attempt.
const retryable = ["sessionStart", "turnCompleted", "waitingForInput"].includes(event);
const attempts = retryable ? 3 : 1;
const budget = retryable ? 1500 : 300;
const perAttempt = retryable ? 450 : 250;
const deadline = Date.now() + budget;
for (let attempt = 0; attempt < attempts; attempt += 1) {
  const remaining = deadline - Date.now();
  if (remaining <= 0) break;
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
      body: payload,
      signal: AbortSignal.timeout(Math.min(perAttempt, remaining)),
    });
    if (response.ok) break;
  } catch {}
  if (attempt < attempts - 1) {
    const pause = Math.min(75, deadline - Date.now());
    if (pause > 0) await new Promise((resolve) => setTimeout(resolve, pause));
  }
}
process.exit(0);
"#;

impl HookScript<'_> {
    /// Renders the hook script with the adapter's tables embedded.
    ///
    /// # Errors
    ///
    /// Returns an error when the tables cannot be encoded as JSON.
    pub(super) fn render(&self) -> Result<String, serde_json::Error> {
        let events = self
            .events
            .iter()
            .map(|(name, event)| Ok(((*name).to_owned(), serde_json::to_value(event)?)))
            .collect::<Result<serde_json::Map<_, _>, serde_json::Error>>()?;
        let events = serde_json::to_string(&serde_json::Value::Object(events))?;
        let waiting = serde_json::to_string(self.waiting_notifications)?;
        Ok(TEMPLATE
            .replace(
                "__PLATFORM_API_URL_FILE__",
                &serde_json::to_string(crate::sandbox::platform::PLATFORM_API_URL_FILE)?,
            )
            .replace(
                "__SESSION_HOOKS_PATH__",
                &serde_json::to_string(crate::platform_api::SESSION_HOOKS_PATH)?,
            )
            .replace("__EVENTS__", &events)
            .replace("__WAITING_NOTIFICATIONS__", &waiting))
    }

    /// Hook event names the adapter registers: exactly the table's keys.
    pub(super) fn event_names(&self) -> impl Iterator<Item = &str> {
        self.events.iter().map(|(name, _)| *name)
    }
}

#[cfg(test)]
mod tests {
    use super::HookScript;
    use crate::sessions::ActivityEvent;

    #[tokio::test]
    async fn hook_exits_after_complete_json_even_when_stdin_remains_open() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let script = HookScript {
            events: &[("SessionStart", ActivityEvent::SessionStart)],
            waiting_notifications: &[],
        }
        .render()
        .expect("script");
        let script = script.replacen(
            "const input = await read(process.stdin);",
            "process.stdout.write('ready\\n');\nconst input = await read(process.stdin);",
            1,
        );
        let mut child = tokio::process::Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env_remove("AGENT_SESSION_TOKEN")
            .env_remove("AGENT_SESSION_ID")
            .kill_on_drop(true)
            .spawn()
            .expect("Node.js runs the hook script, as it does in every Sandbox and CI host");
        let mut ready = [0; 6];
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            child.stdout.as_mut().expect("stdout").read_exact(&mut ready),
        )
        .await
        .expect("Node started")
        .expect("Node reported readiness");
        assert_eq!(&ready, b"ready\n");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(br#"{"hook_event_name":"SessionStart"}"#)
            .await
            .expect("write hook payload");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().expect("hook status") {
                assert!(status.success());
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "hook waited for stdin EOF");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A harness that outlives an `agentd` restart keeps its launch environment and its hook
    /// script, so the hook must reach whichever endpoint the URL file names when it runs.
    #[tokio::test]
    async fn the_same_hook_follows_the_platform_api_url_file_to_a_new_port() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let url_file = directory.path().join("platform-api-url");
        let script = HookScript {
            events: &[("SessionStart", ActivityEvent::SessionStart)],
            waiting_notifications: &[],
        }
        .render()
        .expect("script")
        // The guest path is not writable here; point the script at a file the test owns.
        .replacen(
            &serde_json::to_string(crate::sandbox::platform::PLATFORM_API_URL_FILE).expect("path"),
            &serde_json::to_string(&url_file).expect("path"),
            1,
        );

        // Both stay bound, so the restarted daemon's port differs from the first.
        let first = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let restarted = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        for listener in [&first, &restarted] {
            let port = listener.local_addr().expect("address").port();
            std::fs::write(&url_file, format!("http://127.0.0.1:{port}\n")).expect("URL file");
            assert_eq!(
                report_request_line(&script, listener).await,
                "POST /v1/session/hooks HTTP/1.1"
            );
        }
    }

    /// Runs the hook for one `SessionStart` and returns the request line it sends to `listener`.
    async fn report_request_line(script: &str, listener: &tokio::net::TcpListener) -> String {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

        let mut child = tokio::process::Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env("AGENT_SESSION_TOKEN", "token")
            .env("AGENT_SESSION_ID", "session")
            .kill_on_drop(true)
            .spawn()
            .expect("Node.js runs the hook script, as it does in every Sandbox and CI host");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(br#"{"hook_event_name":"SessionStart","session_id":"native"}"#)
            .await
            .expect("write hook payload");
        drop(stdin);
        let (stream, _) = tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
            .await
            .expect("hook connected")
            .expect("accept");
        let mut line = String::new();
        tokio::io::BufReader::new(stream)
            .read_line(&mut line)
            .await
            .expect("request line");
        child.kill().await.expect("stop hook");
        line.trim_end().to_owned()
    }
}
