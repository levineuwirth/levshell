// Levshell Theme singleton.
//
// Source of truth for every visual token described in
// `spec/levshell-design.pdf` (v0.2). All concrete values here match the
// spec's **warm-dark** default palette, which is the primary design target.
//
// ## Naming convention
//
// The spec uses dot-notation like `surface.raised`, `type.body.emphasis`,
// `space.md`. QML property names can't contain dots, so we flatten to
// camelCase: `surfaceRaised`, `typeBodyEmphasisWeight`, `spaceMd`. The
// spec's §13.1 token-delivery example uses this same convention.
//
// ## Deferred tokens
//
// Blur / opacity (`bar.opacity`, `bar.blur_radius`), density modes,
// focus / break / ambient state, and urgency escalation levels are
// intentionally absent from this file — they're separate follow-up
// phases (see Phase 1.6 scope note). The properties defined here are
// all static values sufficient to render the resting-state dark bar.
//
// ## Spring tuning
//
// QML's `SpringAnimation` uses a simplified `spring`/`damping` model
// instead of raw stiffness/damping constants. The numeric values below
// are starting points that target the spec's settle durations
// (snappy ≈ 120ms, default ≈ 200ms, gentle ≈ 350ms); tune during
// manual testing if the motion feels wrong.

pragma Singleton

import QtQuick

