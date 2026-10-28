//! Guest TCP dialing transport for the Microsandbox Backend.

use sandbox_microsandbox::{GuestTcpDialer, GuestTcpStream, GuestTcpTarget};

use crate::Error;

/// A running agent sandbox, resolved so that another thread can dial its guest.
pub struct GuestTarget(pub(super) GuestTcpTarget);

/// One open TCP stream dialed from inside an agent sandbox.
pub struct GuestConnection {
    stream: GuestTcpStream,
    // The sandbox connection the stream runs over.
    _dialer: GuestTcpDialer,
}

impl GuestTarget {
    /// Opens one TCP connection dialed from inside the guest, over a sandbox
    /// connection of its own. The calling thread's runtime reads it.
    ///
    /// # Errors
    ///
    /// Returns an error when the sandbox cannot be reached or the guest dial
    /// is rejected.
    pub async fn connect(self, host: &str, port: u16) -> Result<GuestConnection, Error> {
        let dialer = self.0.dial().await?;
        let stream = dialer.connect(host, port).await?;
        Ok(GuestConnection {
            stream,
            _dialer: dialer,
        })
    }
}

impl GuestConnection {
    /// Pipes bytes between a host reader/writer pair and the guest connection
    /// until both sides close.
    ///
    /// # Errors
    ///
    /// Returns an error when either side of the relay fails.
    pub async fn relay_io<R, W>(self, reader: R, writer: W) -> Result<(), Error>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        self.stream.relay_io(reader, writer).await.map_err(Error::from)
    }
}
