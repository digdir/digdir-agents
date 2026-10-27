//! Guest TCP dialing transport for the Microsandbox Backend.

use sandbox_microsandbox::{GuestTcpDialer, GuestTcpStream};

use crate::Error;

/// A connection to one running agent sandbox that dials a TCP connection from
/// inside its guest.
pub struct GuestDialer(pub(super) GuestTcpDialer);

/// One open TCP stream dialed from inside an agent sandbox.
pub struct GuestConnection {
    stream: GuestTcpStream,
    // The sandbox connection the stream runs over.
    _dialer: GuestTcpDialer,
}

impl GuestDialer {
    /// Opens one TCP connection dialed from inside the guest, which keeps the
    /// dialer's sandbox connection to itself.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream cannot be opened or the guest dial is
    /// rejected.
    pub async fn connect(self, host: &str, port: u16) -> Result<GuestConnection, Error> {
        let stream = self.0.connect(host, port).await?;
        Ok(GuestConnection {
            stream,
            _dialer: self.0,
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
