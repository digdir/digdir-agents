//! Connections from this machine into agent sandboxes, carried by agentd.
//!
//! Port forwards follow the k9s model: they live in this process, accept
//! connections on a local listener, and carry each one over a guest TCP stream
//! of its own from agentd. They end when the process exits or the forward is
//! stopped. [`relay_stdio`] carries this process's standard input and output
//! over one stream instead, behind `agentctl ssh-proxy`.

use std::{cell::RefCell, net::IpAddr, rc::Rc};

use agent::{
    AgentId, Error,
    control_api::{Client, Connection},
};
use tokio::{
    io::{AsyncWriteExt as _, BufReader},
    net::TcpListener,
};

/// Most bytes read at once in each direction, so a bulk transfer takes fewer reads and writes.
const RELAY_BUFFER_BYTES: usize = 64 * 1024;

/// One requested local-to-guest port mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForwardSpec {
    /// Local interface address accepting connections.
    pub(crate) address: IpAddr,
    /// Local port to bind; zero selects an ephemeral port.
    pub(crate) local_port: u16,
    /// Guest port that receives forwarded connections.
    pub(crate) guest_port: u16,
}

impl ForwardSpec {
    /// Parses `GUEST`, `LOCAL:GUEST`, or `ADDRESS:LOCAL:GUEST`.
    ///
    /// An empty local port (`:GUEST`) selects an ephemeral local port.
    ///
    /// # Errors
    ///
    /// Returns a message describing the malformed mapping.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let parts = text.split(':').collect::<Vec<_>>();
        let (address, local, guest) = match parts.as_slice() {
            [guest] => (None, *guest, *guest),
            [local, guest] => (None, *local, *guest),
            [address, local, guest] => (Some(*address), *local, *guest),
            _ => return Err(format!("{text:?} is not GUEST, LOCAL:GUEST, or ADDRESS:LOCAL:GUEST")),
        };
        let address = match address {
            None => IpAddr::from([127, 0, 0, 1]),
            Some(text) => text
                .parse::<IpAddr>()
                .map_err(|_| format!("{text:?} is not a local IP address"))?,
        };
        let local_port = if local.is_empty() { 0 } else { parse_port(local)? };
        let guest_port = parse_port(guest)?;
        if guest_port == 0 {
            return Err("guest port must not be zero".into());
        }
        Ok(Self {
            address,
            local_port,
            guest_port,
        })
    }
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>().map_err(|_| format!("{text:?} is not a port"))
}

/// One running forward with its local listener task.
pub(crate) struct PortForward {
    spec: ForwardSpec,
    uid: AgentId,
    local: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
    status: Rc<RefCell<Option<String>>>,
}

impl PortForward {
    /// Binds the local listener and forwards each connection to the sandbox of
    /// the agent with this name and `uid` until stopped. Once the agent is
    /// replaced by another of the same name, connections are refused, as a
    /// pod's port forward ends with the pod.
    ///
    /// # Errors
    ///
    /// Returns an error when the local address cannot be bound.
    pub(crate) async fn start(client: Rc<Client>, agent: &str, uid: AgentId, spec: ForwardSpec) -> Result<Self, Error> {
        let listener = TcpListener::bind((spec.address, spec.local_port))
            .await
            .map_err(Error::from)?;
        let local = listener.local_addr().map_err(Error::from)?;
        let status = Rc::new(RefCell::new(None));
        let task = tokio::task::spawn_local(accept_loop(
            client,
            agent.into(),
            uid,
            spec.guest_port,
            listener,
            Rc::clone(&status),
        ));
        Ok(Self {
            spec,
            uid,
            local,
            task,
            status,
        })
    }

    /// Returns the requested mapping.
    #[must_use]
    pub(crate) const fn spec(&self) -> &ForwardSpec {
        &self.spec
    }

    /// Returns the uid of the agent the forward reaches.
    #[must_use]
    pub(crate) const fn uid(&self) -> AgentId {
        self.uid
    }

    /// Returns the bound local address, with any ephemeral port resolved.
    #[must_use]
    pub(crate) const fn local_address(&self) -> std::net::SocketAddr {
        self.local
    }

    /// Returns the most recent connection failure, when one occurred.
    #[must_use]
    pub(crate) fn status(&self) -> Option<String> {
        self.status.borrow().clone()
    }

