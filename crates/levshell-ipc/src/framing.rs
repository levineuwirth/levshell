//! Newline-delimited framing for the Unix-socket transport.
//!
//! Each frame is a codec-encoded payload followed by a single `\n` byte.
//! With [`JsonCodec`](crate::codec::JsonCodec) this produces NDJSON, which
//! the QuickShell `SplitParser` can consume directly. A cap of
//! [`MAX_FRAME_SIZE`] is enforced on both sides so a corrupt or runaway
//! peer cannot OOM the daemon by sending an unbounded line.
//!
//! Why a newline delimiter: Quickshell 0.2 exposes socket reads through
//! `DataStreamParser` subclasses, and the only general-purpose parser is
//! [`SplitParser`] which splits on a string marker. `\n` is a safe choice
//! for JSON because the grammar forbids unescaped newlines inside strings
//! and compact serde_json never emits them.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{IpcError, Result, MAX_FRAME_SIZE};

pub(crate) const FRAME_DELIMITER: u8 = b'\n';

pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncBufRead + Unpin,
{
    // Read at most MAX_FRAME_SIZE + 1 bytes — one past the cap so we can
    // distinguish "frame exactly at the cap" from "peer is sending garbage
    // that never terminates."
    let limit = (MAX_FRAME_SIZE as u64) + 1;
    let mut limited = reader.take(limit);
    let mut buf = Vec::new();
    let n = limited.read_until(FRAME_DELIMITER, &mut buf).await?;
    if n == 0 {
        return Err(IpcError::ConnectionClosed);
    }
    if buf.last() == Some(&FRAME_DELIMITER) {
        buf.pop();
        return Ok(buf);
    }
    if buf.len() > MAX_FRAME_SIZE {
        Err(IpcError::FrameTooLarge {
            size: buf.len(),
            max: MAX_FRAME_SIZE,
        })
    } else {
        Err(IpcError::ConnectionClosed)
    }
}

pub(crate) async fn write_frame<W>(writer: &mut W, bytes: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(IpcError::FrameTooLarge {
            size: bytes.len(),
            max: MAX_FRAME_SIZE,
        });
    }
    if bytes.contains(&FRAME_DELIMITER) {
        return Err(IpcError::Codec(
            "encoded message contains a raw newline — codec must emit compact output".into(),
        ));
    }
    writer.write_all(bytes).await?;
    writer.write_all(&[FRAME_DELIMITER]).await?;
    writer.flush().await?;
    Ok(())
}
