// WidgetWrapper — spec §7.3 health-state chrome + prominence container.
//
// Structure (resting state):
//
//   ┌──┬─────────────────────────────────────────┬───┐
//   │  │                                         │   │
//   │p │             [content slot]              │ s │
//   │  │                                         │   │
//   └──┴─────────────────────────────────────────┴───┘
//
//     p = 2px left-edge status pill (hidden when status == normal)
//     s = 10px status indicator icon in top-right corner
//         (hidden when status == normal)
//
// The widget itself is **edge-to-edge with no background fill** — it
// inherits the bar's `surface` color. Widgets communicate presence
// through spacing, typography, and the health pill — not through
// bordered boxes. This matches the spec's "calm by default" principle.
//
// ## Health-state visual channels
//
// Per §7.3, health state uses three complementary channels:
//
//   1. Left-edge 2px pill (muted tokens, not full-sat state colors)
//   2. Content desaturation (token-swap mode for Phase 1.6; shader
//      mode is a Phase 1.7+ opt-in)
//   3. Status indicator icon (top-right, colorblind-accessible signal)
//
// Content desaturation is implemented by exposing `contentColor` and
// `accentColor` properties that swap to muted tokens when the widget
// is degraded. Child widgets **must** bind their icon/text colors to
// `root.contentColor` / `root.accentColor` (not directly to Theme.fg
// or Theme.primary) to opt into the desaturation pathway.
//
// **No opacity change.** The spec is explicit: readability is never
// compromised for degraded widgets. Opacity animation is reserved for
// prominence transitions (hidden ↔ visible).
//
// ## Prominence
//
// Widget width animates via `SpringAnimation` (spring.default) per
// §6.4 "Widget appear". Prominence "Badge" renders as a centered 6px
// dot per §7.1. Prominence "Hidden" sets width to 0 and opacity to 0.

import QtQuick
import "."

Item {
    id: root

    // =================================================================
    // INPUTS
    // =================================================================
    property string prominence: "visible"
    property string status: "normal"

    // Default content slot — widgets assign children that get parented
    // to `contentHolder`. This keeps the health chrome (pill + status
    // icon) outside the widget's layout so they don't collide with it.
    default property alias content: contentHolder.data

    // =================================================================
    // OUTPUTS — bound by child widgets via `root.contentColor` etc.
    // =================================================================
    readonly property bool degraded: status === "stale" || status === "error"

    readonly property color contentColor: degraded ? Theme.fgMuted : Theme.fg
    readonly property color accentColor:  degraded ? Theme.outline : Theme.primary
    readonly property color subtleColor:  degraded ? Theme.fgMuted : Theme.fgSubtle

    // Target width for the current prominence level. Widgets with
    // content-aware sizing override `targetWidth` directly.
    property int targetWidth: {
        switch (prominence) {
        case "hidden":    return Theme.widthHidden;
        case "badge":     return Theme.widthBadge;
        case "icon_only": return Theme.widthIconOnly;
        case "compact":   return Theme.widthCompact;
        case "visible":   return Theme.widthVisible;
        case "expanded":  return Theme.widthExpanded;
        default:          return Theme.widthVisible;
        }
    }

    readonly property bool isBadge: prominence === "badge"
    readonly property bool isVisibleAtAll:
        prominence !== "hidden" && status !== "unavailable"

    implicitWidth:  targetWidth
    implicitHeight: Theme.barHeightFull
    width:  targetWidth
    opacity: isVisibleAtAll ? 1.0 : 0.0
    visible: opacity > 0.01

    // Width uses a spring so prominence transitions overshoot slightly,
    // matching the "Widget appear/disappear" row of §6.4.
    Behavior on width {
        SpringAnimation {
            spring:  Theme.springDefault
            damping: Theme.springDefaultDamping
            mass:    Theme.springMass
            epsilon: 0.5
        }
    }
    // Opacity must not overshoot — clamped to [0,1] semantically.
    Behavior on opacity {
        NumberAnimation {
            duration: Theme.motionFast
            easing.type: Easing.OutCubic
        }
    }

    // -----------------------------------------------------------------
    // Content slot
    //
    // Anchored between the left-pill gutter and the status-icon
    // corner so the chrome never collides with child content.
    // -----------------------------------------------------------------
    Item {
        id: contentHolder
        anchors.fill: parent
        anchors.leftMargin:  Theme.widgetPaddingHFull
        anchors.rightMargin: Theme.widgetPaddingHFull
        anchors.topMargin:   Theme.spaceXs
        anchors.bottomMargin: Theme.spaceXs
        visible: !root.isBadge && root.isVisibleAtAll
        clip: true
    }

    // -----------------------------------------------------------------
    // Badge prominence — centered 6px dot, no content.
    // -----------------------------------------------------------------
    Rectangle {
        anchors.centerIn: parent
        visible: root.isBadge && root.isVisibleAtAll
        width:  6
        height: 6
        radius: 3
        color:  Theme.primary
    }

    // -----------------------------------------------------------------
    // Left-edge status pill — §7.3 channel 1
    //
    // 2px wide, partial height (title-area aligned — we approximate as
    // 70% of bar height centered vertically). Shows for stale/error.
    // -----------------------------------------------------------------
    Rectangle {
        id: statusPill
        visible: root.status === "stale" || root.status === "error"
        width:   2
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        height:  Math.round(parent.height * 0.7)
        color: root.status === "stale" ? Theme.stalePill
             : root.status === "error" ? Theme.errorPill
             : "transparent"

        Behavior on color {
            ColorAnimation { duration: Theme.motionNormal }
        }
    }

    // -----------------------------------------------------------------
    // Status indicator icon — §7.3 channel 3
    //
    // Small glyph in the top-right corner. Unicode placeholders until
    // Phosphor Icons are bundled (see Theme.fontIcon).
    // -----------------------------------------------------------------
    Text {
        id: statusIcon
        visible: root.status === "stale" || root.status === "error"
        anchors.right: parent.right
        anchors.top:   parent.top
        anchors.rightMargin: Theme.spaceXs
        anchors.topMargin:   Theme.spaceXs
        text: root.status === "stale" ? Theme.statusIconStale
            : root.status === "error" ? Theme.statusIconError
            : ""
        color: root.status === "stale" ? Theme.stalePill : Theme.errorPill
        font.pixelSize: Theme.statusIconSize
    }
}
