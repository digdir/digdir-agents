use std::{path::PathBuf, rc::Rc, time::Duration};

use futures_util::{FutureExt as _, StreamExt as _, stream::FuturesUnordered};
use sandbox::LocalFuture;

use crate::Error;

use super::{Connector, Server, client::Connection};

/// Most calls served at once. Connections switched to streams do not count; the server limits those.
const MAX_CONCURRENT_CONNECTIONS: usize = 128;
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_mins(1);
type ConnectionFuture = futures_util::future::LocalBoxFuture<'static, ()>;

async fn drain_connections(connections: &mut FuturesUnordered<ConnectionFuture>, timeout: Duration) {
    if tokio::time::timeout(timeout, async { while connections.next().await.is_some() {} })
        .await
        .is_err()
    {
        tracing::warn!("cancelled Control API calls that did not finish during the shutdown drain");
    }
}

/// How long the server stops accepting after an accept fails for want of a
/// resource, such as descriptors, that closing connections frees again.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Takes an accepted connection, or pauses accepting after a failure that
/// leaves the listener working. Running out of descriptors, which open
/// streams make reachable, must not end the server, since the connections
/// it already carries free them again as they close.
fn take_accepted<T>(
    accepted: std::io::Result<T>,
    paused_until: &mut Option<tokio::time::Instant>,
) -> Result<Option<T>, Error> {
    match accepted {
        Ok(accepted) => Ok(Some(accepted)),
        Err(error) if listener_is_broken(&error) => Err(Error::Io(error)),
        Err(error) => {
            tracing::warn!(%error, "could not accept a Control API connection, retrying shortly");
            *paused_until = Some(tokio::time::Instant::now() + ACCEPT_RETRY_DELAY);
            Ok(None)
        }
    }
}

/// Whether an accept error means the listener itself no longer works.
fn listener_is_broken(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        use rustix::io::Errno;

        Errno::from_io_error(error)
            .is_some_and(|errno| matches!(errno, Errno::BADF | Errno::INVAL | Errno::NOTSOCK | Errno::OPNOTSUPP))
    }
    #[cfg(windows)]
    {
        // WSAEBADF, WSAEINVAL, WSAENOTSOCK and WSAEOPNOTSUPP.
        matches!(error.raw_os_error(), Some(10009 | 10022 | 10038 | 10045))
    }
}

/// Connector for the fixed per-user Control API socket path.
pub(super) struct PathConnector {
    path: PathBuf,
}

impl PathConnector {
    #[must_use]
    pub(super) const fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[cfg(unix)]
impl Connector for PathConnector {
    fn connect(&self) -> LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(&self.path).await?;
            Ok(Box::new(stream) as Box<dyn Connection>)
        })
    }
}

#[cfg(target_os = "windows")]
impl Connector for PathConnector {
    fn connect(&self) -> LocalFuture<'_, Result<Box<dyn Connection>, Error>> {
        Box::pin(async move {
            use tokio_util::compat::FuturesAsyncReadCompatExt as _;

            let stream = win_uds::net::AsyncStream::connect(&self.path).await?.compat();
            Ok(Box::new(HalfClosing(stream)) as Box<dyn Connection>)
        })
    }
}