    /// Reports whether the listener task has ended and stopped serving.
    #[must_use]
    pub(crate) fn finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Stops the listener and drops in-flight relays.
    pub(crate) fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(
    client: Rc<Client>,
    agent: Rc<str>,
    uid: AgentId,
    guest_port: u16,
    listener: TcpListener,
    status: Rc<RefCell<Option<String>>>,
) {
    // Relays live in the accept task's JoinSet, so aborting the accept task
    // drops the set and aborts every in-flight connection with it.
    let mut relays = tokio::task::JoinSet::new();
    loop {
        while relays.try_join_next().is_some() {}
        let local = match listener.accept().await {
            Ok((local, _)) => local,
            Err(error) => {
                *status.borrow_mut() = Some(format!("accept failed: {error}"));
                return;
            }
        };
        let (client, agent, status) = (Rc::clone(&client), Rc::clone(&agent), Rc::clone(&status));
        // Each connection opens its stream in its own task, so a slow one does not hold up the next.
        relays.spawn_local(async move {
            let relayed = async {
                let stream = client.guest_tcp(&agent, Some(uid), guest_port).await?;
                *status.borrow_mut() = None;
                relay(local, stream).await
            };
            if let Err(error) = relayed.await {
                // agentd's message is the whole story, as for any refused call.
                let message = match error {
                    Error::Rpc(error) => error.message,
                    error => error.to_string(),
                };
                *status.borrow_mut() = Some(message);
            }
        });
    }
}

/// Carries one local TCP connection over a guest TCP stream until both
/// directions have closed.
///
/// A clean end of one direction lets the other run to its own end, as a
/// half-closed TCP connection does. An error in either direction ends both,
/// since the other may wait forever on a peer that sends nothing.
async fn relay(local: tokio::net::TcpStream, stream: Box<dyn Connection>) -> Result<(), Error> {
    let (reader, mut writer) = local.into_split();
    let (from_guest, mut to_guest) = tokio::io::split(stream);
    let mut from_guest = BufReader::with_capacity(RELAY_BUFFER_BYTES, from_guest);
    let mut reader = BufReader::with_capacity(RELAY_BUFFER_BYTES, reader);
    let download = async {
        tokio::io::copy_buf(&mut from_guest, &mut writer).await?;
        writer.shutdown().await
    };
    let upload = async {
        tokio::io::copy_buf(&mut reader, &mut to_guest).await?;
        to_guest.shutdown().await
    };
    tokio::pin!(download, upload);
    let (mut downloaded, mut uploaded) = (false, false);
    while !(downloaded && uploaded) {
        tokio::select! {
            result = &mut download, if !downloaded => {
                result?;
                downloaded = true;
            }
            result = &mut upload, if !uploaded => match result {
                Err(error) if !ends_stream(&error) => return Err(error.into()),
                Ok(()) | Err(_) => uploaded = true,
            },
        }
    }
    Ok(())
}

/// Whether a failed write to agentd is the end of the stream rather than a
/// failure. agentd closes the stream once the guest connection has ended, so
/// a write it no longer takes is that end, which the other direction then
/// reaches too.
fn ends_stream(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
    )
}

/// Carries this process's standard input and output over a guest TCP stream
/// until the guest's end of it closes.
///
/// Standard output cannot be half-closed, so its reader would never learn
/// that the guest finished while this process waited for the end of standard
/// input. The relay ends with the guest's output instead, or with an error in
/// either direction.
///
/// # Errors
///
/// Returns an error when reading the stream or standard input, or writing
/// standard output, fails.
pub(crate) async fn relay_stdio(stream: Box<dyn Connection>) -> Result<(), Error> {
    let (from_guest, mut to_guest) = tokio::io::split(stream);
    let mut from_guest = BufReader::with_capacity(RELAY_BUFFER_BYTES, from_guest);
    let mut input = BufReader::with_capacity(RELAY_BUFFER_BYTES, tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let download = async {
        tokio::io::copy_buf(&mut from_guest, &mut output).await?;
        output.flush().await
    };
    let upload = async {
        tokio::io::copy_buf(&mut input, &mut to_guest).await?;
        to_guest.shutdown().await
    };
    tokio::pin!(download);
    tokio::select! {
        result = &mut download => result?,
        uploaded = upload => match uploaded {
            Err(error) if !ends_stream(&error) => return Err(error.into()),
            Ok(()) | Err(_) => download.await?,
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    /// A stream from agentd whose reads fail, and which takes every write.
    struct FailingStream;

    impl tokio::io::AsyncRead for FailingStream {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            _buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("the stream failed")))
        }
    }

    impl tokio::io::AsyncWrite for FailingStream {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            buffer: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_download_ends_a_connection_that_sends_nothing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().expect("address"))
            .await
            .expect("connect");
        let (local, _) = listener.accept().await.expect("accept");

        let relayed = tokio::time::timeout(std::time::Duration::from_secs(2), relay(local, Box::new(FailingStream)))
            .await
            .expect("the relay ends instead of waiting for the client");
        assert!(relayed.is_err(), "the failure reaches the forward's status");
        let mut rest = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut client, &mut rest)
            .await
            .expect("the local connection closes");
    }

    #[test]
    fn specs_follow_kubectl_shapes() {
        assert_eq!(
            ForwardSpec::parse("80").expect("guest-only spec"),
            ForwardSpec {
                address: IpAddr::from([127, 0, 0, 1]),
                local_port: 80,
                guest_port: 80,
            }
        );
        assert_eq!(
            ForwardSpec::parse("9090:80").expect("local and guest spec"),
            ForwardSpec {
                address: IpAddr::from([127, 0, 0, 1]),
                local_port: 9090,
                guest_port: 80,
            }
        );
        assert_eq!(
            ForwardSpec::parse("0.0.0.0:80:80").expect("address spec"),
            ForwardSpec {
                address: IpAddr::from([0, 0, 0, 0]),
                local_port: 80,
                guest_port: 80,
            }
        );
        assert_eq!(
            ForwardSpec::parse(":80").expect("ephemeral local port"),
            ForwardSpec {
                address: IpAddr::from([127, 0, 0, 1]),
                local_port: 0,
                guest_port: 80,
            }
        );
        assert!(ForwardSpec::parse("web:80").is_err());
        assert!(ForwardSpec::parse("1:2:3:4").is_err());
        assert!(ForwardSpec::parse("8080:0").is_err());
    }
}
