//! Host-dialed TCP streams into a running Microsandbox guest.
//!
//! The Microsandbox agent relay multiplexes `TcpConnect` streams over the
//! sandbox's host socket, so a host process can open TCP connections that are
//! dialed from inside the guest network namespace without any configuration
//! on the sandbox itself.
//!
//! A connection's data travels on the relay's bulk lane when the runtime
//! offers one. Inline data shares the single ordered control lane with every
//! other operation on the sandbox, so one busy download would delay the
//! keystrokes of every SSH session and terminal behind it. The bulk lane takes
//! turns between connections and bounds what each has in flight by a credit
//! window. Older runtimes keep the inline frames.

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use bytes::Bytes;
use microsandbox::{
    agent::AgentClient,
    protocol::{
        bulk::{
            BULK_FLOW_MASK_GUEST_TO_HOST, BULK_FLOW_MASK_HOST_TO_GUEST, BulkAccepted, BulkCancel, BulkCancelReason,
            BulkCredit, BulkFinish, BulkFlow, BulkKind, BulkOffer, BulkReceiveState, BulkRecord, BulkSendState,
        },
        message::{Message, MessageType},
        tcp::{TcpClose, TcpConnect, TcpConnected, TcpData, TcpEof, TcpFailed},
    },
};
use microsandbox_agent_client::AgentFrame;
use sandbox::{Error, SandboxId};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{MicrosandboxProvider, error};

/// Most bytes read from the host at once for a connection whose data travels inline.
const RELAY_READ_BUFFER_BYTES: usize = 32 * 1024;

/// How long connecting to a sandbox's runtime may take, as the Microsandbox SDK allows.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a bulk connection waits, once both ends have finished, for the
/// guest to write the last of what it received and close the connection.
const BULK_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Where a running sandbox's runtime listens, resolved from the provider's
/// state so that any thread can dial it.
///
/// It holds no connection. The thread that calls [`Self::dial`] owns the
/// connection, and that thread's runtime reads every byte from it, so a
/// thread that carries guest connections never waits on the provider's own.
pub struct GuestTcpTarget {
    backend: Arc<microsandbox::LocalBackend>,
    runtime_name: String,
}

/// Dials TCP connections from inside one running sandbox.
///
/// The dialer keeps a single multiplexed agent connection, so opening many
/// concurrent streams through one dialer is cheap. It stops working when the
/// sandbox runtime restarts; dial its target again when a connect fails.
pub struct GuestTcpDialer {
    client: Arc<AgentClient>,
}

/// One open TCP stream dialed from inside the guest.
pub struct GuestTcpStream {
    id: u32,
    client: Arc<AgentClient>,
    receiver: tokio::sync::mpsc::Receiver<AgentFrame>,
    /// Whether the connection's data travels on the bulk lane.
    on_bulk_lane: bool,
    /// The connection's bulk-lane flows until a relay takes them.
    bulk: Option<BulkFlows>,
    /// Whether the guest has ended the connection, so nothing is left to release.
    closed: bool,
}

/// The two bulk-lane flows of one connection. Each flow's credit window bounds
/// the bytes its sender may have in flight before the receiver has taken them.
struct BulkFlows {
    to_guest: BulkSendState,
    from_guest: BulkReceiveState,
}

impl MicrosandboxProvider {
    /// Resolves where one running sandbox's runtime listens.
    ///
    /// # Errors
    ///
    /// Returns an error when the sandbox is unknown or not running.
    pub async fn guest_tcp_target(&self, id: &SandboxId) -> Result<GuestTcpTarget, Error> {
        let record = self.state.sandbox_by_id(id).await?;
        let runtime = self
            .runtime_handle(&record.runtime_name)
            .await?
            .ok_or_else(|| Error::not_found(sandbox::ResourceKind::Sandbox, &record.id))?;
        if !matches!(
            runtime.status_snapshot(),
            microsandbox::sandbox::SandboxStatus::Running | microsandbox::sandbox::SandboxStatus::Draining
        ) {
            return Err(error::microsandbox(microsandbox::MicrosandboxError::SandboxNotRunning(
                format!("sandbox {} is not running", record.id),
            )));
        }
        Ok(GuestTcpTarget {
            backend: self.client.shared(),
            runtime_name: record.runtime_name,
        })
    }
}

impl GuestTcpTarget {
    /// Connects a dialer to the sandbox's runtime, read by the calling thread's runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime cannot be reached, or predates
    /// agent-relay TCP support.
    pub async fn dial(self) -> Result<GuestTcpDialer, Error> {
        use microsandbox::Backend as _;

        let client = self
            .backend
            .dial_agent(&self.runtime_name, CONNECT_TIMEOUT)
            .await
            .map_err(error::microsandbox)?;
        if !client.supports(MessageType::TcpConnect) {
            return Err(Error::Backend(
                "Sandbox runtime does not support agent-relay TCP forwarding; restart the sandbox".into(),
            ));
        }
        Ok(GuestTcpDialer {
            client: Arc::new(client),
        })
    }
}

