//! The frames a connection carries once a call has turned it into a terminal.
//!
//! Each frame is a type byte, a big-endian `u32` payload length and the payload. Output stays raw bytes, as it can end
//! inside a UTF-8 character and is nearly all of the traffic. `agentd` never interprets it, only cuts it into frames.

use std::io;

use bytes::{Buf as _, Bytes, BytesMut};
use sandbox::terminal::TerminalSize;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// The largest payload one frame carries. `agentd` splits larger output, so a reader never buffers more than this.
pub(crate) const MAX_FRAME_PAYLOAD: usize = 64 * 1024;

const HEADER_LENGTH: usize = 5;

const TYPE_INPUT: u8 = 0x01;
const TYPE_RESIZE: u8 = 0x02;
const TYPE_DETACH: u8 = 0x03;
const TYPE_OUTPUT: u8 = 0x81;
const TYPE_ENDED: u8 = 0x82;

/// One frame in either direction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Frame {
    /// Bytes typed or pasted, as the client's terminal sends them.
    Input(Bytes),
    /// The client's terminal changed size.
    Resize(TerminalSize),
    /// The client is leaving and wants its terminal put back first.
    Detach,
    /// What the program drew.
    Output(Bytes),
    /// Why the terminal ended. `agentd` closes the connection after it.
    Ended(Ended),
}

/// Why a terminal ends when agentd stops, which [`Ended::agentd_stopped`] recognizes.
pub(crate) const AGENTD_STOPPING: &str = "agentd is stopping";

/// Why a terminal ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ended {
    /// The program exited with this code.
    Exited(i32),
    /// The client detached, and a session's program keeps running.
    Detached,
    /// The terminal could not go on, such as when `agentd` stops.
    Error(String),
}

/// `Ended` as JSON: an object with exactly one of the fields, so a later reason is a new field a client can refuse.
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EndedJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exited: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detached: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Ended {
    /// Whether the terminal ended because agentd stopped, as it does for an upgrade, so that a client can attach
    /// again once agentd is back.
    #[must_use]
    pub fn agentd_stopped(&self) -> bool {
        matches!(self, Self::Error(message) if message == AGENTD_STOPPING)
    }

    fn to_json(&self) -> Vec<u8> {
        let json = match self {
            Self::Exited(code) => EndedJson {
                exited: Some(*code),
                ..EndedJson::default()
            },
            Self::Detached => EndedJson {
                detached: Some(true),
                ..EndedJson::default()
            },
            Self::Error(message) => EndedJson {
                error: Some(message.clone()),
                ..EndedJson::default()
            },
        };
        serde_json::to_vec(&json).unwrap_or_default()
    }

    fn from_json(payload: &[u8]) -> io::Result<Self> {
        let json: EndedJson = serde_json::from_slice(payload).map_err(|error| invalid(error.to_string()))?;
        match json {
            EndedJson {
                exited: Some(code),
                detached: None,
                error: None,
            } => Ok(Self::Exited(code)),
            EndedJson {
                exited: None,
                detached: Some(true),
                error: None,
            } => Ok(Self::Detached),
            EndedJson {
                exited: None,
                detached: None,
                error: Some(message),
            } => Ok(Self::Error(message)),
            EndedJson { .. } => Err(invalid("an ended frame must say exactly one reason")),
        }
    }
}

/// Writes one frame and flushes it, so a keystroke or its echo is not held back.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let (kind, payload) = match frame {
        Frame::Input(bytes) => (TYPE_INPUT, bytes.clone()),
        Frame::Resize(size) => {
            let mut payload = [0; 4];
            payload[..2].copy_from_slice(&size.rows().to_be_bytes());
            payload[2..].copy_from_slice(&size.columns().to_be_bytes());
            (TYPE_RESIZE, Bytes::copy_from_slice(&payload))
        }
        Frame::Detach => (TYPE_DETACH, Bytes::new()),
        Frame::Output(bytes) => (TYPE_OUTPUT, bytes.clone()),
        Frame::Ended(ended) => (TYPE_ENDED, Bytes::from(ended.to_json())),
    };
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(invalid("a frame's payload exceeds 64 KiB"));
    }
    let length = u32::try_from(payload.len()).map_err(|_| invalid("a frame's payload exceeds 64 KiB"))?;
    let mut header = [0; HEADER_LENGTH];
    header[0] = kind;
    header[1..].copy_from_slice(&length.to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

/// `bytes` as output frames of at most [`MAX_FRAME_PAYLOAD`] each, without copying.
pub(crate) fn output_frames(bytes: Bytes) -> impl Iterator<Item = Frame> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let chunk = rest.split_to(rest.len().min(MAX_FRAME_PAYLOAD));
        Some(Frame::Output(chunk))
    })
}

