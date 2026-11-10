//! Blocking work, such as reading a directory tree, run off the local async runtime.
//!
//! The runtime's one thread serves every request and sandbox, so work that blocks it, such as
//! file system calls, runs on Tokio's blocking threads instead. Run it through [`run`] rather than
//! spawning it directly, so that it stops once nothing waits for it.
//!
//! Work stops where it calls [`check`], or writes through [`Stoppable`]. Code that takes time in
//! proportion to its input does so between steps, such as between the files it reads. It needs
//! no handle for it, so code that also runs outside [`run`], such as in the CLI, checks the same
//! way, and there the check never fails.
//!
//! [`stop_all`] stops the work the thread calling it started, which for a daemon is its runtime's
//! thread.

use std::{cell::RefCell, io};

use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

/// Blocking work that did not finish.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The work stopped at a [`check`], as nothing waited for it any more.
    #[error("{what} stopped, as nothing waits for it any more")]
    Stopped {
        /// The work, as its caller described it.
        what: &'static str,
    },
    /// The work panicked.
    #[error("{what} failed: {source}")]
    Failed {
        /// The work, as its caller described it.
        what: &'static str,
        /// Why the blocking thread did not return.
        #[source]
        source: JoinError,
    },
}

/// The work a blocking thread runs for [`run`], which [`check`] reads.
struct Running {
    token: CancellationToken,
    what: &'static str,
}

thread_local! {
    /// Stops the work this thread started, for [`stop_all`].
    static STOP: CancellationToken = CancellationToken::new();
    static RUNNING: RefCell<Option<Running>> = const { RefCell::new(None) };
}

/// Clears [`RUNNING`] when the work ends, also when it panics, as the thread runs other work next.
struct Entered;

impl Entered {
    fn new(running: Running) -> Self {
        RUNNING.set(Some(running));
        Self
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        RUNNING.set(None);
    }
}

/// Runs `task` on a blocking thread and returns what it returns. `what` describes the work in
/// errors, such as "reading the skill".
///
/// Dropping the returned future before it completes makes [`check`] fail in `task`. The thread
/// itself cannot be interrupted, so `task` runs until it next checks or returns.
///
/// # Errors
///
/// Returns [`Error::Failed`] when `task` panics.
pub async fn run<T, F>(what: &'static str, task: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let token = STOP.with(CancellationToken::child_token);
    let _stop_when_dropped = token.clone().drop_guard();
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the one place that starts blocking work"
    )]
    tokio::task::spawn_blocking(move || {
        let _entered = Entered::new(Running { token, what });
        task()
    })
    .await
    .map_err(|source| Error::Failed { what, source })
}

/// Fails once the blocking work this thread runs for [`run`] should stop: its caller no longer
/// waits for it, or [`stop_all`] was called. Outside [`run`] it never fails.
///
/// # Errors
///
/// Returns [`Error::Stopped`] when the work should stop.
pub fn check() -> Result<(), Error> {
    RUNNING.with_borrow(|running| match running {
        Some(running) if running.token.is_cancelled() => Err(Error::Stopped { what: running.what }),
        _ => Ok(()),
    })
}

/// Writes to the writer it holds after each [`check`], so that a copy into it, such as of a large
/// file into an archive, stops within one write.
pub struct Stoppable<W>(W);

impl<W> Stoppable<W> {
    /// Writes to `inner`.
    pub const fn new(inner: W) -> Self {
        Self(inner)
    }

    /// Returns the writer it writes to.
    pub fn into_inner(self) -> W {
        self.0
    }
}

impl<W: io::Write> io::Write for Stoppable<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        check().map_err(io::Error::other)?;
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Stops the work this thread started, and any it starts later, at its next [`check`].
///
/// A daemon calls this as it shuts down, before it waits for its blocking threads, so that work it
/// no longer needs ends early and cleans up after itself.
pub fn stop_all() {
    STOP.with(CancellationToken::cancel);
}

#[cfg(test)]
mod tests {
    use std::{io::Write as _, time::Duration};

    use tokio::{runtime::LocalRuntime, sync::oneshot};

    use super::{Error, Stoppable, check, run, stop_all};

    /// Reports that it started, then waits until it is told to stop and reports that.
    fn until_stopped(started: oneshot::Sender<()>, stopped: oneshot::Sender<()>) -> impl FnOnce() + Send {
        move || {
            let _ = started.send(());
            while check().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = stopped.send(());
        }
    }

    #[tokio::test(flavor = "local")]
    async fn work_stops_once_nothing_waits_for_it() {
        let (started, has_started) = oneshot::channel();
        let (stopped, has_stopped) = oneshot::channel();
        let work = tokio::task::spawn_local(run("waiting", until_stopped(started, stopped)));
        has_started.await.expect("the work should start");
        work.abort();
        tokio::time::timeout(Duration::from_secs(5), has_stopped)
            .await
            .expect("the work should stop")
            .expect("the work should report stopping");
    }

    #[test]
    fn stop_all_stops_running_and_later_work() {
        // On its own thread, so stopping its work cannot stop another test's.
        #[allow(clippy::disallowed_methods, reason = "isolates the thread-local state under test")]
        let thread = std::thread::spawn(|| {
            LocalRuntime::new().expect("runtime").block_on(async {
                let (started, has_started) = oneshot::channel();
                let (stopped, has_stopped) = oneshot::channel();
                let running = tokio::task::spawn_local(run("waiting", until_stopped(started, stopped)));
                has_started.await.expect("the work should start");
                stop_all();
                has_stopped.await.expect("running work should stop");
                running.await.expect("task").expect("running work should return");
                let later = run("checking", check).await;
                assert!(matches!(later, Ok(Err(Error::Stopped { what: "checking" }))));
                let written = run("writing", || Stoppable::new(Vec::new()).write_all(b"data")).await;
                assert!(written.expect("the work should return").is_err());
            });
        });
        thread.join().expect("the test thread should finish");
    }
}