impl GuestTcpDialer {
    /// Opens one TCP connection dialed from inside the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the relay stream cannot be opened or the guest
    /// dial is rejected.
    pub async fn connect(&self, host: &str, port: u16) -> Result<GuestTcpStream, Error> {
        GuestTcpStream::open(Arc::clone(&self.client), host, port).await
    }
}

impl GuestTcpStream {
    async fn open(client: Arc<AgentClient>, host: &str, port: u16) -> Result<Self, Error> {
        let offer = client.supports(MessageType::BulkAccepted).then(BulkOffer::tcp);
        let request = TcpConnect {
            host: host.to_owned(),
            port,
            bulk: offer,
        };
        let (id, mut receiver) = client
            .stream_frames(MessageType::TcpConnect, &request)
            .await
            .map_err(error::backend)?;
        let first = next_control(&mut receiver, "replying to connect").await?;
        match first.t {
            MessageType::TcpConnected => {
                let _: TcpConnected = first.payload().map_err(error::backend)?;
                let bulk = match offer {
                    Some(offer) => Some(BulkFlows::accept(
                        &next_control(&mut receiver, "accepting the bulk lane").await?,
                        offer,
                    )?),
                    None => None,
                };
                Ok(Self {
                    id,
                    client,
                    receiver,
                    on_bulk_lane: bulk.is_some(),
                    bulk,
                    closed: false,
                })
            }
            MessageType::TcpFailed => {
                let failed: TcpFailed = first.payload().map_err(error::backend)?;
                Err(Error::Backend(format!(
                    "guest TCP connect to {host}:{port} failed: {}",
                    failed.error
                )))
            }
            other => Err(Error::Backend(format!(
                "unexpected sandbox agent reply {:?} to guest TCP connect",
                other.as_str()
            ))),
        }
    }
}

/// Reads the next control message of a connection that is still being opened.
async fn next_control(receiver: &mut tokio::sync::mpsc::Receiver<AgentFrame>, awaited: &str) -> Result<Message, Error> {
    match receiver.recv().await {
        Some(AgentFrame::Control(message)) => Ok(message),
        Some(AgentFrame::Bulk(_)) => Err(Error::Backend(format!("Sandbox agent sent TCP data before {awaited}"))),
        None => Err(Error::Backend(format!(
            "Sandbox agent closed the TCP stream before {awaited}"
        ))),
    }
}

impl BulkFlows {
    fn accept(message: &Message, offer: BulkOffer) -> Result<Self, Error> {
        if message.t != MessageType::BulkAccepted {
            return Err(Error::Backend(format!(
                "unexpected sandbox agent reply {:?} to a TCP bulk offer",
                message.t.as_str()
            )));
        }
        let accepted = message
            .payload::<BulkAccepted>()
            .map_err(error::backend)?
            .validate_against(
                offer,
                BulkKind::Tcp,
                BULK_FLOW_MASK_HOST_TO_GUEST | BULK_FLOW_MASK_GUEST_TO_HOST,
            )
            .map_err(|source| Error::Backend(format!("invalid TCP bulk acceptance: {source}")))?;
        let to_guest = BulkSendState::new(
            BulkKind::Tcp,
            BulkFlow::HostToGuest,
            accepted.max_record_payload,
            accepted.host_to_guest_credit_limit,
        )
        .map_err(error::backend)?;
        let from_guest = BulkReceiveState::new(
            BulkKind::Tcp,
            BulkFlow::GuestToHost,
            accepted.max_record_payload,
            accepted.guest_to_host_credit_limit,
            offer.guest_to_host_credit_limit,
        )
        .map_err(error::backend)?;
        Ok(Self { to_guest, from_guest })
    }
}

impl Drop for GuestTcpStream {
    /// Releases the guest socket and its agent session on every path that
    /// leaves it open: an error, and a cancelled relay task alike.
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let client = Arc::clone(&self.client);
        let id = self.id;
        let bulk = self.on_bulk_lane;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                // A bulk correlation ends with a cancellation, which also discards its records
                // still in flight.
                if bulk {
                    let cancel = BulkCancel {
                        kind: BulkKind::Tcp,
                        reason: BulkCancelReason::CallerCancelled,
                        message: "the host closed the forwarded connection".into(),
                    };
                    let _ = client.cancel_bulk(id, &cancel).await;
                } else {
                    let _ = client.send(id, MessageType::TcpClose, &TcpClose {}).await;
                }
            });
        }
    }
}