/// Reads frames from a stream.
///
/// [`Self::next`] can be dropped before it completes, as a branch of `tokio::select!` is, without losing a frame: what
/// it has read stays in its buffer for the next call.
pub(crate) struct FrameReader<R> {
    reader: R,
    buffer: BytesMut,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: BytesMut::new(),
        }
    }

    /// The next frame, or `None` when the stream ends between frames.
    ///
    /// # Errors
    ///
    /// A stream that ends inside a frame, and a frame that is too large, of an unknown type or with a payload its type
    /// does not allow, are [`io::ErrorKind::InvalidData`].
    pub(crate) async fn next(&mut self) -> io::Result<Option<Frame>> {
        loop {
            if let Some(frame) = self.parse()? {
                return Ok(Some(frame));
            }
            // `read_buf` either reads into the buffer or, when dropped, reads nothing.
            if self.reader.read_buf(&mut self.buffer).await? == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                return Err(invalid("the stream ended inside a frame"));
            }
        }
    }

    /// Takes one whole frame from the buffer, if it holds one. Refuses a bad header before its payload arrives.
    fn parse(&mut self) -> io::Result<Option<Frame>> {
        let Some(header) = self.buffer.get(..HEADER_LENGTH) else {
            if let Some(&kind) = self.buffer.first() {
                known_type(kind)?;
            }
            return Ok(None);
        };
        let kind = known_type(header[0])?;
        let length = usize::try_from(u32::from_be_bytes([header[1], header[2], header[3], header[4]]))
            .ok()
            .filter(|length| *length <= MAX_FRAME_PAYLOAD)
            .ok_or_else(|| invalid("a frame's payload exceeds 64 KiB"))?;
        if self.buffer.len() < HEADER_LENGTH + length {
            self.buffer.reserve(HEADER_LENGTH + length - self.buffer.len());
            return Ok(None);
        }
        self.buffer.advance(HEADER_LENGTH);
        let payload = self.buffer.split_to(length).freeze();
        frame(kind, payload).map(Some)
    }
}

fn known_type(kind: u8) -> io::Result<u8> {
    match kind {
        TYPE_INPUT | TYPE_RESIZE | TYPE_DETACH | TYPE_OUTPUT | TYPE_ENDED => Ok(kind),
        _ => Err(invalid(format!("unknown frame type {kind:#04x}"))),
    }
}

