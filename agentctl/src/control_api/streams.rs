//! Streams that calls switch their connections to, carried on a thread of their own.
//!
//! The control thread answers calls and resolves what a stream reaches. The
//! connection then moves to the stream thread, which opens the stream, answers
//! the call and relays until both sides have finished. The bytes of an SSH
//! session or a forwarded port never pass through the control thread, so a
//! slow call or reconciliation pass does not stall them, and a busy stream
//! does not hold up calls. Streams share their thread with each other.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use sandbox::LocalFuture;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, BufReader},
    sync::{mpsc, watch},
};

use crate::Error;

use super::{
    protocol::{CODE_INTERNAL, StreamKind, StreamResult, error_response},
    server::{result_response, write_response},
};

/// A connection a call switches to a stream, which the stream thread takes over.
pub trait StreamConnection: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> StreamConnection for T {}

/// A connection the server accepts, which can leave the runtime that accepted it.
///
/// A tokio socket is registered with the I/O driver of the runtime that
/// created it, and only that runtime's thread learns when it can be read or
/// written. Moved to the stream thread as it is, a socket accepted on the
/// control thread would still wait for the control thread to wake it, so a
/// blocked control thread would stall every stream. Such a socket leaves its
/// runtime here and registers again on the stream thread. Connections that no
/// runtime drives, such as in-memory pipes, cross as they are.
pub trait ServedConnection: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    /// Leaves the runtime this connection is registered with, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot leave its runtime.
    fn detach(self) -> std::io::Result<Detached>;
}

/// A connection on its way to another runtime, which registers it there.
pub struct Detached(DetachedConnection);

enum DetachedConnection {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Unregistered(Box<dyn StreamConnection>),
}

impl Detached {
    /// Takes a connection that no runtime drives across as it is.
    #[must_use]
    pub fn unregistered(connection: impl StreamConnection + 'static) -> Self {
        Self(DetachedConnection::Unregistered(Box::new(connection)))
    }

    /// Registers the connection with the runtime of the calling thread.
    #[cfg_attr(
        not(unix),
        allow(clippy::unnecessary_wraps, reason = "only registering a Unix socket again can fail")
    )]
    fn attach(self) -> std::io::Result<Box<dyn StreamConnection>> {
        match self.0 {
            #[cfg(unix)]
            DetachedConnection::Unix(socket) => Ok(Box::new(tokio::net::UnixStream::from_std(socket)?)),
            DetachedConnection::Unregistered(connection) => Ok(connection),
        }
    }
}

#[cfg(unix)]
impl ServedConnection for tokio::net::UnixStream {
    fn detach(self) -> std::io::Result<Detached> {
        Ok(Detached(DetachedConnection::Unix(self.into_std()?)))
    }
}

impl ServedConnection for tokio::io::DuplexStream {
    fn detach(self) -> std::io::Result<Detached> {
        Ok(Detached::unregistered(self))
    }
}

/// A sandbox resolved on the control thread, which the stream thread dials.
pub trait GuestTcpTarget: Send {
    /// Dials `port` on the sandbox's loopback interface. The stream thread
    /// calls this, so the connection is read there.
    fn connect(self: Box<Self>, port: std::num::NonZeroU16)
    -> LocalFuture<'static, Result<Box<dyn ByteStream>, Error>>;
}

impl GuestTcpTarget for crate::sandbox::GuestTarget {
    fn connect(
        self: Box<Self>,
        port: std::num::NonZeroU16,
    ) -> LocalFuture<'static, Result<Box<dyn ByteStream>, Error>> {
        Box::pin(async move {
            let connection = Self::connect(*self, "127.0.0.1", port.get()).await?;
            Ok(Box::new(connection) as Box<dyn ByteStream>)
        })
    }
}

/// Bytes a call carries both ways over its connection once it has answered,
/// such as a connection dialed into a sandbox.
pub trait ByteStream {
    /// Relays the stream over `connection` until both directions have closed.
    fn relay(self: Box<Self>, connection: Box<dyn StreamConnection>) -> LocalFuture<'static, Result<(), Error>>;
}

impl ByteStream for crate::sandbox::GuestConnection {
    fn relay(self: Box<Self>, connection: Box<dyn StreamConnection>) -> LocalFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let (reader, writer) = tokio::io::split(connection);
            self.relay_io(reader, writer).await
        })
    }
}

/// What the stream thread opens for a call, as resolved on the control thread.
pub(super) enum Opening {
    /// A TCP connection dialed inside a sandbox, carried as bytes.
    GuestTcp {
        target: Box<dyn GuestTcpTarget>,
        port: std::num::NonZeroU16,
    },
}

impl Opening {
    const fn kind(&self) -> StreamKind {
        match self {
            Self::GuestTcp { .. } => StreamKind::Bytes,
        }
    }

    async fn open(self) -> Result<Box<dyn ByteStream>, Error> {
        match self {
            Self::GuestTcp { target, port } => target.connect(port).await,
        }
    }
}