impl GuestTcpStream {
    /// Pipes bytes between a host socket and the guest connection until both
    /// directions have closed; dropping the stream releases the guest session.
    ///
    /// # Errors
    ///
    /// Returns an error when a host socket read or write fails, or a relay
    /// message cannot be sent or decoded.
    pub async fn relay(self, stream: tokio::net::TcpStream) -> Result<(), Error> {
        let (host_reader, host_writer) = stream.into_split();
        self.relay_io(host_reader, host_writer).await
    }

    /// Pipes bytes between an arbitrary host byte stream pair, such as a
    /// process's standard input and output, and the guest connection until
    /// both directions have closed; dropping the stream releases the guest
    /// session.
    ///
    /// # Errors
    ///
    /// Returns an error when a host read or write fails, or a relay message
    /// cannot be sent or decoded.
    pub async fn relay_io<R, W>(mut self, host_reader: R, host_writer: W) -> Result<(), Error>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let (result, terminated) = match self.bulk.take() {
            Some(flows) => self.relay_bulk(flows, host_reader, host_writer).await,
            None => self.relay_inline(host_reader, host_writer).await,
        };
        self.closed = terminated;
        result
    }

    /// Relays a connection whose data travels inline, and returns whether the
    /// guest ended it with a terminal frame.
    ///
    /// Only runtimes without the bulk lane carry data inline, and they send no
    /// terminal frame after a mutual half-close, so the relay ends then. They
    /// write everything the host sent before acting on the close that follows.
    async fn relay_inline<R, W>(&mut self, mut host_reader: R, mut host_writer: W) -> (Result<(), Error>, bool)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let client = Arc::clone(&self.client);
        let id = self.id;
        let credit = tokio::sync::Notify::new();
        let host_closed = Cell::new(false);
        let guest_closed = Cell::new(false);
        let terminated = Cell::new(false);

        let host_to_guest = pump_to_guest(&client, id, &mut host_reader, None, &credit, &host_closed);

        let guest_to_host = async {
            while let Some(frame) = self.receiver.recv().await {
                let AgentFrame::Control(message) = frame else {
                    return Err(Error::Backend(
                        "Sandbox agent sent bulk TCP data on a connection that did not accept bulk".into(),
                    ));
                };
                match message.t {
                    MessageType::TcpData => {
                        let data: TcpData = message.payload().map_err(error::backend)?;
                        write_to_host(&mut host_writer, &data.data, self.receiver.is_empty()).await?;
                    }
                    MessageType::TcpEof => {
                        host_writer
                            .shutdown()
                            .await
                            .map_err(|source| error::io("shut down forwarded host connection", source))?;
                        guest_closed.set(true);
                        if host_closed.get() {
                            return Ok(());
                        }
                    }
                    other => return terminal_or_unexpected(other, &terminated),
                }
            }
            terminated.set(true);
            Ok(())
        };

        // A host-side EOF is a half-close: the guest may still be writing its
        // response, so the relay ends when the guest side has closed too.
        tokio::pin!(guest_to_host);
        let result = tokio::select! {
            result = &mut guest_to_host => result,
            result = host_to_guest => match result {
                Ok(()) if guest_closed.get() => Ok(()),
                Ok(()) => guest_to_host.await,
                Err(error) => Err(error),
            },
        };
        (result, terminated.get())
    }

    /// Relays a connection on the bulk lane, and returns whether the guest
    /// ended it with a terminal frame.
    ///
    /// The guest sends one once both directions have finished and it has
    /// written everything it received, so the relay waits for it rather than
    /// cancelling records still on their way. Frames from the guest are read
    /// apart from writing its data to the host: a host that does not read
    /// until its own upload is sent would otherwise stop the credit that
    /// upload waits for.
    async fn relay_bulk<R, W>(
        &mut self,
        flows: BulkFlows,
        mut host_reader: R,
        mut host_writer: W,
    ) -> (Result<(), Error>, bool)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let client = Arc::clone(&self.client);
        let id = self.id;
        let to_guest = RefCell::new(flows.to_guest);
        let from_guest = RefCell::new(flows.from_guest);
        let credit = tokio::sync::Notify::new();
        let host_closed = Cell::new(false);
        let terminated = Cell::new(false);
        // Keeps its permit when the guest finishes before the host does.
        let guest_finished = tokio::sync::Notify::new();
        // Unbounded in records, while the receive window bounds its bytes: the
        // guest sends no more than its credit, which returns only once the
        // host has taken the bytes.
        let (output, mut queued) = tokio::sync::mpsc::unbounded_channel::<GuestOutput>();

        let host_to_guest = pump_to_guest(&client, id, &mut host_reader, Some(&to_guest), &credit, &host_closed);

        let receiver = &mut self.receiver;
        let frames = async {
            let result = async {
                while let Some(frame) = receiver.recv().await {
                    let message = match frame {
                        AgentFrame::Bulk(record) => {
                            let end = from_guest.borrow_mut().accept_record(&record).map_err(error::backend)?;
                            let _ = output.send(GuestOutput::Data(record.payload, end));
                            continue;
                        }
                        AgentFrame::Control(message) => message,
                    };
                    match message.t {
                        MessageType::BulkCredit => {
                            let update: BulkCredit = message.payload().map_err(error::backend)?;
                            to_guest.borrow_mut().apply_credit(update).map_err(error::backend)?;
                            credit.notify_waiters();
                        }
                        MessageType::BulkFinish => {
                            let finish: BulkFinish = message.payload().map_err(error::backend)?;
                            from_guest.borrow_mut().accept_finish(finish).map_err(error::backend)?;
                            let _ = output.send(GuestOutput::Finished);
                            guest_finished.notify_one();
                        }
                        other => return terminal_or_unexpected(other, &terminated),
                    }
                }
                terminated.set(true);
                Ok(())
            }
            .await;
            // The host still gets what is queued before a terminal frame.
            drop(output);
            result
        };

        let deliver = async {
            while let Some(queued_output) = queued.recv().await {
                match queued_output {
                    GuestOutput::Data(payload, end) => {
                        write_to_host(&mut host_writer, &payload, queued.is_empty()).await?;
                        // Credit covers only bytes the host has taken, so a slow host reader holds the
                        // guest back instead of filling memory here.
                        let update = from_guest.borrow_mut().consume(end).map_err(error::backend)?;
                        if let Some(update) = update {
                            client
                                .send(id, MessageType::BulkCredit, &update)
                                .await
                                .map_err(error::backend)?;
                        }
                    }
                    GuestOutput::Finished => host_writer
                        .shutdown()
                        .await
                        .map_err(|source| error::io("shut down forwarded host connection", source))?,
                }
            }
            Ok::<(), Error>(())
        };

        let guest_to_host = async {
            tokio::pin!(frames, deliver);
            let mut guest_done = false;
            loop {
                tokio::select! {
                    result = &mut frames, if !guest_done => {
                        result?;
                        guest_done = true;
                    }
                    result = &mut deliver => return result,
                }
            }
        };

        tokio::pin!(guest_to_host);
        let result = tokio::select! {
            result = &mut guest_to_host => result,
            result = host_to_guest => match result {
                // A guest whose destination never takes the last of the upload would hold the
                // connection after both ends have finished, so it gets a grace period before the
                // connection is cancelled.
                Ok(()) => tokio::select! {
                    result = &mut guest_to_host => result,
                    () = async {
                        guest_finished.notified().await;
                        tokio::time::sleep(BULK_CLOSE_GRACE).await;
                    } => Err(Error::Backend(format!(
                        "the guest did not finish writing a forwarded connection's input within {}s of its end",
                        BULK_CLOSE_GRACE.as_secs()
                    ))),
                },
                Err(error) => Err(error),
            },
        };
        (result, terminated.get())
    }
}

