//! Levshell IPC layer.
//!
//! Defines the wire protocol between `levshell-daemon` and the QuickShell QML
//! frontend: newline-delimited framing over a Unix domain socket carrying
//! codec-encoded messages. With the default [`JsonCodec`] this produces
//! NDJSON, which QuickShell's `SplitParser` can consume directly. The
//! [`Codec`] trait keeps the door open for alternate encodings as long as
//! they emit compact single-line output.

#![forbid(unsafe_code)]

mod codec;
mod error;
mod framing;
mod handshake;
mod messages;
mod publisher;
mod server;

pub use codec::{Codec, JsonCodec};
pub use error::{IpcError, Result, MAX_FRAME_SIZE};
pub use handshake::{
    ClientRole, ContextSnapshotAction, CtlRequest, CtlResponse, DuckAction, Hello, NotifyUrgency,
    PaletteAction, ProfileAction, ProjectSummary, StatusSnapshot, ThemeAction, ThemeSnapshot,
    WarmupAction, PROTOCOL_VERSION,
};
pub use messages::{
    BarDensity, BarDensityState, BarLayout, ClockHubPayload, CommandPaletteQuery,
    CommandPaletteSelect, ProcInfo, ProcessListPayload,
    CriticalEscalation, DaemonMessage, DensityChange, DuckSay, DuckToken, EscalationLevel,
    Nudge, PowerState, Prominence, ShellMessage, ThemeBar, ThemeColors, ThemeHealth, ThemeIcons,
    ThemePayload, ThemeTypography, WarmupEvent, WarmupPayload, WarmupProject, WidgetAction,
    WidgetStatus, WidgetUpdate, WidgetVisibility,
};
pub use publisher::{spawn_writer_task, WidgetPublisher, WriterTask};
pub use server::{default_socket_path, IpcConnection, IpcReader, IpcServer, IpcWriter};
