//! Error type for the IPC crate.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, IpcError>;

/// The maximum frame size we will accept on the wire. Anything larger is
/// almost certainly a corrupt prefix or a malicious peer; refuse to allocate
/// instead of OOMing the daemon.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: usize, max: usize },

    #[error("$XDG_RUNTIME_DIR is not set")]
    NoRuntimeDir,

    #[error("connection closed by peer")]
    ConnectionClosed,
}

impl IpcError {
    pub fn codec(err: impl std::fmt::Display) -> Self {
        IpcError::Codec(err.to_string())
    }
}
