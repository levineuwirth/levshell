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
    /// Full warmup payload sent when the daemon decides to fire the
    /// ramp-up overlay (spec §2.12.1). The shell opens the warmup
    /// panel on receipt; dismissal is shell-local. Boxed for the same
    /// reason as [`Self::Theme`] — keeps `DaemonMessage` small.
    Warmup(Box<WarmupPayload>),
    /// Tell the shell to render the rubber-duck overlay (spec §2.12.6).
    DuckOpen,
    /// Tell the shell to hide the rubber-duck overlay. The conversation
    /// is not cleared — a subsequent [`Self::DuckOpen`] reveals the
    /// same messages. Use [`Self::DuckReset`] to wipe.
    DuckClose,
    /// Wipe the shell's conversation state (daemon side has already
    /// cleared). Sent in response to ctl `duck reset` or an overflow
    /// safety condition.
    DuckReset,
    /// One streaming frame from the local LLM. `delta` is appended to
    /// the active assistant turn; `done = true` finalizes it. `role`
    /// is `"assistant"` in practice.
    DuckToken(DuckToken),
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
    /// The user typed a message in the rubber-duck overlay and hit
    /// send (spec §2.12.6). The daemon appends to its conversation
    /// vec and kicks off a streaming Ollama request, replying with a
    /// sequence of [`DaemonMessage::DuckToken`] frames.
    DuckSay(DuckSay),
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

/// User-typed message from the rubber-duck overlay (shell → daemon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuckSay {
    pub text: String,
}

/// One streaming frame of an assistant reply (daemon → shell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuckToken {
    /// `"assistant"` for every frame in practice; left as a string
    /// so the shell renders whatever the model claims.
    pub role: String,
    /// Substring to append to the active assistant turn. Empty on
    /// the final `done = true` frame.
    pub delta: String,
    /// `true` on the last frame — the shell uses this to commit the
    /// assistant turn (stop the typing cursor, allow send again).
    pub done: bool,
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

// ---------------------------------------------------------------------------
// Warmup payload (spec §2.12.1)
// ---------------------------------------------------------------------------

/// Ramp-up data assembled by the warmup module and pushed to the shell
/// when the trigger fires. The three section arrays may be empty
/// (e.g. no CalDAV events today); the shell renders empty-state copy
/// in that case rather than hiding the section.
///
/// Wall-clock timestamps are serialized as RFC 3339 strings so the QML
/// side can `new Date(str)` directly — no Z vs. offset parsing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WarmupPayload {
    /// UTC wall-clock of the fire. Shown as "Good morning / afternoon"
    /// headline and as a "fired at" debug hint.
    pub fired_at: String,
    /// Events starting today in the user's local timezone.
    pub events: Vec<WarmupEvent>,
    /// Flashcards due on or before `fired_at`.
    pub anki_due_count: u32,
    /// Active projects (status != complete), newest-active first.
    pub projects: Vec<WarmupProject>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarmupEvent {
    pub title: String,
    /// RFC 3339 UTC.
    pub start_at: String,
    /// RFC 3339 UTC.
    pub end_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarmupProject {
    pub name: String,
    /// One of `active`, `simmering`, `blocked`, `writing_up`.
    pub status: String,
    /// Seconds since the project was last active during the live
    /// session, or `None` if it has never been focused since daemon
    /// start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
}
