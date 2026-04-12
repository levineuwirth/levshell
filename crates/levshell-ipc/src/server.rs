//! Unix-socket IPC server, connection split, and the well-known socket path.
//!
//! The server binds at `$XDG_RUNTIME_DIR/levshell.sock` (or a caller-supplied
//! path), removes any stale socket file from a previous daemon run, and waits
//! for the QML shell to connect. Once accepted, the connection is split into
//! an [`IpcReader`] and an [`IpcWriter`]: typically the daemon's main loop
//! owns the reader (draining `ShellMessage`s from QML) and a single writer
//! task owns the writer (fanning `DaemonMessage`s out to QML).
//!
//! The server's `Drop` impl unlinks the socket file so a clean shutdown
//! leaves no stale entry behind. A crash leaves the file but the next
//! `bind` removes it before binding.

use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, BufWriter};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::codec::{Codec, JsonCodec};
use crate::error::{IpcError, Result};
use crate::framing::{read_frame, write_frame};

/// Resolve the well-known socket path: `$XDG_RUNTIME_DIR/levshell.sock`.
/// Returns [`IpcError::NoRuntimeDir`] if the env var is unset, which is the
/// usual signal that the user is not in a normal desktop session.
pub fn default_socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(IpcError::NoRuntimeDir)?;
    Ok(PathBuf::from(runtime).join("levshell.sock"))
}

/// A bound IPC server. Holds the [`UnixListener`] and the path it bound to,
/// and unlinks the path on `Drop`.
#[derive(Debug)]
pub struct IpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl IpcServer {
    /// Bind at a specific socket path. Removes any stale file at that path
    /// before binding so a previous unclean shutdown does not block startup.
    pub fn bind(path: impl Into<PathBuf>) -> Result<Self> {
        let socket_path = path.into();
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }
        if let Some(parent) = socket_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!(path = %socket_path.display(), "ipc server bound");
        Ok(Self {
            listener,
            socket_path,
        })
    }

    /// Convenience: bind at [`default_socket_path`].
    pub fn bind_default() -> Result<Self> {
        Self::bind(default_socket_path()?)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Wait for the next QML shell to connect. The Phase 0 daemon only
    /// expects a single shell, but this is async and can be called
    /// repeatedly if the shell reconnects.
    pub async fn accept(&self) -> Result<IpcConnection<JsonCodec>> {
        let (stream, _addr) = self.listener.accept().await?;
        tracing::debug!("ipc server accepted shell connection");
        Ok(IpcConnection::from_stream(stream, JsonCodec))
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.socket_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %self.socket_path.display(),
                    error = %e,
                    "failed to unlink ipc socket on drop"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connection types
// ---------------------------------------------------------------------------

/// One half-open IPC connection. Combines a buffered read half, a buffered
/// write half, and a codec. The expected use is to call [`Self::split`]
/// immediately and hand each half to a different task.
pub struct IpcConnection<C = JsonCodec> {
    reader: IpcReader<C>,
    writer: IpcWriter<C>,
}

impl<C: Codec + Clone> IpcConnection<C> {
    fn from_stream(stream: UnixStream, codec: C) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: IpcReader {
                inner: BufReader::new(read_half),
                codec: codec.clone(),
            },
            writer: IpcWriter {
                inner: BufWriter::new(write_half),
                codec,
            },
        }
    }

    /// Build a connection from an existing `UnixStream`. Useful for the
    /// shell side, which connects with `UnixStream::connect` rather than
    /// `accept`ing on a listener.
    pub fn from_unix_stream(stream: UnixStream) -> Self
    where
        C: Default,
    {
        Self::from_stream(stream, C::default())
    }

    pub fn split(self) -> (IpcReader<C>, IpcWriter<C>) {
        (self.reader, self.writer)
    }

    pub fn reader(&mut self) -> &mut IpcReader<C> {
        &mut self.reader
    }

    pub fn writer(&mut self) -> &mut IpcWriter<C> {
        &mut self.writer
    }
}

/// Read half: drains length-prefixed frames from the socket and decodes
/// them via the connection's codec.
pub struct IpcReader<C = JsonCodec, R = BufReader<OwnedReadHalf>> {
    inner: R,
    codec: C,
}

impl<C, R> IpcReader<C, R>
where
    C: Codec,
    R: AsyncRead + Unpin,
{
    pub async fn recv<M>(&mut self) -> Result<M>
    where
        M: DeserializeOwned,
    {
        let bytes = read_frame(&mut self.inner).await?;
        self.codec.decode(&bytes)
    }
}

/// Write half: encodes a message via the connection's codec and writes a
/// length-prefixed frame.
pub struct IpcWriter<C = JsonCodec, W = BufWriter<OwnedWriteHalf>> {
    inner: W,
    codec: C,
}

impl<C, W> IpcWriter<C, W>
where
    C: Codec,
    W: AsyncWrite + Unpin,
{
    pub async fn send<M>(&mut self, msg: &M) -> Result<()>
    where
        M: Serialize + ?Sized,
    {
        let bytes = self.codec.encode(msg)?;
        write_frame(&mut self.inner, &bytes).await
    }
}
