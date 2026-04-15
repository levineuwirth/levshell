//! Phase 0 IPC message types.
//!
//! `DaemonMessage` flows daemon → QML; `ShellMessage` flows QML → daemon.
//! Both enums are tagged with a `type` discriminator and snake_case variant
//! names so the QML/JavaScript side can dispatch with a single `switch`
//! statement on the parsed JSON object.
//!
//! `state` and `data` payloads are deliberately `serde_json::Value` so this
//! crate stays agnostic to specific widget shapes — each module crate owns
//! the typed struct for its widget state and serializes it into the opaque
//! payload before publishing.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Daemon → QML
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DaemonMessage {
    WidgetUpdate(WidgetUpdate),
    WidgetVisibility(WidgetVisibility),
    BarLayout(BarLayout),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetUpdate {
    pub widget_id: String,
    pub widget_type: String,
    pub state: serde_json::Value,
    pub status: WidgetStatus,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WidgetStatus {
    #[default]
    Normal,
    Stale,
    Error,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetVisibility {
    pub widget_id: String,
    pub visible: bool,
    pub prominence: Prominence,
}

/// Widget prominence levels in strict total order:
/// `Hidden` < `Badge` < `IconOnly` < `Compact` < `Visible` < `Expanded`.
///
/// `Badge` renders as a 6px colored dot and communicates minimal presence
/// ("there are unread items", "a connection exists") without consuming
/// meaningful bar space. It sits between full invisibility and icon-only.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prominence {
    Hidden,
    Badge,
    IconOnly,
    Compact,
    Visible,
    Expanded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BarLayout {
    #[serde(default)]
    pub left: Vec<String>,
    #[serde(default)]
    pub center: Vec<String>,
    #[serde(default)]
    pub right: Vec<String>,
}

// ---------------------------------------------------------------------------
// QML → Daemon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShellMessage {
    WidgetAction(WidgetAction),
    CommandPaletteQuery(CommandPaletteQuery),
    CommandPaletteSelect(CommandPaletteSelect),
    /// The shell asks the daemon to close an open command palette. Sent
    /// when the user hits Escape or clicks outside the palette. Carries
    /// no payload — close is idempotent.
    CommandPaletteClose,
    DensityChange(DensityChange),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetAction {
    pub widget_id: String,
    pub action: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPaletteQuery {
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPaletteSelect {
    pub provider: String,
    pub item_id: String,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DensityChange {
    pub mode: BarDensity,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarDensity {
    Full,
    Compact,
    Hidden,
}