/// Guest data queued for the host, in the order the guest sent it.
enum GuestOutput {
    /// Bytes, with the offset their delivery returns credit up to.
    Data(Bytes, u64),
    /// The guest's end of stream.
    Finished,
}

/// Ends a relay on a frame that is not data: cleanly for the guest's terminal
/// frame, with an error for a failure or a frame that does not belong.
fn terminal_or_unexpected(message: MessageType, terminated: &Cell<bool>) -> Result<(), Error> {
    match message {
        MessageType::TcpClosed => {
            terminated.set(true);
            Ok(())
        }
        MessageType::TcpFailed | MessageType::BulkCancel => {
            terminated.set(true);
            Err(Error::Backend(format!(
                "Sandbox agent ended the guest TCP stream with {:?}",
                message.as_str()
            )))
        }
        other => Err(Error::Backend(format!(
            "unexpected sandbox agent message {:?} on a guest TCP stream",
            other.as_str()
        ))),
    }
}

/// Sends what the host writes to the guest until the host's end of stream,
/// which it passes on and records in `host_closed`.
async fn pump_to_guest<R: AsyncRead + Unpin>(
    client: &AgentClient,
    id: u32,
    host_reader: &mut R,
    to_guest: Option<&RefCell<BulkSendState>>,
    credit: &tokio::sync::Notify,
    host_closed: &Cell<bool>,
) -> Result<(), Error> {
    let read_size = to_guest.map_or(RELAY_READ_BUFFER_BYTES, |flow| {
        usize::try_from(flow.borrow().max_record_payload()).unwrap_or(RELAY_READ_BUFFER_BYTES)
    });
    let mut buffer = vec![0u8; read_size];
    loop {
        let read = host_reader
            .read(&mut buffer)
            .await
            .map_err(|source| error::io("read forwarded host connection", source))?;
        if read == 0 {
            match to_guest {
                Some(flow) => {
                    let finish = flow.borrow_mut().finish().map_err(error::backend)?;
                    client
                        .send(id, MessageType::BulkFinish, &finish)
                        .await
                        .map_err(error::backend)?;
                }
                None => client
                    .send(id, MessageType::TcpEof, &TcpEof {})
                    .await
                    .map_err(error::backend)?,
            }
            host_closed.set(true);
            return Ok(());
        }
        match to_guest {
            Some(flow) => send_bulk(client, id, flow, credit, &buffer[..read]).await?,
            None => client
                .send(
                    id,
                    MessageType::TcpData,
                    &TcpData {
                        data: buffer[..read].to_vec(),
                    },
                )
                .await
                .map_err(error::backend)?,
        }
    }
}

