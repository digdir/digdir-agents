//! Relays a terminal between a program in a sandbox and the client whose connection asked for it.

use std::{rc::Rc, time::Duration};

use bytes::BytesMut;
use futures_util::StreamExt as _;
use sandbox::{
    LocalFuture,
    terminal::{TerminalControl, TerminalEvent, TerminalEventStream, TerminalSize},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, WriteHalf},
    time::Instant,
};

use crate::Error;

use super::{
    frames::{AGENTD_STOPPING, Ended, Frame, FrameReader, output_frames, write_frame},
    server::Server,
};

/// How long a program has to exit after it is asked to, before it is killed. tmux's client detaches at once on
/// `SIGTERM`, so this only matters for a program that ignores it, such as an interactive shell.
const DETACH_GRACE: Duration = Duration::from_secs(2);

/// A terminal `agentd` started for a client, and how to stop it.
pub struct Terminal {
    /// Takes the client's input and size.
    pub control: Rc<dyn TerminalControl>,
    /// What the program draws, then how it ended.
    pub events: TerminalEventStream,
    /// Stops the program when the client leaves. Dropping it releases what the terminal held.
    pub process: Box<dyn TerminalProcess>,
}

/// Stops the program behind a [`Terminal`].
pub trait TerminalProcess {
    /// Asks the program to exit, as `SIGTERM` does.
    fn terminate(&self) -> LocalFuture<'_, Result<(), Error>>;

    /// Ends the program.
    fn kill(&self) -> LocalFuture<'_, Result<(), Error>>;
}

/// The most input the relay holds for a program that has not taken it yet. Beyond it the relay stops reading the
/// client's frames, which pushes back on the client.
const MAX_QUEUED_INPUT: usize = 64 * 1024;

/// `agentd` has asked the program to exit, and says why once it has.
struct Detaching {
    ended: Ended,
    deadline: Instant,
    killed: bool,
}

/// Relays `terminal` over `stream` until the program exits, then tells the client why and returns.
///
/// The client leaves by sending `detach` or by closing the connection. Either way the program is asked to exit and
/// killed after [`DETACH_GRACE`]. Its exit then counts as a detach, whatever its code, since `agentd` asked for it.
///
/// Input to the program and output to the client each have one write in flight, and neither holds up the other. A
/// program that does not read its input would otherwise stop its own output, and with it the program, and a client
/// that does not read its output would keep `agentd` from stopping.
pub(super) async fn serve<S>(stream: BufReader<S>, terminal: Terminal, server: &Server)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Terminal {
        control,
        mut events,
        process,
    } = terminal;
    let (reader, writer) = tokio::io::split(stream);
    let mut frames = FrameReader::new(reader);
    // The writer, while no output is on its way to the client.
    let mut writer = Some(writer);
    let mut drawing: Option<LocalFuture<'_, (WriteHalf<BufReader<S>>, bool)>> = None;
    let mut client = Client::default();
    // What the program is taking now.
    let mut sending: Option<LocalFuture<'_, Result<(), sandbox::Error>>> = None;
    let mut detaching: Option<Detaching> = None;

    let ended = loop {
        if sending.is_none() {
            sending = client.next_for(&*control);
        }
        let deadline = detaching.as_ref().map(|detaching| detaching.deadline);
        // Not stopping already, so a stop only starts once.
        let running = detaching.is_none();
        tokio::select! {
            // The client's frames first, so a flood of output does not hold back its detach.
            biased;
            frame = frames.next(), if client.takes_frames() => {
                if let Some(ended) = client.take(frame)
                    && detaching.is_none()
                {
                    detaching = Some(detach(&*process, ended).await);
                }
            }
            sent = in_flight(&mut sending) => {
                sending = None;
                if let Err(error) = sent {
                    tracing::debug!(%error, "writing to a terminal failed");
                }
            }
            (returned, written) = in_flight(&mut drawing) => {
                drawing = None;
                writer = Some(returned);
                if !written {
                    client.gone();
                    if detaching.is_none() {
                        detaching = Some(detach(&*process, Ended::Detached).await);
                    }
                }
            }
            // No more output while some is on its way, which pushes back on the program.
            event = events.next(), if drawing.is_none() => match event {
                Some(Ok(TerminalEvent::Output(bytes))) => {
                    if client.writing && let Some(mut stream) = writer.take() {
                        drawing = Some(Box::pin(async move {
                            let written = write_output(&mut stream, bytes).await;
                            (stream, written)
                        }));
                    }
                }
                Some(Ok(TerminalEvent::Exited(status))) => {
                    break detaching.map_or(Ended::Exited(status.code), |detaching| detaching.ended);
                }
                Some(Ok(TerminalEvent::Failed { message })) => break Ended::Error(message),
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(%error, "a terminal stopped answering");
                    kill(&*process).await;
                    break Ended::Error("the terminal stopped answering".into());
                }
                None => {
                    kill(&*process).await;
                    break Ended::Error("the terminal stopped answering".into());
                }
            },
            () = server.shutdown_requested(), if running => {
                detaching = Some(detach(&*process, Ended::Error(AGENTD_STOPPING.into())).await);
            }
            () = sleep_until(deadline), if deadline.is_some() => {
                let Some(detaching) = detaching.as_mut() else { continue };
                if detaching.killed {
                    break Ended::Error("the terminal did not stop".into());
                }
                kill(&*process).await;
                detaching.killed = true;
                detaching.deadline = Instant::now() + DETACH_GRACE;
            }
        }
    };

    // Output cut off in the middle of a frame leaves nothing the client could read after it.
    if client.writing
        && drawing.is_none()
        && let Some(mut stream) = writer
    {
        // A client that does not read is not waited for.
        let _told = tokio::time::timeout(DETACH_GRACE, async {
            if write_frame(&mut stream, &Frame::Ended(ended)).await.is_ok() {
                let _closed = stream.shutdown().await;
            }
        })
        .await;
    }
}

