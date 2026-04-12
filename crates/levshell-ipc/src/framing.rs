//! Length-prefixed framing helpers for the Unix-socket transport.
//!
//! Frames are `4-byte big-endian u32 length || payload`. The reader refuses
//! to allocate any frame larger than [`MAX_FRAME_SIZE`] so a corrupt or
//! malicious peer cannot OOM the daemon by lying about a payload's size.
//!
//! These helpers are deliberately small and codec-agnostic — every IPC call
//! site funnels through them, so any future tweak (compression, framing
//! version negotiation) only has to land in one place.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{IpcError, Result, MAX_FRAME_SIZE};

pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(IpcError::ConnectionClosed);
        }
        Err(e) => return Err(IpcError::Io(e)),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(IpcError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }

    let mut buf = vec![0u8; len];
    match reader.read_exact(&mut buf).await {
        Ok(_) => Ok(buf),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err(IpcError::ConnectionClosed)
        }
        Err(e) => Err(IpcError::Io(e)),
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
    let len = (bytes.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}
