//! Blocking work, such as reading a directory tree, run off the local async runtime.
//!
//! The runtime's one thread serves every request and sandbox, so work that blocks it, such as
//! file system calls, runs on Tokio's blocking threads instead. Run it through [`run`] rather than
//! spawning it directly. [`run`] limits how much of each kind of [`Work`] runs at once, so that
//! agents applied together do not copy every tree at the same time, and lets the work stop once
//! nothing waits for it.
//!
//! Work stops where it calls [`check`], or writes through [`Stoppable`]. Code that takes time in
//! proportion to its input does so between steps, such as between the files it reads. It needs
//! no handle for it, so code that also runs outside [`run`], such as in the CLI, checks the same
//! way, and there the check never fails.
//!
//! The limits and [`stop_all`] belong to the thread that starts the work, which for a daemon is
//! its runtime's thread.

use std::{cell::RefCell, io, sync::Arc};

use tokio::{sync::Semaphore, task::JoinError};
use tokio_util::sync::CancellationToken;

/// What a piece of blocking work costs, which decides how many of its kind run at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Work {
    /// Touches a few files, so it needs no limit.
    Brief,
    /// Walks a host directory tree, which may be a whole checkout.
    Tree,
    /// Copies a directory tree into an image or archive, which holds disk or memory the size of
    /// the tree while it runs.
    Copy,
}

/// How many [`Work::Tree`] run at once.
const TREE_LIMIT: usize = 4;
/// How many [`Work::Copy`] run at once.
const COPY_LIMIT: usize = 2;

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

/// The blocking work one thread starts. A permit moves to the blocking thread with its work, so
/// the semaphores are shared across threads.
struct Pool {
    stop: CancellationToken,
    tree: Arc<Semaphore>,
    copy: Arc<Semaphore>,
}

impl Pool {
    fn limit(&self, work: Work) -> Option<Arc<Semaphore>> {
        match work {
            Work::Brief => None,
            Work::Tree => Some(self.tree.clone()),
            Work::Copy => Some(self.copy.clone()),
        }
    }
}

/// The work a blocking thread runs for [`run`], which [`check`] reads.
struct Running {
    token: CancellationToken,
    what: &'static str,
}

thread_local! {
    static POOL: Pool = Pool {
        stop: CancellationToken::new(),
        tree: Arc::new(Semaphore::new(TREE_LIMIT)),
        copy: Arc::new(Semaphore::new(COPY_LIMIT)),
    };
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

/// Runs `task` on a blocking thread once its kind of `work` is below its limit, and returns
/// what it returns. `what` describes the work in errors, such as "reading the skill".
///
/// Dropping the returned future before it completes makes [`check`] fail in `task`. The thread
/// itself cannot be interrupted, so `task` runs until it next checks or returns, and its place
/// under the limit stays taken until then.
///
/// # Errors
///
/// Returns [`Error::Failed`] when `task` panics.
pub async fn run<T, F>(work: Work, what: &'static str, task: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (token, limit) = POOL.with(|pool| (pool.stop.child_token(), pool.limit(work)));
    // The semaphores are never closed, so acquiring one cannot fail.
    let permit = match limit {
        Some(limit) => limit.acquire_owned().await.ok(),
        None => None,
    };
    let _stop_when_dropped = token.clone().drop_guard();
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the one place that starts blocking work"
    )]
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
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
    POOL.with(|pool| pool.stop.cancel());
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use tokio::{runtime::LocalRuntime, sync::oneshot};

    use super::{COPY_LIMIT, Error, Stoppable, Work, check, run, stop_all};

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
        let work = tokio::task::spawn_local(run(Work::Tree, "waiting", until_stopped(started, stopped)));
        has_started.await.expect("the work should start");
        work.abort();
        tokio::time::timeout(Duration::from_secs(5), has_stopped)
            .await
            .expect("the work should stop")
            .expect("the work should report stopping");
    }

    #[tokio::test(flavor = "local")]
    async fn limits_how_much_work_of_a_kind_runs_at_once() {
        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        static MOST: AtomicUsize = AtomicUsize::new(0);
        let copy = || {
            run(Work::Copy, "copying", || {
                let running = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
                MOST.fetch_max(running, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                RUNNING.fetch_sub(1, Ordering::SeqCst);
            })
        };
        let results = tokio::join!(copy(), copy(), copy(), copy(), copy());
        for result in <[_; 5]>::from(results) {
            result.expect("the copy should finish");
        }
        assert_eq!(MOST.load(Ordering::SeqCst), COPY_LIMIT);
    }

    #[test]
    fn stop_all_stops_running_and_later_work() {
        // On its own thread, so stopping its work cannot stop another test's.
        #[allow(clippy::disallowed_methods, reason = "isolates the thread-local state under test")]
        let thread = std::thread::spawn(|| {
            LocalRuntime::new().expect("runtime").block_on(async {
                let (started, has_started) = oneshot::channel();
                let (stopped, has_stopped) = oneshot::channel();
                let running = tokio::task::spawn_local(run(Work::Brief, "waiting", until_stopped(started, stopped)));
                has_started.await.expect("the work should start");
                stop_all();
                has_stopped.await.expect("running work should stop");
                running.await.expect("task").expect("running work should return");
                let later = run(Work::Brief, "checking", check).await;
                assert!(matches!(later, Ok(Err(Error::Stopped { what: "checking" }))));
                let written = run(Work::Brief, "writing", || Stoppable::new(Vec::new()).write_all(b"data")).await;
                assert!(written.expect("the work should return").is_err());
            });
        });
        thread.join().expect("the test thread should finish");
    }
}
