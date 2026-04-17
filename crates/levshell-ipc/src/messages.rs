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
    PowerState(PowerState),
    BarDensityState(BarDensityState),
    /// Full theme payload sent on shell connect and whenever
    /// `levshell-ctl theme set` activates a new theme. Fields mirror
    /// the TOML structure (spec design doc §11) as partial overrides
    /// — every inner field is `Option<String>` / `Option<f64>` so
    /// the QML side applies only the tokens this theme chose to
    /// override.
    ///
    /// Boxed because the variant is ~20 hex-string fields — leaving
    /// it inline would bloat every other variant by the same amount
    /// and blow out `TrySendError<DaemonMessage>` (clippy's
    /// `result_large_err` / `large_enum_variant`).
    Theme(Box<ThemePayload>),
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
pub struct PowerState {
    pub on_battery: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarDensityState {
    pub mode: BarDensity,
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

// ---------------------------------------------------------------------------
// Theme payload (spec design doc §11)
// ---------------------------------------------------------------------------

/// What the daemon sends to the shell after a theme load or
/// `levshell-ctl theme set`. Mirrors [`levshell_config::ThemeFile`]
/// closely but flattens `ThemeMeta` onto the root so QML consumers
/// don't need to dot through `payload.meta.name`. Every override
/// field is `Option<T>` — unspecified tokens fall back to the
/// Theme.qml built-in defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemePayload {
    pub name: String,
    /// `"dark"` or `"light"`.
    pub variant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_pair: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark_pair: Option<String>,

    #[serde(default)]
    pub colors: ThemeColors,
    #[serde(default)]
    pub health: ThemeHealth,
    #[serde(default)]
    pub bar: ThemeBar,
    #[serde(default)]
    pub typography: ThemeTypography,
    #[serde(default)]
    pub icons: ThemeIcons,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeColors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_dark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_raised: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_muted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_subtle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_primary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tertiary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeHealth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_pill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_pill: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeBar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur_radius: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity_battery: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur_radius_battery: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_full: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_compact: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeTypography {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_mono: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_icon: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeIcons {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duotone_secondary: Option<String>,
}