/// What the relay knows of the client, and what it sent that the program has not taken yet.
struct Client {
    /// Whether the client still sends frames.
    reading: bool,
    /// Whether the client can still be written to.
    writing: bool,
    queued: BytesMut,
    resized: Option<TerminalSize>,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            reading: true,
            writing: true,
            queued: BytesMut::new(),
            resized: None,
        }
    }
}

impl Client {
    /// Whether to read the client's next frame: not once it stopped, nor while too much input waits.
    fn takes_frames(&self) -> bool {
        self.reading && self.queued.len() < MAX_QUEUED_INPUT
    }

    /// Takes in one read of the client's frames, and says why the client is leaving when it is.
    fn take(&mut self, frame: std::io::Result<Option<Frame>>) -> Option<Ended> {
        match frame {
            Ok(Some(Frame::Input(bytes))) => {
                self.queued.extend_from_slice(&bytes);
                None
            }
            Ok(Some(Frame::Resize(size))) => {
                self.resized = Some(size);
                None
            }
            // The client still reads, and may still close, which stops the writing.
            Ok(Some(Frame::Detach)) => Some(Ended::Detached),
            // What follows can no longer be read as frames, but the client can still be told why.
            Ok(Some(Frame::Output(_) | Frame::Ended(_))) => {
                self.reading = false;
                Some(Ended::Error("the client sent a frame only agentd sends".into()))
            }
            Err(error) => {
                self.reading = false;
                Some(Ended::Error(format!("the client sent a malformed frame: {error}")))
            }
            Ok(None) => {
                self.gone();
                Some(Ended::Detached)
            }
        }
    }

    /// The client closed its connection.
    const fn gone(&mut self) {
        self.reading = false;
        self.writing = false;
    }

    /// Hands the program the latest size, else the input waiting, if there is either.
    fn next_for<'a>(
        &mut self,
        control: &'a dyn TerminalControl,
    ) -> Option<LocalFuture<'a, Result<(), sandbox::Error>>> {
        if let Some(size) = self.resized.take() {
            return Some(control.resize(size));
        }
        (!self.queued.is_empty()).then(|| control.write_input(self.queued.split().freeze()))
    }
}

/// Completes with what `pending` completes with, and never when there is nothing pending.
async fn in_flight<T>(pending: &mut Option<LocalFuture<'_, T>>) -> T {
    match pending {
        Some(pending) => pending.await,
        None => std::future::pending().await,
    }
}

/// Asks the program to exit, and gives it [`DETACH_GRACE`] to.
async fn detach(process: &dyn TerminalProcess, ended: Ended) -> Detaching {
    if let Err(error) = process.terminate().await {
        tracing::debug!(%error, "asking a terminal's program to exit failed");
    }
    Detaching {
        ended,
        deadline: Instant::now() + DETACH_GRACE,
        killed: false,
    }
}

async fn kill(process: &dyn TerminalProcess) {
    if let Err(error) = process.kill().await {
        tracing::debug!(%error, "killing a terminal's program failed");
    }
}

/// Writes output as frames. `false` when the client is gone.
async fn write_output<W: AsyncWrite + Unpin>(writer: &mut W, bytes: bytes::Bytes) -> bool {
    for frame in output_frames(bytes) {
        if write_frame(writer, &frame).await.is_err() {
            return false;
        }
    }
    true
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