QtObject {
    // =================================================================
    // THEME VARIANT — "dark" or "light"
    //
    // Set by `dispatchTheme()` in main.qml from the ThemePayload's
    // variant field. Widgets can key light-specific treatments off
    // this (e.g. inverted health pill outlines).
    // =================================================================
    property string mode: "dark"
    /// Active theme's display name ("Warm Dark", "Neutral Dark", ...).
    property string themeName: "Warm Dark"

    // =================================================================
    // SURFACES — §3.2, 4-tier hierarchy
    //
    // These (and everything from here through state colors) are
    // `property` (not `readonly`) so the daemon's TOML theme loader
    // can override them via DaemonMessage.Theme. Unspecified tokens
    // fall back to the warm-dark default values in these initializers.
    // =================================================================
    property color bg:            "#1A1B26"
    property color bgDark:        "#16161E"
    property color surface:       "#24283B"
    property color surfaceRaised: "#2F3549"
    property color overlay:       "#3B4261"

    // =================================================================
    // CONTENT COLORS — §3.3
    //
    // Note: the spec names these `on.primary` and `on.surface`, but QML
    // property names starting with `on*` are parsed as signal handlers,
    // so we prefix with `textOn` to keep them as colors.
    // =================================================================
    property color fg:            "#C0CAF5"  // primary text & icons
    property color fgMuted:       "#565F89"  // disabled, timestamps
    property color fgSubtle:      "#737AA2"  // secondary labels, hints
    property color textOnPrimary: "#1A1B26"  // text on primary bg
    property color textOnSurface: "#C0CAF5"  // alias of fg
    property color outline:       "#3B4261"  // borders, dividers

    // =================================================================
    // ACCENT COLORS — §3.4, single primary
    // =================================================================
    property color primary:          "#7AA2F7"
    property color primaryVariant:   "#5A7FD4"
    property color secondary:        "#BB9AF7"
    property color secondaryVariant: "#9D7CD8"
    property color tertiary:         "#7DCFFF"

    // =================================================================
    // STATE COLORS — §3.5, full saturation for escalation
    // =================================================================
    property color success: "#9ECE6A"
    property color warning: "#E0AF68"
    property color error:   "#F7768E"
    property color info:    "#2AC3DE"

    // =================================================================
    // HEALTH STATE PILLS — §7.3, muted/desaturated
    // These are deliberately *not* the full-saturation state colors.
    // Health state communicates data freshness (a lower-urgency concern);
    // the full-sat tokens are reserved for the urgency/escalation system.
    // =================================================================
    property color stalePill: "#737AA2"  // derived from fgSubtle
    property color errorPill: "#B8806A"  // muted terracotta

    // =================================================================
    // TYPOGRAPHY — §4
    //
    // `fontText` and `fontMono` are overridable via the TOML theme's
    // [typography] section. `fontIcon` normally stays fixed (Phosphor
    // is bundled at `shell/fonts/Phosphor.ttf`) but can be overridden
    // for themes that ship a different icon set.
    // =================================================================
    property string fontText: "Spectral"
    property string fontMono: "IBM Plex Mono"
    property string fontIcon: "Phosphor"

    // Type scale (1.2× minor-third, anchored at 13px body). Each entry
    // has a matching weight token.
    readonly property int typeDisplay:           28
    readonly property int typeDisplayWeight:     600
    readonly property int typeHeadline:          22
    readonly property int typeHeadlineWeight:    600
    readonly property int typeTitle:             17
    readonly property int typeTitleWeight:       600
    readonly property int typeBody:              13
    readonly property int typeBodyWeight:        400
    readonly property int typeBodyEmphasisSize:  13
    readonly property int typeBodyEmphasisWeight: 600
    readonly property int typeLabel:             12
    readonly property int typeLabelWeight:       500
    readonly property int typeCaption:           11
    readonly property int typeCaptionWeight:     400

    // Line-height multipliers (QML Text uses `lineHeightMode: ProportionalHeight`).
    readonly property real lineHeightBody:    1.4
    readonly property real lineHeightLabel:   1.2
    readonly property real lineHeightDisplay: 1.0

    // =================================================================
    // SPACING — §5.1, 4px base unit
    // =================================================================
    readonly property int spaceXs:  2
    readonly property int spaceSm:  4
    readonly property int spaceMd:  8
    readonly property int spaceLg:  12
    readonly property int spaceXl:  16
    readonly property int space2xl: 24

    // =================================================================
    // BAR BLUR AND OPACITY — §3.1.3
    //
    // In blur mode the bar and overlay panels are semi-transparent with
    // a compositor-side backdrop blur. On battery (or when the
    // compositor lacks the ext_background_effect_v1 protocol) the bar
    // falls back to full opacity. Both modes are intentional visual
    // treatments — opaque is not "broken blur". The daemon relays
    // PowerStateChanged → DaemonMessage::PowerState; the shell sets
    // `onBattery` in its dispatchDaemonMessage handler.
    // =================================================================
    property real barOpacity:        0.80
    property real barOpacityBattery: 1.0
    property int  barBlurRadius:          30
    property int  barBlurRadiusBattery:   0

    // Dropdown panels (palette / notification center / quick settings /
    // clock hub). The original design assumed a compositor-side backdrop
    // blur behind a semi-transparent panel (alpha ≈ 0.72). Sway/wlroots
    // does NOT implement the background-blur protocol Quickshell's
    // `BackgroundEffect.blurRegion` needs (`ext_background_effect_v1`),
    // so on this compositor the blur is a silent no-op and a 0.72 panel
    // just renders raw see-through — wallpaper bleeds through the text
    // and the panel reads as broken. Until/unless a blurred compositor
    // is in use, panels must be near-opaque to stay legible.
    property real panelOpacity:        0.97
    property real panelOpacityBattery: 0.98

    property bool onBattery: false

    // =================================================================
    // BAR GEOMETRY — §5.2
    //
    // Static per-density token pairs. The `density` property (set by
    // the shell's `dispatchDaemonMessage` handler) selects between them
    // via the computed `barHeight`, `iconSize`, etc. accessors below.
    // =================================================================
    property string density: "full"

    property int barHeightFull:    44
    property int barHeightCompact: 32
    readonly property int barHeightHidden:  0

    readonly property int iconSizeFull:    20
    readonly property int iconSizeCompact: 16

    readonly property int widgetPaddingHFull:    8
    readonly property int widgetPaddingVFull:    8
    readonly property int widgetPaddingHCompact: 4
    readonly property int widgetPaddingVCompact: 4

    readonly property int interWidgetGapFull:    8
    readonly property int interWidgetGapCompact: 4

    // Density-responsive computed tokens. Widgets and the bar bind to
    // these instead of the static *Full / *Compact variants.
    readonly property int barHeight:
        density === "full" ? barHeightFull
      : density === "compact" ? barHeightCompact
      : barHeightHidden
    readonly property int iconSize:
        density === "compact" ? iconSizeCompact : iconSizeFull
    readonly property int widgetPaddingH:
        density === "compact" ? widgetPaddingHCompact : widgetPaddingHFull
    readonly property int widgetPaddingV:
        density === "compact" ? widgetPaddingVCompact : widgetPaddingVFull
    readonly property int interWidgetGap:
        density === "compact" ? interWidgetGapCompact : interWidgetGapFull

    readonly property int widgetCornerRadius: 0

    // =================================================================
    // DROPDOWN / OVERLAY GEOMETRY — §12.1
    // =================================================================
    readonly property int panelCornerRadius: 8
    readonly property int panelInnerPadding: 12   // spaceLg
    readonly property int panelBorderWidth:  1
    // Drop shadow: QML doesn't ship a direct shadow primitive; we paint
    // shadow via a layered Rectangle. Tokens are exposed for consumers.
    readonly property int  panelShadowOffsetY: 4
    readonly property int  panelShadowBlur:    16
    readonly property real panelShadowOpacity: 0.30

    // =================================================================
    // MOTION — §6.2, duration scale
    // =================================================================
    readonly property int motionInstant: 0
    readonly property int motionFast:    120
    readonly property int motionNormal:  200
    readonly property int motionSlow:    350

    // =================================================================
    // SPRING TOKENS — §6.3
    //
    // Mapped from the spec's raw stiffness/damping onto QML
    // `SpringAnimation`'s `spring` (constant) + `damping` (ratio 0..1).
    // Settle-time targets: snappy ≈ 120ms, default ≈ 200ms, gentle ≈ 350ms.
    // =================================================================
    readonly property real springSnappy:         5.0
    readonly property real springSnappyDamping:  0.30
    readonly property real springDefault:        3.0
    readonly property real springDefaultDamping: 0.30
    readonly property real springGentle:         2.0
    readonly property real springGentleDamping:  0.35

    // Critically-damped variants for vertical anchored transitions
    // (dropdown open/close, bar reveal). Overshoot on a vertically
    // anchored element reads as disconnection from its anchor, so these
    // springs approach the target asymptotically.
    readonly property real springDefaultCriticalDamping: 1.0
    readonly property real springGentleCriticalDamping:  1.0
    readonly property real springMass: 1.0

    // =================================================================
    // PROMINENCE WIDTH HEURISTICS
    //
    // The spec doesn't prescribe pixel widths per prominence — those are
    // content-driven (§5, §7). WidgetWrapper sizes itself from its child
    // content plus `widgetPaddingH` on each side. The two tokens
    // below are the only non-content-driven widths in the system:
    //   • widthHidden — zero, used when prominence == "hidden".
    //   • widthBadge  — the 6px dot + breathing room, used when
    //     prominence == "badge" (no icon/text content at all).
    // =================================================================
    readonly property int widthHidden: 0
    readonly property int widthBadge:  16

    // =================================================================
    // ICONOGRAPHY — §8 Phosphor Icons private-use-area codepoints
    //
    // Each token holds a single PUA codepoint into the bundled
    // Phosphor.ttf. Widgets render these via
    //     font.family: Theme.fontIcon
    //     text: Theme.iconFoo
    // rather than hardcoding the escape sequence. The mapping follows
    // the `ph-<name>` CSS class names from phosphor-icons/web; the
    // comments next to each token name the Phosphor icon used.
    //
    // §7.3 Status indicator (top-right corner) uses `iconClockCountdown`
    // for Stale and `iconWarning` for Error, rendered at `statusIconSize`.
    // =================================================================
    readonly property string iconClockCountdown:  "\uED2C"  // ph-clock-countdown
    readonly property string iconWarning:         "\uE4E0"  // ph-warning
    readonly property string iconMemory:          "\uE9C4"  // ph-memory
    readonly property string iconCpu:             "\uE610"  // ph-cpu
    readonly property string iconHardDrive:       "\uE796"  // ph-hard-drive (disk)
    // Power profiles (spec \u00A72.3.2) \u2014 power-saver / balanced / performance.
    readonly property string iconLeaf:            "\uE2E0"  // ph-leaf (power-saver)
    readonly property string iconGauge:           "\uE27A"  // ph-gauge (balanced)
    readonly property string iconLightning:       "\uE2EC"  // ph-lightning (performance)
    readonly property string iconBell:            "\uE0CE"  // ph-bell (notifications)
    readonly property string iconAppWindow:       "\uE5DA"  // ph-app-window (app hint)
    readonly property string iconSquaresFour:     "\uE464"  // ph-squares-four (workspace hint)
    readonly property string iconNote:            "\uE348"  // ph-note (note hint)
    readonly property string iconMagnifyingGlass: "\uE30C"  // ph-magnifying-glass (search)

    // Battery — six levels plus charging. BatteryWidget picks via
    // percent buckets: full≥90, high≥70, medium≥40, low≥15, else empty.
    readonly property string iconBatteryFull:     "\uE0C0"  // ph-battery-full
    readonly property string iconBatteryHigh:     "\uE0C2"  // ph-battery-high
    readonly property string iconBatteryMedium:   "\uE0C6"  // ph-battery-medium
    readonly property string iconBatteryLow:      "\uE0C4"  // ph-battery-low
    readonly property string iconBatteryEmpty:    "\uE0BE"  // ph-battery-empty
    readonly property string iconBatteryCharging: "\uE0BC"  // ph-battery-charging-vertical

    // Network — wifi signal tiers plus a wired / fallback indicator.
    // NetworkWidget picks via: !primary → slash, has wifi quality →
    // high/medium/low by percent, wired or metadata-free → network.
    readonly property string iconWifiHigh:        "\uE4EA"  // ph-wifi-high
    readonly property string iconWifiMedium:      "\uE4EE"  // ph-wifi-medium
    readonly property string iconWifiLow:         "\uE4EC"  // ph-wifi-low
    readonly property string iconWifiSlash:       "\uE4F2"  // ph-wifi-slash (no connection)
    readonly property string iconNetwork:         "\uEDDE"  // ph-network (wired / generic)

    // Notification center — bell variant for DnD, dismiss glyph.
    readonly property string iconBellSlash:       "\uE0D0"  // ph-bell-slash
    readonly property string iconX:               "\uE4F6"  // ph-x

    // Quick-settings — PipeWire volume state + brightness.
    readonly property string iconSpeakerHigh:     "\uE44A"  // ph-speaker-high
    readonly property string iconSpeakerSlash:    "\uE45A"  // ph-speaker-slash (muted)
    readonly property string iconSun:             "\uE472"  // ph-sun (brightness)

    // Quick-settings tiles. These codepoints were render-verified against
    // the bundled shell/fonts/Phosphor.ttf (the font carries no semantic
    // glyph names, so each was confirmed visually, not assumed from a
    // Phosphor version map). NOTE: the prior inline \uE0A0 ("Bluetooth")
    // and \uE334 ("Night Light") were WRONG in THIS font \u2014 they render as
    // left-right arrows and a computer mouse respectively; the constants
    // below are the verified replacements.
    readonly property string iconBluetooth:       "\uE0DA"  // ph-bluetooth
    readonly property string iconMoon:            "\uE330"  // ph-moon (night light)
    readonly property string iconVideoCamera:     "\uE4DA"  // ph-video-camera (screen rec)
    readonly property string iconShieldCheck:     "\uE40C"  // ph-shield-check (VPN)
    readonly property string iconCaretDown:       "\uE136"  // ph-caret-down (expand)

    // Control-center bar entry point. Aliased to the verified
    // squares-four glyph (a 2x2 tile grid \u2014 the macOS Control Center
    // metaphor) rather than minting an unverified Phosphor codepoint.
    readonly property string iconControlCenter:   iconSquaresFour

    // Status indicator icon aliases (referenced by WidgetWrapper).
    readonly property string statusIconStale: iconClockCountdown
    readonly property string statusIconError: iconWarning
    readonly property int    statusIconSize:  10
}
