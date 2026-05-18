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
    /// Advance bar density to the next mode server-side
    /// (full -> compact -> hidden -> full). The daemon resolves the next
    /// value from the stored `bar.density` signal, so the client need not
    /// know the current density.
    DensityCycle,
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
    /// Activate, query, or enumerate themes (spec design doc §11).
    /// `Set` requires `name`; every other action ignores it.
    Theme {
        action: ThemeAction,
        #[serde(default)]
        name: Option<String>,
    },
    /// Force-fire the warmup overlay (spec §2.12.1). Bypasses the gap
    /// heuristic — the daemon assembles and pushes a fresh payload
    /// regardless of recent activity. v1 only supports `open`; other
    /// actions (query, dismiss) will land when the use case demands.
    Warmup { action: WarmupAction },

    /// Save / restore / list / delete a named context snapshot
    /// (spec §2.12.2). `name` is required for save/restore/delete
    /// and ignored for list.
    ContextSnapshot {
        action: ContextSnapshotAction,
        #[serde(default)]
        name: Option<String>,
    },

    /// Export the whole data store to a JSON snapshot, or restore one
    /// into an empty store (durability — spec §5.1, the unified store
    /// must not be a single-opaque-file risk). `path` is an absolute
    /// filesystem path the *daemon* reads/writes; the ctl client
    /// resolves it before sending.
    Data {
        action: DataAction,
        path: String,
    },

    /// Open / close / reset the rubber-duck overlay (spec §2.12.6).
    Duck { action: DuckAction },

    /// Count flashcards currently due (`due_at <= now`). Spec §2.19.1
    /// (`levshell-ctl anki due-count`); the daemon replies
    /// [`CtlResponse::Count`].
    AnkiDueCount,

    /// Drive the Pomodoro / focus-session timer (spec §2.2.1). The
    /// daemon publishes `Event::SessionTimerCommand` and replies
    /// [`CtlResponse::Ok`] — fire-and-forget; the bar pill reflects
    /// the new state.
    Timer { action: TimerAction },

    /// Forward a generic widget action onto the daemon bus (spec §2.19.1,
    /// e.g. `levshell-ctl widget ssh-dashboard reconnect host=gpu-3`).
    /// `data` is a JSON object string assembled by the ctl client from
    /// `key=value` params; it defaults to `"{}"` when no params are given.
    /// The daemon publishes `Event::WidgetActionReceived` and replies
    /// [`CtlResponse::Ok`] — delivery is fire-and-forget.
    Widget {
        widget_id: String,
        action: String,
        #[serde(default = "default_widget_data")]
        data: String,
    },

    /// Emit a desktop notification (spec §2.19.1, e.g.
    /// `levshell-ctl notify "Build finished" --urgency normal`).
    Notify {
        title: String,
        body: String,
        #[serde(default)]
        urgency: NotifyUrgency,
    },
}

fn default_widget_data() -> String {
    "{}".to_string()
}

/// Desktop-notification urgency for [`CtlRequest::Notify`]. Mirrors the
/// three Freedesktop urgency levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NotifyUrgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl NotifyUrgency {
    /// The lowercase wire string the daemon and `notify-rust` expect.
    pub fn as_wire(self) -> &'static str {
        match self {
            NotifyUrgency::Low => "low",
            NotifyUrgency::Normal => "normal",
            NotifyUrgency::Critical => "critical",
        }
    }
}

/// What to do with the session timer. See [`CtlRequest::Timer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimerAction {
    /// Start a work interval (from idle) or resume (from paused).
    Start,
    /// Freeze the elapsed counter.
    Pause,
    /// Unfreeze a paused timer.
    Resume,
    /// End the current interval and return to idle.
    Stop,
    /// End the current interval immediately and advance to the next.
    Skip,
}

/// What to do with the rubber-duck overlay. See [`CtlRequest::Duck`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DuckAction {
    /// Reveal the overlay. Conversation persists from the previous
    /// session (until daemon restart).
    Open,
    /// Hide the overlay without clearing the conversation.
    Close,
    /// Clear the conversation and close the overlay.
    Reset,
}

/// What to do with a named context snapshot. See
/// [`CtlRequest::ContextSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextSnapshotAction {
    /// Capture the current sway tree into `name`, overwriting any
    /// existing snapshot with the same name.
    Save,
    /// Apply the saved snapshot `name` — move existing windows to
    /// their saved workspaces and re-launch any missing apps via the
    /// captured cmdline.
    Restore,
    /// List saved snapshot names.
    List,
    /// Delete the saved snapshot `name`.
    Delete,
}

/// Whole-store durability operation. See [`CtlRequest::Data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DataAction {
    /// Write a portable JSON snapshot of every row to `path`.
    Export,
    /// Restore a snapshot from `path` into this (empty) store.
    Import,
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
    /// Currently active theme summary. Returned for
    /// [`ThemeAction::Query`] and [`ThemeAction::Set`].
    ActiveTheme(ThemeSnapshot),
    /// Available theme names (file stems). Returned for
    /// [`ThemeAction::List`].
    Themes {
        names: Vec<String>,
    },
    /// Human-readable summary of a context save / restore / delete
    /// operation (spec §2.12.2). Carries a single line the ctl client
    /// prints verbatim.
    ContextSnapshotResult {
        summary: String,
    },
    /// Saved context snapshot names. Returned for
    /// [`ContextSnapshotAction::List`].
    ContextSnapshots {
        names: Vec<String>,
    },
    /// A scalar count. Returned for [`CtlRequest::AnkiDueCount`]
    /// (spec §2.19.1, `levshell-ctl anki due-count`).
    Count {
        count: u64,
    },
}

/// Compact shape of the currently-active theme. The full token
/// payload is streamed over [`crate::DaemonMessage::Theme`] on the
/// persistent shell connection; this summary is for ctl output
/// ("warm-dark (dark)").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeSnapshot {
    pub name: String,
    /// `"dark"` or `"light"`.
    pub variant: String,
    #[serde(default)]
    pub light_pair: Option<String>,
    #[serde(default)]
    pub dark_pair: Option<String>,
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

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAction {
    /// Activate the theme named by `name`.
    Set,
    /// Switch between the current theme and its `light_pair` /
    /// `dark_pair`. No-op if the current theme doesn't declare one.
    ToggleMode,
    /// Return the active theme snapshot without changing anything.
    Query,
    /// Enumerate available theme names.
    List,
    /// Toggle presentation mode (spec §2.18) — mute non-critical
    /// surfaces for screen-sharing / talks. `name` carries the desired
    /// state: `"on"`, `"off"`, or `"toggle"` (default when omitted).
    Presentation,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmupAction {
    /// Force-fire the warmup overlay now.
    Open,
}