/// Writes guest data to the host, flushing once the guest has nothing more queued.
///
/// A buffered writer such as `tokio::io::stdout()`, used by `ssh-proxy`, holds
/// bytes until the buffer fills, so an interactive protocol would stall
/// waiting for a reply that is sitting unflushed. Flushing only when nothing
/// else is queued keeps a bulk transfer batched.
async fn write_to_host<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8], drained: bool) -> Result<(), Error> {
    writer
        .write_all(data)
        .await
        .map_err(|source| error::io("write forwarded host connection", source))?;
    if drained {
        writer
            .flush()
            .await
            .map_err(|source| error::io("flush forwarded host connection", source))?;
    }
    Ok(())
}

/// Sends host data to the guest as bulk records, waiting for credit whenever
/// the guest has not yet taken what the window allows in flight.
async fn send_bulk(
    client: &AgentClient,
    id: u32,
    flow: &RefCell<BulkSendState>,
    credit: &tokio::sync::Notify,
    data: &[u8],
) -> Result<(), Error> {
    let mut remaining = Bytes::copy_from_slice(data);
    while !remaining.is_empty() {
        // Registered before credit is read, so an update between the two still wakes this.
        let granted = credit.notified();
        tokio::pin!(granted);
        granted.as_mut().enable();
        let next = {
            let mut flow = flow.borrow_mut();
            let available = usize::try_from(flow.available_credit()).unwrap_or(usize::MAX);
            let max_record = usize::try_from(flow.max_record_payload()).unwrap_or(usize::MAX);
            let len = remaining.len().min(max_record).min(available);
            if len == 0 {
                None
            } else {
                Some((flow.admit(len).map_err(error::backend)?, len))
            }
        };
        let Some((offset, len)) = next else {
            granted.await;
            continue;
        };
        client
            .send_bulk(BulkRecord {
                id,
                kind: BulkKind::Tcp,
                flow: BulkFlow::HostToGuest,
                offset,
                payload: remaining.split_to(len),
            })
            .await
            .map_err(error::backend)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use std::{sync::Arc, time::Duration};

    use bytes::{Bytes, BytesMut};
    use microsandbox::{
        agent::AgentClient,
        protocol::{
            bulk::{
                BULK_FLOW_MASK_GUEST_TO_HOST, BULK_FLOW_MASK_HOST_TO_GUEST, BULK_FORMAT_RAW_V1, BulkAccepted,
                BulkCredit, BulkFinish, BulkFlow, BulkKind, BulkRecord,
            },
            codec::{self, DecodedFrame},
            core::Ready,
            message::{Message, MessageType},
            tcp::{TcpClosed, TcpConnect, TcpConnected, TcpEof},
        },
    };
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream},
        net::{TcpListener, TcpStream},
    };

    use super::GuestTcpStream;

    const RELAY_ID_MIN: u32 = 1;
    const RELAY_ID_MAX: u32 = 1 << 20;
    /// The last protocol generation without the bulk lane.
    const GENERATION_WITHOUT_BULK: u8 = 7;

    /// The guest end of the agent relay, driven by the test one frame at a time.
    struct FakeGuestAgent {
        wire: DuplexStream,
        pending: BytesMut,
    }

    impl FakeGuestAgent {
        /// Connects a host client to a guest speaking `generation`, or the current one.
        async fn handshake(generation: Option<u8>) -> (Arc<AgentClient>, Self) {
            let (host, guest) = tokio::io::duplex(64 * 1024);
            let mut agent = Self {
                wire: guest,
                pending: BytesMut::new(),
            };
            agent
                .wire
                .write_all(&RELAY_ID_MIN.to_be_bytes())
                .await
                .expect("write relay id range start");
            agent
                .wire
                .write_all(&RELAY_ID_MAX.to_be_bytes())
                .await
                .expect("write relay id range end");
            let mut ready = Message::with_payload(MessageType::Ready, 0, &Ready::default()).expect("ready frame");
            if let Some(generation) = generation {
                ready.v = generation;
            }
            agent.write(&ready).await;
            let client = AgentClient::connect_stream_with_timeout(host, Duration::from_secs(5))
                .await
                .expect("relay handshake");
            (Arc::new(client), agent)
        }

        async fn write(&mut self, message: &Message) {
            let mut frame = Vec::new();
            codec::encode_to_buf(message, &mut frame).expect("encode frame");
            self.wire.write_all(&frame).await.expect("write frame");
        }

        async fn reply<T: serde::Serialize>(&mut self, t: MessageType, id: u32, payload: &T) {
            self.write(&Message::with_payload(t, id, payload).expect("reply frame"))
                .await;
        }

        async fn write_bulk(&mut self, record: &BulkRecord) {
            let mut frame = Vec::new();
            codec::encode_bulk_to_buf(record, &mut frame).expect("encode bulk record");
            self.wire.write_all(&frame).await.expect("write bulk record");
        }

        async fn next(&mut self) -> DecodedFrame {
            loop {
                if let Some(frame) = codec::try_decode_frame_from_bytes(&mut self.pending).expect("decode frame") {
                    return frame;
                }
                let mut chunk = [0u8; 4096];
                let read = self.wire.read(&mut chunk).await.expect("read frame");
                assert!(read > 0, "host closed the relay");
                self.pending.extend_from_slice(&chunk[..read]);
            }
        }

        async fn expect(&mut self, expected: MessageType) -> Message {
            match self.next().await {
                DecodedFrame::Control(message) => {
                    assert_eq!(message.t, expected, "unexpected frame from the host relay");
                    message
                }
                DecodedFrame::Bulk(record) => panic!("expected {expected:?}, got bulk record {record:?}"),
            }
        }

        /// Asserts that the host sends nothing more for a while, or closes the relay without.
        async fn expect_silence(&mut self) {
            let silent = tokio::time::timeout(Duration::from_millis(200), async {
                loop {
                    if let Some(frame) = codec::try_decode_frame_from_bytes(&mut self.pending).expect("decode frame") {
                        return Some(frame);
                    }
                    let mut chunk = [0u8; 4096];
                    let read = self.wire.read(&mut chunk).await.expect("read frame");
                    if read == 0 {
                        return None;
                    }
                    self.pending.extend_from_slice(&chunk[..read]);
                }
            })
            .await;
            if let Ok(Some(frame)) = silent {
                let frame = match frame {
                    DecodedFrame::Control(message) => format!("{:?}", message.t),
                    DecodedFrame::Bulk(record) => format!("a bulk record at {}", record.offset),
                };
                panic!("the host sent {frame} after the connection ended");
            }
        }

        async fn expect_bulk(&mut self) -> BulkRecord {
            match self.next().await {
                DecodedFrame::Bulk(record) => record,
                DecodedFrame::Control(message) => panic!("expected a bulk record, got {:?}", message.t),
            }
        }
    }

    /// A forwarded connection: the browser end the test drives, and the end the relay serves.
    async fn host_connection() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind host listener");
        let browser = TcpStream::connect(listener.local_addr().expect("listener address"))
            .await
            .expect("connect browser side");
        let (forwarded, _) = listener.accept().await.expect("accept forwarded connection");
        (browser, forwarded)
    }

    #[tokio::test(flavor = "local")]
    async fn an_older_runtime_keeps_tcp_data_inline() {
        let (client, mut agent) = FakeGuestAgent::handshake(Some(GENERATION_WITHOUT_BULK)).await;
        let (opened, ()) = tokio::join!(GuestTcpStream::open(client, "127.0.0.1", 80), async {
            let connect = agent.expect(MessageType::TcpConnect).await;
            let request: TcpConnect = connect.payload().expect("connect request");
            assert_eq!(request.bulk, None, "no bulk lane is offered to a runtime without one");
            agent
                .reply(MessageType::TcpConnected, connect.id, &TcpConnected {})
                .await;
        });
        let stream = opened.expect("guest connect should succeed");
        let session = stream.id;

        let (mut browser, forwarded) = host_connection().await;
        let relay = tokio::task::spawn_local(stream.relay(forwarded));

        browser.shutdown().await.expect("half-close the browser side");
        agent.expect(MessageType::TcpEof).await;

        agent.reply(MessageType::TcpEof, session, &TcpEof {}).await;
        let mut sink = [0u8; 1];
        let read = browser
            .read(&mut sink)
            .await
            .expect("read guest EOF on the browser side");
        assert_eq!(read, 0, "guest EOF should half-close the browser side");

        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .expect("relay should finish once both directions have closed")
            .expect("relay task should not panic")
            .expect("relay should end cleanly");
        let close = agent.expect(MessageType::TcpClose).await;
        assert_eq!(
            close.id, session,
            "dropping the finished relay should release the guest session"
        );
    }

    #[tokio::test(flavor = "local")]
    async fn data_crosses_the_bulk_lane_within_its_credit() {
        const HOST_TO_GUEST_CREDIT: u64 = 8;

        let (client, mut agent) = FakeGuestAgent::handshake(None).await;
        let (opened, ()) = tokio::join!(GuestTcpStream::open(client, "127.0.0.1", 80), async {
            let connect = agent.expect(MessageType::TcpConnect).await;
            let request: TcpConnect = connect.payload().expect("connect request");
            let offer = request.bulk.expect("a bulk lane is offered to a runtime that has one");
            agent
                .reply(MessageType::TcpConnected, connect.id, &TcpConnected {})
                .await;
            let accepted = BulkAccepted {
                kind: BulkKind::Tcp,
                flows: BULK_FLOW_MASK_HOST_TO_GUEST | BULK_FLOW_MASK_GUEST_TO_HOST,
                format: BULK_FORMAT_RAW_V1,
                max_record_payload: offer.max_record_payload,
                host_to_guest_credit_limit: HOST_TO_GUEST_CREDIT,
                guest_to_host_credit_limit: offer.guest_to_host_credit_limit,
            };
            agent.reply(MessageType::BulkAccepted, connect.id, &accepted).await;
        });
        let stream = opened.expect("guest connect should succeed");
        let session = stream.id;

        let (mut browser, forwarded) = host_connection().await;
        let relay = tokio::task::spawn_local(stream.relay(forwarded));

        browser.write_all(b"hello world").await.expect("browser request");
        let first = agent.expect_bulk().await;
        assert_eq!(
            (first.offset, &first.payload[..]),
            (0, &b"hello wo"[..]),
            "only the credited bytes"
        );
        agent
            .reply(
                MessageType::BulkCredit,
                session,
                &BulkCredit {
                    kind: BulkKind::Tcp,
                    flow: BulkFlow::HostToGuest,
                    consumed_offset: HOST_TO_GUEST_CREDIT,
                    credit_limit: 64,
                },
            )
            .await;
        let rest = agent.expect_bulk().await;
        assert_eq!(
            (rest.offset, &rest.payload[..]),
            (8, &b"rld"[..]),
            "the rest once credited"
        );

        agent
            .write_bulk(&BulkRecord {
                id: session,
                kind: BulkKind::Tcp,
                flow: BulkFlow::GuestToHost,
                offset: 0,
                payload: Bytes::from_static(b"reply"),
            })
            .await;
        let mut answer = [0u8; 5];
        browser.read_exact(&mut answer).await.expect("guest reply");
        assert_eq!(&answer, b"reply");

        // The guest finishes its output while the host's upload is still on its way.
        agent
            .reply(
                MessageType::BulkFinish,
                session,
                &BulkFinish {
                    kind: BulkKind::Tcp,
                    flow: BulkFlow::GuestToHost,
                    final_offset: 5,
                },
            )
            .await;
        let mut sink = [0u8; 1];
        assert_eq!(browser.read(&mut sink).await.expect("guest EOF"), 0);
        browser.write_all(b"!").await.expect("the rest of the upload");
        browser.shutdown().await.expect("half-close the browser side");
        let last = agent.expect_bulk().await;
        assert_eq!(
            (last.offset, &last.payload[..]),
            (11, &b"!"[..]),
            "the whole upload arrives"
        );
        let finish: BulkFinish = agent.expect(MessageType::BulkFinish).await.payload().expect("finish");
        assert_eq!(finish.final_offset, 12, "the host's end follows all of its data");
        agent.expect_silence().await;

        // Once it has written everything it received, the guest ends the connection.
        agent.reply(MessageType::TcpClosed, session, &TcpClosed {}).await;
        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .expect("relay should finish once the guest has ended the connection")
            .expect("relay task should not panic")
            .expect("relay should end cleanly");
        agent.expect_silence().await;
    }

    #[tokio::test(flavor = "local")]
    async fn a_guest_that_never_closes_a_finished_connection_is_cancelled() {
        let (client, mut agent) = FakeGuestAgent::handshake(None).await;
        let (opened, ()) = tokio::join!(GuestTcpStream::open(client, "127.0.0.1", 80), async {
            let connect = agent.expect(MessageType::TcpConnect).await;
            let request: TcpConnect = connect.payload().expect("connect request");
            let offer = request.bulk.expect("a bulk lane is offered to a runtime that has one");
            agent
                .reply(MessageType::TcpConnected, connect.id, &TcpConnected {})
                .await;
            let accepted = BulkAccepted {
                kind: BulkKind::Tcp,
                flows: BULK_FLOW_MASK_HOST_TO_GUEST | BULK_FLOW_MASK_GUEST_TO_HOST,
                format: BULK_FORMAT_RAW_V1,
                max_record_payload: offer.max_record_payload,
                host_to_guest_credit_limit: offer.guest_to_host_credit_limit,
                guest_to_host_credit_limit: offer.guest_to_host_credit_limit,
            };
            agent.reply(MessageType::BulkAccepted, connect.id, &accepted).await;
        });
        let stream = opened.expect("guest connect should succeed");
        let session = stream.id;

        let (mut browser, forwarded) = host_connection().await;
        let relay = tokio::task::spawn_local(stream.relay(forwarded));
        browser.shutdown().await.expect("half-close the browser side");
        agent.expect(MessageType::BulkFinish).await;
        agent
            .reply(
                MessageType::BulkFinish,
                session,
                &BulkFinish {
                    kind: BulkKind::Tcp,
                    flow: BulkFlow::GuestToHost,
                    final_offset: 0,
                },
            )
            .await;

        // Both ends have finished, but the guest never sends its terminal frame.
        tokio::time::pause();
        let ended = tokio::time::timeout(super::BULK_CLOSE_GRACE * 2, relay)
            .await
            .expect("the relay should give up once its grace period has passed")
            .expect("relay task should not panic");
        assert!(ended.is_err(), "giving up on the guest is reported");
        tokio::time::resume();
        let cancel = agent.expect(MessageType::BulkCancel).await;
        assert_eq!(cancel.id, session, "the abandoned connection is cancelled");
    }

    #[tokio::test(flavor = "local")]
    async fn credit_flows_while_the_host_is_not_reading() {
        const WINDOW: u64 = 64 * 1024;
        const UPLOAD: usize = 256 * 1024;

        let (client, mut agent) = FakeGuestAgent::handshake(None).await;
        let (opened, ()) = tokio::join!(GuestTcpStream::open(client, "127.0.0.1", 80), async {
            let connect = agent.expect(MessageType::TcpConnect).await;
            let request: TcpConnect = connect.payload().expect("connect request");
            let offer = request.bulk.expect("a bulk lane is offered");
            agent
                .reply(MessageType::TcpConnected, connect.id, &TcpConnected {})
                .await;
            let accepted = BulkAccepted {
                kind: BulkKind::Tcp,
                flows: BULK_FLOW_MASK_HOST_TO_GUEST | BULK_FLOW_MASK_GUEST_TO_HOST,
                format: BULK_FORMAT_RAW_V1,
                max_record_payload: offer.max_record_payload,
                host_to_guest_credit_limit: WINDOW,
                guest_to_host_credit_limit: offer.guest_to_host_credit_limit,
            };
            agent.reply(MessageType::BulkAccepted, connect.id, &accepted).await;
        });
        let stream = opened.expect("guest connect should succeed");
        let session = stream.id;

        // A small pipe, so a guest answer the host does not read soon blocks writing it.
        let (browser, forwarded) = tokio::io::duplex(1024);
        let (host_reader, host_writer) = tokio::io::split(forwarded);
        let relay = tokio::task::spawn_local(stream.relay_io(host_reader, host_writer));

        // The guest answers early, with more than the pipe holds.
        agent
            .write_bulk(&BulkRecord {
                id: session,
                kind: BulkKind::Tcp,
                flow: BulkFlow::GuestToHost,
                offset: 0,
                payload: Bytes::from(vec![b'a'; 64 * 1024]),
            })
            .await;

        // The browser reads only once its upload, larger than the window, is sent.
        let (mut browser_reader, mut browser_writer) = tokio::io::split(browser);
        let uploading = tokio::task::spawn_local(async move {
            browser_writer.write_all(&vec![b'u'; UPLOAD]).await.expect("upload");
            browser_writer.shutdown().await.expect("end the upload");
            let mut answer = Vec::new();
            browser_reader.read_to_end(&mut answer).await.expect("answer");
            answer.len()
        });

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            let mut received = 0;
            while received < UPLOAD {
                let record = agent.expect_bulk().await;
                received += record.payload.len();
                // The guest takes each record as it comes and grants a window beyond it.
                agent
                    .reply(
                        MessageType::BulkCredit,
                        session,
                        &BulkCredit {
                            kind: BulkKind::Tcp,
                            flow: BulkFlow::HostToGuest,
                            consumed_offset: u64::try_from(received).expect("offset"),
                            credit_limit: u64::try_from(received).expect("offset") + WINDOW,
                        },
                    )
                    .await;
            }
            received
        })
        .await
        .expect("the upload completes while the host is not reading");
        assert_eq!(received, UPLOAD);
        agent.expect(MessageType::BulkFinish).await;

        agent
            .reply(
                MessageType::BulkFinish,
                session,
                &BulkFinish {
                    kind: BulkKind::Tcp,
                    flow: BulkFlow::GuestToHost,
                    final_offset: 64 * 1024,
                },
            )
            .await;
        agent.reply(MessageType::TcpClosed, session, &TcpClosed {}).await;
        let answered = tokio::time::timeout(Duration::from_secs(2), uploading)
            .await
            .expect("the browser reads the answer")
            .expect("browser task");
        assert_eq!(answered, 64 * 1024);
        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .expect("relay should finish")
            .expect("relay task should not panic")
            .expect("relay should end cleanly");
    }
}
