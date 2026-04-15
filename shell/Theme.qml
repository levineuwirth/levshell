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
    // SURFACES — §3.2, 4-tier hierarchy
    // =================================================================
    readonly property color bg:            "#1A1B26"
    readonly property color bgDark:        "#16161E"
    readonly property color surface:       "#24283B"
    readonly property color surfaceRaised: "#2F3549"
    readonly property color overlay:       "#3B4261"

    // =================================================================
    // CONTENT COLORS — §3.3
    //
    // Note: the spec names these `on.primary` and `on.surface`, but QML
    // property names starting with `on*` are parsed as signal handlers,
    // so we prefix with `textOn` to keep them as colors.
    // =================================================================
    readonly property color fg:            "#C0CAF5"  // primary text & icons
    readonly property color fgMuted:       "#565F89"  // disabled, timestamps
    readonly property color fgSubtle:      "#737AA2"  // secondary labels, hints
    readonly property color textOnPrimary: "#1A1B26"  // text on primary bg
    readonly property color textOnSurface: "#C0CAF5"  // alias of fg
    readonly property color outline:       "#3B4261"  // borders, dividers

    // =================================================================
    // ACCENT COLORS — §3.4, single primary
    // =================================================================
    readonly property color primary:          "#7AA2F7"
    readonly property color primaryVariant:   "#5A7FD4"
    readonly property color secondary:        "#BB9AF7"
    readonly property color secondaryVariant: "#9D7CD8"
    readonly property color tertiary:         "#7DCFFF"

    // =================================================================
    // STATE COLORS — §3.5, full saturation for escalation
    // =================================================================
    readonly property color success: "#9ECE6A"
    readonly property color warning: "#E0AF68"
    readonly property color error:   "#F7768E"
    readonly property color info:    "#2AC3DE"

    // =================================================================
    // HEALTH STATE PILLS — §7.3, muted/desaturated
    // These are deliberately *not* the full-saturation state colors.
    // Health state communicates data freshness (a lower-urgency concern);
    // the full-sat tokens are reserved for the urgency/escalation system.
    // =================================================================
    readonly property color stalePill: "#737AA2"  // derived from fgSubtle
    readonly property color errorPill: "#B8806A"  // muted terracotta

    // =================================================================
    // TYPOGRAPHY — §4
    // =================================================================
    readonly property string fontText: "Spectral"
    readonly property string fontMono: "IBM Plex Mono"
    // Phosphor Icons — requires ttf-phosphor-icons installed, or a
    // bundled .ttf in shell/fonts/. Phase 1.6 falls back to Unicode
    // glyphs when the font is missing.
    readonly property string fontIcon: "Phosphor"

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
    // BAR GEOMETRY — §5.2
    // =================================================================
    readonly property int barHeightFull:    44
    readonly property int barHeightCompact: 32
    readonly property int barHeightHidden:  0

    readonly property int iconSizeFull:    20
    readonly property int iconSizeCompact: 16

    readonly property int widgetPaddingHFull:    8
    readonly property int widgetPaddingVFull:    8
    readonly property int widgetPaddingHCompact: 4
    readonly property int widgetPaddingVCompact: 4

    readonly property int interWidgetGapFull:    8
    readonly property int interWidgetGapCompact: 4

    // Bar widgets are edge-to-edge in the bar — no rounding.
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
    // content-driven. These are fallback targets used by WidgetWrapper
    // when a widget doesn't override `targetWidth`. They match the
    // heuristic table in `levshell-modules::context_engine`.
    // =================================================================
    readonly property int widthHidden:   0
    readonly property int widthBadge:    16
    readonly property int widthIconOnly: 32
    readonly property int widthCompact:  96
    readonly property int widthVisible:  160
    readonly property int widthExpanded: 220

    // =================================================================
    // STATUS INDICATOR ICON (§7.3, top-right corner)
    //
    // Unicode placeholders until Phosphor Icons are bundled. The spec
    // calls for a small clock for Stale and a warning-triangle for
    // Error, rendered at 10px.
    // =================================================================
    readonly property string statusIconStale: "◷"
    readonly property string statusIconError: "⚠"
    readonly property int    statusIconSize:  10
}
