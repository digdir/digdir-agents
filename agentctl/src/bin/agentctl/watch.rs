//! `get --watch`: the selected resources each time they change.

use std::time::Duration;

use agent::{control_api::Client, resources::Resources};

use crate::CommandResult;

/// Pause before watching again after the daemon could not be reached.
pub(crate) const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// Calls `changed` with the current selection, then again each time it changes.
///
/// Each call has the full selection rather than a change, so one missed while
/// agentd restarts needs no recovery. The watch reconnects to a restarted
/// agentd and ends with an error once agentd is incompatible, such as after an
/// upgrade, so that its caller can start it again from the new binary.
pub(crate) async fn follow<T: PartialEq>(
    client: &Client,
    select: impl Fn(Resources) -> Vec<T>,
    mut changed: impl FnMut(&[T]) -> CommandResult<()>,
) -> CommandResult<()> {
    let mut after = None;
    let mut last = None;
    let mut reachable = true;
    loop {
        // agentd may have restarted as another build since the last reply, such as after an upgrade.
        let reply = match client.health().await {
            Ok(daemon) => {
                daemon.require_compatible()?;
                client.watch_resources(after).await
            }
            Err(error) => Err(error),
        };
        match reply {
            Ok(resources) => {
                after = Some(resources.revision);
                reachable = true;
                let selected = select(resources);
                if last.as_ref() != Some(&selected) {
                    changed(&selected)?;
                    last = Some(selected);
                }
            }
            Err(error) => {
                if reachable {
                    eprintln!("agentctl: lost agentd, reconnecting: {error}");
                    reachable = false;
                }
                tokio::time::sleep(RECONNECT_INTERVAL).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use agent::{Error, control_api::Connection, sessions::Session};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// What the scripted daemon does with one request.
    enum Reply {
        /// Answers a watch with these Sessions at this revision.
        Watch(&'static str, serde_json::Value),
        /// Closes the connection without answering, as a stopping agentd does.
        Drop,
        /// Refuses the connection, as when agentd is not running.
        Refuse,
        /// Answers a health check from a daemon of this build or another.
        Health { compatible: bool },
    }

    struct Scripted {
        replies: Rc<RefCell<VecDeque<Reply>>>,
        requests: Rc<RefCell<Vec<serde_json::Value>>>,
    }

    impl agent::control_api::Connector for Scripted {
        fn connect(&self) -> sandbox::LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
            Box::pin(async move {
                let reply = self.replies.borrow_mut().pop_front().expect("scripted reply");
                if matches!(reply, Reply::Refuse) {
                    return Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into());
                }
                let (client, server) = tokio::io::duplex(64 * 1024);
                let requests = self.requests.clone();
                tokio::task::spawn_local(async move {
                    let mut server = tokio::io::BufReader::new(server);
                    let mut line = String::new();
                    server.read_line(&mut line).await.expect("request");
                    let request: serde_json::Value = serde_json::from_str(&line).expect("RPC");
                    requests.borrow_mut().push(request.clone());
                    let result = match reply {
                        Reply::Drop | Reply::Refuse => return,
                        Reply::Watch(revision, sessions) => {
                            assert_eq!(request["method"], "resources.v1.watch");
                            serde_json::json!({"revision": revision, "agents": [], "sessions": sessions})
                        }
                        Reply::Health { compatible } => {
                            assert_eq!(request["method"], "control.v1.health");
                            let build = if compatible { agent::build_version() } else { "0.0.0" };
                            serde_json::json!({
                                "protocolVersion": agent::control_api::PROTOCOL_VERSION,
                                "buildVersion": build
                            })
                        }
                    };
                    let response = serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
                    server
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .expect("response");
                });
                Ok(Box::new(client) as Box<dyn Connection>)
            })
        }
    }

    fn session(agent: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "agentId": "00000000-0000-0000-0000-000000000002",
            "agent": agent,
            "name": name,
            "harness": "claudeCode",
            "createdAt": "2026-08-25T00:00:00Z",
            "status": {"state": "idle", "lifecycle": {"state": "running"}}
        })
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn prints_each_change_across_restarts_until_agentd_is_incompatible() {
        const FIRST: &str = "00000000-0000-0000-0000-00000000000a:1";
        const RESTARTED: &str = "00000000-0000-0000-0000-00000000000b:0";
        let compatible = || Reply::Health { compatible: true };
        let replies = VecDeque::from([
            compatible(),
            Reply::Watch(FIRST, serde_json::json!([session("worker", "s1")])),
            // A keepalive, and a change to a Session outside the selection.
            compatible(),
            Reply::Watch(FIRST, serde_json::json!([session("worker", "s1")])),
            compatible(),
            Reply::Watch(
                "00000000-0000-0000-0000-00000000000a:2",
                serde_json::json!([session("worker", "s1"), session("builder", "b1")]),
            ),
            // agentd stops, and is started again.
            compatible(),
            Reply::Drop,
            Reply::Refuse,
            compatible(),
            Reply::Watch(
                RESTARTED,
                serde_json::json!([session("worker", "s1"), session("worker", "s2")]),
            ),
            // agentd is upgraded.
            Reply::Health { compatible: false },
        ]);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let client = Client::new(Rc::new(Scripted {
            replies: Rc::new(RefCell::new(replies)),
            requests: requests.clone(),
        }));
        let mut printed = Vec::new();

        let error = follow(
            &client,
            |resources| {
                resources
                    .sessions
                    .into_iter()
                    .filter(|session| session.agent == "worker")
                    .collect::<Vec<Session>>()
            },
            |sessions| {
                printed.push(
                    sessions
                        .iter()
                        .map(|session| session.name.to_string())
                        .collect::<Vec<_>>(),
                );
                Ok(())
            },
        )
        .await
        .expect_err("an incompatible agentd ends the watch");

        assert!(error.to_string().contains("incompatible"), "{error}");
        assert_eq!(printed, [vec!["s1"], vec!["s1", "s2"]]);
        let watched_after = requests
            .borrow()
            .iter()
            .filter(|request| request["method"] == "resources.v1.watch")
            .map(|request| request["params"]["after"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert_eq!(watched_after[..2], [None, Some(FIRST.to_owned())]);
    }
}
