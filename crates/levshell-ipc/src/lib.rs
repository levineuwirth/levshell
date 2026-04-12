//! Levshell IPC layer.
//!
//! Defines the wire protocol between `levshell-daemon` and the QuickShell QML
//! frontend: a length-prefixed framing over a Unix domain socket carrying
//! codec-encoded messages. The default codec is JSON; the [`Codec`] trait keeps
//! the door open for swapping in MessagePack or FlatBuffers without protocol
//! redesign.

#![forbid(unsafe_code)]

mod codec;
mod error;
mod framing;
mod messages;
mod publisher;
mod server;

pub use codec::{Codec, JsonCodec};
pub use error::{IpcError, Result, MAX_FRAME_SIZE};
pub use messages::{
    BarDensity, BarLayout, CommandPaletteQuery, CommandPaletteSelect, DaemonMessage, DensityChange,
    Prominence, ShellMessage, WidgetAction, WidgetStatus, WidgetUpdate, WidgetVisibility,
};
pub use publisher::{spawn_writer_task, WidgetPublisher, WriterTask};
pub use server::{default_socket_path, IpcConnection, IpcReader, IpcServer, IpcWriter};
