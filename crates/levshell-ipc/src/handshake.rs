//! Client handshake and the `levshell-ctl` request/response protocol.
//!
//! Every client sends exactly one [`Hello`] frame as its first message after
//! connecting to the daemon's Unix socket. The daemon reads the handshake,
//! dispatches on [`ClientRole`], and from that point on the two sides speak
//! different wire protocols:
//!
//! - [`ClientRole::Shell`] → persistent streaming of [`DaemonMessage`] /
//!   [`ShellMessage`]. The QML bar takes this path.
//! - [`ClientRole::Ctl`] → one [`CtlRequest`] → one [`CtlResponse`] →
//!   close. `levshell-ctl` takes this path.
//!
//! The protocol version is bumped whenever a wire change would break older
//! clients. Phase 1 ships `PROTOCOL_VERSION = 1`.
//!
//! [`DaemonMessage`]: crate::DaemonMessage
//! [`ShellMessage`]: crate::ShellMessage

use serde::{Deserialize, Serialize};

use crate::messages::BarDensity;

/// Current wire-protocol version. Bumped when a change would break older
/// clients; handshake rejects mismatched versions.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// The first frame every client sends after connecting. Wrapped in an enum so
/// future handshake extensions (auth tokens, capabilities) can land as new
/// variants without breaking `type` dispatch in QML/JavaScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Hello {
    Hello {
        role: ClientRole,
        protocol_version: u32,
    },
}

impl Hello {
    pub fn new(role: ClientRole) -> Self {
        Hello::Hello {
            role,
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

/// Which half of the split protocol this client wants to speak.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    /// The QML bar. Persistent streaming connection. At most one at a time.
    Shell,
    /// `levshell-ctl` or equivalent one-shot CLI client. Any number of
    /// concurrent connections are allowed.
    Ctl,
}

// ---------------------------------------------------------------------------
// levshell-ctl request/response protocol
// ---------------------------------------------------------------------------

/// A one-shot command from a ctl client. Sent after the [`Hello`] handshake
/// on a ctl connection; the daemon replies with exactly one [`CtlResponse`]
/// and closes the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CtlRequest {
    /// Round-trip liveness check. Daemon replies [`CtlResponse::Pong`].
    Ping,
    /// Return a health snapshot of the running daemon.
    Status,
    /// Request a bar-density change. Phase 1.4 wires the bus consumer.
    Density { mode: BarDensity },
    /// Activate, cycle, or query a context profile. Phase 1.2 wires the
    /// bus consumer.
    Profile {
        action: ProfileAction,
        name: Option<String>,
    },
    /// Open, close, toggle, or query the command palette. Phase 1.5 wires
    /// the bus consumer.
    Palette {
        action: PaletteAction,
        query: Option<String>,
    },
    /// List registered projects. Daemon replies with
    /// [`CtlResponse::Projects`].
    Projects,
    /// Attach an entity (note / ref / flashcard / event / task) to a
    /// project. `project` is either the project's display name or its
    /// UUID-v7 string. `entity_id` is a UUID. `entity_type` is one of
    /// `"note"`, `"ref"`, `"flashcard"`, `"event"`, `"task"` — matching
    /// the data-model's serialized entity-type form.
    Attach {
        entity_type: String,
        entity_id: String,
        project: String,
    },
    /// Detach an entity from its current project (set project_id = NULL).
    /// Experiments cannot be detached — their `project_id` column is
    /// `NOT NULL` — the daemon returns
    /// [`CtlResponse::Error`] for that case.
    Detach {
        entity_type: String,
        entity_id: String,
    },
}

/// The daemon's reply to a [`CtlRequest`]. Exactly one of these is written
/// back on the same connection before the daemon closes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CtlResponse {
    Ok,
    Pong,
    Status(StatusSnapshot),
    /// List of registered projects. Returned for [`CtlRequest::Projects`].
    /// Wrapped in a struct variant (rather than tuple) because
    /// internally-tagged enums cannot merge a `type` field into a JSON
    /// array — serde would silently produce malformed output.
    Projects {
        projects: Vec<ProjectSummary>,
    },
    /// The request was rejected. `message` is human-readable and safe to
    /// print from the ctl client.
    Error {
        message: String,
    },
}

/// Compact shape of a project, used by [`CtlResponse::Projects`]. The
/// full entry structure lives in `levshell-projects` but we keep the IPC
/// surface self-contained so levshell-ipc stays a leaf crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub tags: Vec<String>,
    pub workspace_names: Vec<String>,
    pub accent_color: Option<String>,

    // Runtime metadata (spec §3.7). Session-scoped; resets on daemon
    // restart. Timestamps are RFC3339 strings (empty when never).
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default)]
    pub accumulated_focus_time_secs: u64,
    #[serde(default)]
    pub currently_active_workspaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub protocol_version: u32,
    pub socket_path: String,
    pub db_path: String,
    pub shell_connected: bool,
    pub module_count: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAction {
    Activate,
    Cycle,
    Query,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteAction {
    Open,
    Close,
    Toggle,
    Query,
}
