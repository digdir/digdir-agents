//! Waiting out Microsandbox's install lease.
//!
//! While an install, update or catalog upgrade holds the lease, Microsandbox refuses to open its
//! catalog and names the time the lease ends. It refuses before it changes anything, so trying
//! again once the lease ends is safe. Microsandbox reports the refusal only as a message, so it is
//! recognized by that.

use std::{future::Future, time::Duration};

use microsandbox::{MicrosandboxError, MicrosandboxResult};

/// How Microsandbox's refusal starts while the lease is held.
const REFUSAL: &str = "microsandbox install operation in progress";

/// How long to wait for the lease to end. A lease expires 30 minutes after its holder last renewed
/// it, so this outlasts one an interrupted install left behind, but not a live install that keeps
/// renewing it.
const LIMIT: Duration = Duration::from_mins(30);

/// How often to try again. The lease ends before the time it names when its holder finishes.
const POLL: Duration = Duration::from_secs(2);

/// How long to wait before saying why, so a lease that ends at once goes unmentioned.
const NOTICE_AFTER: Duration = Duration::from_secs(3);

/// Runs `attempt` until Microsandbox no longer refuses it for the install lease, or the lease
/// outlives [`LIMIT`], and returns its last result.
pub(crate) async fn wait_out<T, F, Fut>(mut attempt: F) -> MicrosandboxResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = MicrosandboxResult<T>>,
{
    let start = tokio::time::Instant::now();
    let mut noticed = false;
    loop {
        match attempt().await {
            Err(MicrosandboxError::Runtime(message)) if message.starts_with(REFUSAL) && start.elapsed() < LIMIT => {
                if !noticed && start.elapsed() >= NOTICE_AFTER {
                    noticed = true;
                    // The refusal names the time in UTC, then asks for what this does.
                    let until = message
                        .strip_prefix(REFUSAL)
                        .and_then(|rest| rest.split(';').next())
                        .unwrap_or_default();
                    tracing::warn!("waiting for a Microsandbox install or update to finish; its lease runs{until} UTC");
                }
                tokio::time::sleep(POLL).await;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::cell::Cell;

    use super::*;

    fn refusal() -> MicrosandboxError {
        MicrosandboxError::Runtime(format!(
            "{REFUSAL} until 2026-11-10 15:05:30.414714600; retry after it completes"
        ))
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn waits_until_the_lease_ends() {
        let attempts = Cell::new(0);
        let start = tokio::time::Instant::now();
        let result = wait_out(|| async {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 4 { Err(refusal()) } else { Ok(()) }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.get(), 4);
        assert!(start.elapsed() < LIMIT);
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn fails_when_the_lease_outlives_the_limit() {
        let start = tokio::time::Instant::now();
        let result: MicrosandboxResult<()> = wait_out(|| async { Err(refusal()) }).await;
        let error = result.expect_err("a lease that never ends should fail");
        assert!(error.to_string().contains(REFUSAL));
        assert!(start.elapsed() >= LIMIT);
    }

    #[tokio::test(flavor = "local", start_paused = true)]
    async fn other_failures_are_not_retried() {
        let attempts = Cell::new(0);
        let result: MicrosandboxResult<()> = wait_out(|| async {
            attempts.set(attempts.get() + 1);
            Err(MicrosandboxError::Runtime("database is locked".into()))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }
}
