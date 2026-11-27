//! Relays a terminal between a program in a sandbox and the client whose connection asked for it.

use std::{rc::Rc, time::Duration};

use futures_util::StreamExt as _;
use sandbox::{
    LocalFuture,
    terminal::{TerminalControl, TerminalEvent, TerminalEventStream},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader},
    time::Instant,
};

use crate::Error;

use super::{
    frames::{Ended, Frame, FrameReader, output_frames, write_frame},
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
pub(super) async fn serve<S>(stream: BufReader<S>, terminal: Terminal, server: &Server)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Terminal {
        control,
        mut events,
        process,
    } = terminal;
    let mut frames = FrameReader::new(stream);
    // Whether the client still sends frames, and whether it can still be written to.
    let mut reading = true;
    let mut writing = true;
    let mut detaching: Option<Detaching> = None;

    let ended = loop {
        let deadline = detaching.as_ref().map(|detaching| detaching.deadline);
        // Not stopping already, so a stop only starts once.
        let running = detaching.is_none();
        tokio::select! {
            // The client's frames first, so a flood of output does not hold back its detach.
            biased;
            frame = frames.next(), if reading => {
                let leaving = match frame {
                    Ok(Some(Frame::Input(bytes))) => {
                        if let Err(error) = control.write_input(bytes).await {
                            tracing::debug!(%error, "writing to a terminal failed");
                        }
                        None
                    }
                    Ok(Some(Frame::Resize(size))) => {
                        if let Err(error) = control.resize(size).await {
                            tracing::debug!(%error, "resizing a terminal failed");
                        }
                        None
                    }
                    // The client still reads, and may still close, which stops the writing.
                    Ok(Some(Frame::Detach)) => Some(Ended::Detached),
                    // What follows can no longer be read as frames, but the client can still be told why.
                    Ok(Some(Frame::Output(_) | Frame::Ended(_))) => {
                        reading = false;
                        Some(Ended::Error("the client sent a frame only agentd sends".into()))
                    }
                    Err(error) => {
                        reading = false;
                        Some(Ended::Error(format!("the client sent a malformed frame: {error}")))
                    }
                    Ok(None) => {
                        reading = false;
                        writing = false;
                        Some(Ended::Detached)
                    }
                };
                if let Some(ended) = leaving
                    && detaching.is_none()
                {
                    detaching = Some(detach(&*process, ended).await);
                }
            }
            event = events.next() => match event {
                Some(Ok(TerminalEvent::Output(bytes))) => {
                    if writing && !write_output(frames.get_mut(), bytes).await {
                        reading = false;
                        writing = false;
                        if detaching.is_none() {
                            detaching = Some(detach(&*process, Ended::Detached).await);
                        }
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
                detaching = Some(detach(&*process, Ended::Error("agentd is stopping".into())).await);
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

    if writing {
        let stream = frames.get_mut();
        if write_frame(stream, &Frame::Ended(ended)).await.is_ok() {
            let _closed = stream.shutdown().await;
        }
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