fn frame(kind: u8, payload: Bytes) -> io::Result<Frame> {
    match kind {
        TYPE_INPUT => Ok(Frame::Input(payload)),
        TYPE_RESIZE => {
            let [rows_high, rows_low, columns_high, columns_low] = payload[..] else {
                return Err(invalid("a resize frame carries 4 bytes"));
            };
            TerminalSize::new(
                u16::from_be_bytes([rows_high, rows_low]),
                u16::from_be_bytes([columns_high, columns_low]),
            )
            .map(Frame::Resize)
            .map_err(|error| invalid(error.to_string()))
        }
        TYPE_DETACH if payload.is_empty() => Ok(Frame::Detach),
        TYPE_DETACH => Err(invalid("a detach frame carries nothing")),
        TYPE_OUTPUT => Ok(Frame::Output(payload)),
        _ => Ended::from_json(&payload).map(Frame::Ended),
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    async fn encoded(frames: &[Frame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for frame in frames {
            write_frame(&mut bytes, frame).await.expect("write");
        }
        bytes
    }

    async fn decoded(bytes: &[u8]) -> io::Result<Vec<Frame>> {
        let mut reader = FrameReader::new(bytes);
        let mut frames = Vec::new();
        while let Some(frame) = reader.next().await? {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn header(kind: u8, length: u32) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes
    }

    async fn refused(bytes: &[u8]) -> io::Error {
        decoded(bytes).await.expect_err("refused")
    }

    #[tokio::test]
    async fn every_frame_comes_back_as_it_was_written() {
        let frames = vec![
            Frame::Input(Bytes::from_static(b"ls\r")),
            Frame::Input(Bytes::new()),
            Frame::Resize(TerminalSize::new(50, 132).expect("size")),
            Frame::Detach,
            Frame::Output(Bytes::from_static(b"\xe2\x94")),
            Frame::Output(Bytes::new()),
            Frame::Ended(Ended::Exited(-1)),
            Frame::Ended(Ended::Detached),
            Frame::Ended(Ended::Error("agentd is stopping".into())),
        ];
        assert_eq!(decoded(&encoded(&frames).await).await.expect("read"), frames);
    }

    #[tokio::test]
    async fn ended_says_why_in_json() {
        let bytes = encoded(&[Frame::Ended(Ended::Detached)]).await;
        assert_eq!(&bytes[HEADER_LENGTH..], br#"{"detached":true}"#);
        let bytes = encoded(&[Frame::Ended(Ended::Exited(3))]).await;
        assert_eq!(&bytes[HEADER_LENGTH..], br#"{"exited":3}"#);
    }

    #[test]
    fn output_is_cut_at_the_largest_payload() {
        let output = Bytes::from(vec![7; MAX_FRAME_PAYLOAD + 1]);
        let frames: Vec<_> = output_frames(output).collect();
        assert_eq!(
            frames,
            [
                Frame::Output(Bytes::from(vec![7; MAX_FRAME_PAYLOAD])),
                Frame::Output(Bytes::from_static(&[7])),
            ]
        );
        assert_eq!(output_frames(Bytes::new()).count(), 0);
    }

    #[tokio::test]
    async fn a_frame_read_in_pieces_is_whole() {
        let bytes = encoded(&[Frame::Output(Bytes::from_static(b"hello"))]).await;
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut reader = FrameReader::new(reader);
        writer.write_all(&bytes[..3]).await.expect("write");
        // A read that is dropped before the frame is whole loses nothing.
        let pending = tokio::time::timeout(std::time::Duration::from_millis(10), reader.next()).await;
        assert!(pending.is_err());
        writer.write_all(&bytes[3..]).await.expect("write");
        assert_eq!(
            reader.next().await.expect("read"),
            Some(Frame::Output(Bytes::from_static(b"hello")))
        );
    }

    #[tokio::test]
    async fn a_stream_that_ends_inside_a_frame_is_refused() {
        let bytes = encoded(&[Frame::Output(Bytes::from_static(b"hello"))]).await;
        assert_eq!(refused(&bytes[..2]).await.kind(), io::ErrorKind::InvalidData);
        assert_eq!(refused(&bytes[..7]).await.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_payload_over_the_largest_is_refused_from_its_header() {
        let length = u32::try_from(MAX_FRAME_PAYLOAD + 1).expect("length");
        assert_eq!(
            refused(&header(TYPE_OUTPUT, length)).await.kind(),
            io::ErrorKind::InvalidData
        );
        let mut writer = Vec::new();
        let error = write_frame(&mut writer, &Frame::Input(Bytes::from(vec![0; MAX_FRAME_PAYLOAD + 1])))
            .await
            .expect_err("too large");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn an_unknown_type_is_refused_from_its_first_byte() {
        assert_eq!(refused(&[0x04]).await.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_resize_must_be_four_bytes_of_nonzero_sizes() {
        let mut short = header(TYPE_RESIZE, 3);
        short.extend_from_slice(&[0, 1, 0]);
        assert_eq!(refused(&short).await.kind(), io::ErrorKind::InvalidData);
        let mut zero = header(TYPE_RESIZE, 4);
        zero.extend_from_slice(&[0, 0, 0, 80]);
        assert_eq!(refused(&zero).await.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_detach_with_a_payload_is_refused() {
        let mut detach = header(TYPE_DETACH, 1);
        detach.push(0);
        assert_eq!(refused(&detach).await.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn an_ended_frame_must_give_one_known_reason() {
        for json in [
            &b"not json"[..],
            br"{}",
            br#"{"exited":0,"detached":true}"#,
            br#"{"detached":false}"#,
            br#"{"signal":9}"#,
        ] {
            let mut ended = header(TYPE_ENDED, u32::try_from(json.len()).expect("length"));
            ended.extend_from_slice(json);
            assert_eq!(refused(&ended).await.kind(), io::ErrorKind::InvalidData, "{json:?}");
        }
    }
}