/// What both threads know about the open streams.
struct Shared {
    open: AtomicUsize,
    closing: watch::Sender<bool>,
}

/// Counts one stream as open until it is dropped, on whichever thread holds it then.
pub(super) struct Open(Arc<Shared>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A connection the control thread hands to the stream thread, with the call it answers.
struct Handoff {
    id: u64,
    connection: Detached,
    /// What the control thread already read after the request, which the stream reads first.
    buffered: Vec<u8>,
    opening: Opening,
    _open: Open,
}

/// Takes up a handed-off connection on the calling thread, with the bytes already read in front.
fn attach(connection: Detached, buffered: Vec<u8>) -> std::io::Result<Box<dyn StreamConnection>> {
    let connection = connection.attach()?;
    if buffered.is_empty() {
        return Ok(connection);
    }
    let (reader, writer) = tokio::io::split(connection);
    Ok(Box::new(tokio::io::join(
        std::io::Cursor::new(buffered).chain(reader),
        writer,
    )))
}

/// The control thread's side of the stream thread.
pub(super) struct Streams {
    shared: Arc<Shared>,
    handoffs: mpsc::UnboundedSender<Handoff>,
}

impl Streams {
    /// Starts the stream thread, which ends once these are dropped.
    pub(super) fn start() -> Self {
        let shared = Arc::new(Shared {
            open: AtomicUsize::new(0),
            closing: watch::channel(false).0,
        });
        let (handoffs, received) = mpsc::unbounded_channel();
        let thread_shared = Arc::clone(&shared);
        if let Err(error) = std::thread::Builder::new()
            .name("agentd-streams".into())
            .spawn(move || run(received, &thread_shared))
        {
            // Every stream call then fails with the reason, and calls keep working.
            tracing::error!(%error, "could not start the stream thread");
        }
        Self { shared, handoffs }
    }

    /// How many streams are open, including those still being opened.
    pub(super) fn open(&self) -> usize {
        self.shared.open.load(Ordering::Acquire)
    }

    /// Counts a stream as open from the moment its call is admitted, so calls
    /// still resolving their agent cannot together pass `most`.
    pub(super) fn reserve(&self, most: usize) -> Option<Open> {
        self.shared
            .open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < most).then_some(open + 1)
            })
            .ok()
            .map(|_| Open(Arc::clone(&self.shared)))
    }

    /// Closes every open stream and every stream opened from now on.
    pub(super) fn close(&self) {
        self.shared.closing.send_replace(true);
    }

    /// Hands `connection` to the stream thread, which opens the stream and
    /// answers the call `id` itself, with an error if it cannot open it.
    pub(super) async fn carry<S: ServedConnection>(
        &self,
        id: u64,
        connection: BufReader<S>,
        opening: Opening,
        open: Open,
    ) -> Result<(), Error> {
        let buffered = connection.buffer().to_vec();
        let handoff = Handoff {
            id,
            connection: connection.into_inner().detach()?,
            buffered,
            opening,
            _open: open,
        };
        let Err(mpsc::error::SendError(handoff)) = self.handoffs.send(handoff) else {
            return Ok(());
        };
        let mut connection = attach(handoff.connection, handoff.buffered)?;
        write_response(
            &mut connection,
            &error_response(
                id,
                CODE_INTERNAL,
                "agentd cannot carry streams, since its stream thread stopped",
            ),
            None,
        )
        .await?;
        Err(Error::Daemon("the stream thread stopped".into()))
    }
}

fn run(mut received: mpsc::UnboundedReceiver<Handoff>, shared: &Shared) {
    let runtime = match tokio::runtime::LocalRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not start the stream thread's runtime");
            return;
        }
    };
    runtime.block_on(async {
        while let Some(handoff) = received.recv().await {
            tokio::task::spawn_local(carry(handoff, shared.closing.subscribe()));
        }
    });
}

async fn carry(handoff: Handoff, mut closing: watch::Receiver<bool>) {
    let Handoff {
        id,
        connection,
        buffered,
        opening,
        _open,
    } = handoff;
    let mut connection = match attach(connection, buffered) {
        Ok(connection) => connection,
        Err(error) => {
            tracing::warn!(%error, "could not take up a stream's connection");
            return;
        }
    };
    let kind = opening.kind();
    let carried = async {
        match opening.open().await {
            Ok(stream) => {
                write_response(
                    &mut connection,
                    &result_response(id, Ok(StreamResult { stream: kind })),
                    None,
                )
                .await?;
                stream.relay(connection).await
            }
            // The connection has not switched yet, so the call fails as any other does.
            Err(error) => write_response(&mut connection, &result_response::<()>(id, Err(error)), None).await,
        }
    };
    tokio::select! {
        result = carried => {
            if let Err(error) = result {
                tracing::debug!(%error, "stream ended with an error");
            }
        }
        // Draining closes open streams rather than waiting for them, since an SSH session or a
        // forwarded connection may last for days. The upgrade warns about them first.
        _closed = closing.wait_for(|closing| *closing) => {}
    }
}