#[cfg(unix)]
pub(crate) async fn serve(server: Rc<Server>, path: &std::path::Path) -> Result<(), Error> {
    use std::os::unix::fs::FileTypeExt;

    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("local API socket has no parent directory".into()))?;
    std::fs::create_dir_all(parent)?;
    crate::local::home::secure_directory(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => return Err(Error::Invalid("local API path exists and is not a socket".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::Io(error)),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    crate::local::home::secure_file(path)?;
    let mut connections = FuturesUnordered::<ConnectionFuture>::new();
    let mut paused_until = None;

    loop {
        if server.is_draining() {
            break;
        }
        tokio::select! {
            accepted = listener.accept(), if paused_until.is_none() && connections.len() < MAX_CONCURRENT_CONNECTIONS + server.open_streams() => {
                let Some((stream, _)) = take_accepted(accepted, &mut paused_until)? else {
                    continue;
                };
                let connection_server = server.clone();
                connections.push(async move {
                    if let Err(error) = connection_server.serve_connection(stream).await {
                        connection_server.report(&error);
                    }
                }.boxed_local());
            }
            Some(()) = connections.next(), if !connections.is_empty() => {}
            () = tokio::time::sleep_until(paused_until.unwrap_or_else(tokio::time::Instant::now)), if paused_until.is_some() => {
                paused_until = None;
            }
            // A connection that became a stream frees its place for calls.
            () = server.stream_opened() => {}
            () = server.shutdown_requested() => break,
        }
    }
    drain_connections(&mut connections, CONNECTION_DRAIN_TIMEOUT).await;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) async fn serve(server: Rc<Server>, path: &std::path::Path) -> Result<(), Error> {
    use tokio_util::compat::FuturesAsyncReadCompatExt as _;

    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("local API socket has no parent directory".into()))?;
    std::fs::create_dir_all(parent)?;
    crate::local::home::secure_directory(parent)?;
    sweep_quarantined_socket_directories(parent);
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            // Windows leaves the AF_UNIX path behind after an abnormal exit.
            // Refuse a path with a live listener, but remove an unreachable
            // entry before binding. agentd holds the exclusive home lock while
            // calling this function, so another daemon cannot race recovery.
            if win_uds::net::AsyncStream::connect(path).await.is_ok() {
                return Err(Error::Invalid("local API path is already occupied".into()));
            }
            // afd.sys can keep a stale socket file undeletable and unbindable
            // until reboot. Renaming its directory aside still works then, so
            // quarantine it and recreate the directory before binding.
            if std::fs::remove_file(path).is_err() {
                quarantine_socket_directory(parent)?;
                std::fs::create_dir_all(parent)?;
                crate::local::home::secure_directory(parent)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::Io(error)),
    }
    // The socket inherits the user-only ACL from the home directory secured
    // above; icacls cannot open an AF_UNIX socket reparse point (error 1920).
    let listener = win_uds::net::AsyncListener::bind(path)?;
    let _cleanup = SocketCleanup(path.to_path_buf());
    let mut connections = FuturesUnordered::<ConnectionFuture>::new();
    let mut paused_until = None;

    loop {
        if server.is_draining() {
            break;
        }
        tokio::select! {
            accepted = listener.accept(), if paused_until.is_none() && connections.len() < MAX_CONCURRENT_CONNECTIONS + server.open_streams() => {
                let Some((stream, _)) = take_accepted(accepted, &mut paused_until)? else {
                    continue;
                };
                let connection_server = server.clone();
                connections.push(async move {
                    if let Err(error) = connection_server.serve_connection(HalfClosing(stream.compat())).await {
                        connection_server.report(&error);
                    }
                }.boxed_local());
            }
            Some(()) = connections.next(), if !connections.is_empty() => {}
            () = tokio::time::sleep_until(paused_until.unwrap_or_else(tokio::time::Instant::now)), if paused_until.is_some() => {
                paused_until = None;
            }
            // A connection that became a stream frees its place for calls.
            () = server.stream_opened() => {}
            () = server.shutdown_requested() => break,
        }
    }
    drain_connections(&mut connections, CONNECTION_DRAIN_TIMEOUT).await;
    Ok(())
}

/// A Windows `AF_UNIX` connection whose shutdown ends its write direction, as
/// it does on Unix, where the socket type's own close only flushes. A stream
/// carries the end of one direction of a guest connection that way.
#[cfg(target_os = "windows")]
struct HalfClosing(tokio_util::compat::Compat<win_uds::net::AsyncStream>);

#[cfg(target_os = "windows")]
impl tokio::io::AsyncRead for HalfClosing {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

#[cfg(target_os = "windows")]
impl tokio::io::AsyncWrite for HalfClosing {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::ready!(std::pin::Pin::new(&mut self.0).poll_flush(context))?;
        std::task::Poll::Ready(self.0.get_ref().get_ref().shutdown(std::net::Shutdown::Write))
    }
}

#[cfg(target_os = "windows")]
const QUARANTINE_INFIX: &str = ".stale-";
#[cfg(target_os = "windows")]
const QUARANTINE_ATTEMPTS: u32 = 1000;

/// Renames the socket directory to an unused `<name>.stale-<n>` sibling.
#[cfg(target_os = "windows")]
fn quarantine_socket_directory(directory: &std::path::Path) -> Result<(), Error> {
    let name = directory
        .file_name()
        .ok_or_else(|| Error::Invalid("local API socket directory has no name".into()))?;
    for attempt in 0..QUARANTINE_ATTEMPTS {
        let mut candidate = name.to_os_string();
        candidate.push(format!("{QUARANTINE_INFIX}{attempt}"));
        let candidate = directory.with_file_name(candidate);
        if candidate.exists() {
            continue;
        }
        match std::fs::rename(directory, &candidate) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Error::Io(error)),
        }
    }
    Err(Error::Invalid(
        "no free quarantine name for the local API socket directory".into(),
    ))
}

/// Best-effort removal of quarantined socket directories; stale `AF_UNIX`
/// files become deletable again after a reboot.
#[cfg(target_os = "windows")]
fn sweep_quarantined_socket_directories(directory: &std::path::Path) {
    let (Some(parent), Some(name)) = (directory.parent(), directory.file_name().and_then(|name| name.to_str())) else {
        return;
    };
    let prefix = format!("{name}{QUARANTINE_INFIX}");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(|name| name.starts_with(&prefix)) {
            let _ignored = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(target_os = "windows")]
struct SocketCleanup(PathBuf);

#[cfg(target_os = "windows")]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_drain_is_bounded_by_its_deadline() {
        let mut connections = FuturesUnordered::new();
        connections.push(std::future::pending::<()>().boxed_local());

        tokio::time::timeout(
            Duration::from_millis(100),
            drain_connections(&mut connections, Duration::ZERO),
        )
        .await
        .expect("bounded drain");
        assert_eq!(connections.len(), 1);
        drop(connections);
    }
}
