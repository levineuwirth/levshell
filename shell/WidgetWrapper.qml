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
    // Urgency level per spec design §9, computed by the daemon's
    // `EscalationTracker`. One of "ambient" | "aware" | "attention" |
    // "critical". Default "ambient" renders exactly as pre-§9 widgets.
    property string escalation: "ambient"

    // Opt-in click affordance (spec §7.1). When true, the wrapper renders
    // a full-bleed hover wash *behind* the content and a click target
    // *above* it, and emits `clicked()`. This chrome lives on `root`, not
    // in `contentHolder`, so it never enters the `childrenRect`
    // measurement that drives `targetWidth` — keeping that binding
    // loop-free even for interactive widgets. Widgets must NOT inject
    // their own full-bleed MouseArea into the content slot.
    property bool interactive: false
    signal clicked()

    // Default content slot — widgets assign children that get parented
    // to `contentHolder`. This keeps the health chrome (pill + status
    // icon) outside the widget's layout so they don't collide with it.
    default property alias content: contentHolder.data

    // =================================================================
    // OUTPUTS — bound by child widgets via `root.contentColor` etc.
    //
    // Stacking rule (spec design §10.1): escalation > health state.
    // When escalation ≥ Attention, the widget renders in the
    // full-saturation state color regardless of health status — the
    // user needs to see critical info. Below Attention, we fall back
    // to the health-state treatment.
    // =================================================================
    readonly property bool degraded: status === "stale" || status === "error"
    readonly property bool escalated: escalation === "attention" || escalation === "critical"

    readonly property color contentColor: {
        if (escalation === "critical")  return Theme.error;
        if (escalation === "attention") return Theme.warning;
        return degraded ? Theme.fgMuted : Theme.fg;
    }
    readonly property color accentColor: {
        if (escalation === "critical")  return Theme.error;
        if (escalation === "attention") return Theme.warning;
        return degraded ? Theme.outline : Theme.primary;
    }
    readonly property color subtleColor: {
        if (escalated) return contentColor;
        return degraded ? Theme.fgMuted : Theme.fgSubtle;
    }

    // Target width is content-driven per §5 / §7: the wrapper measures
    // its child content and adds `widgetPaddingH` on each side.
    // Fixed tokens are used only for the two prominence levels that
    // don't render child content at all:
    //   • "hidden" → zero width (removed from layout).
    //   • "badge"  → the 6px dot rectangle rendered by the wrapper
    //     itself; children are not shown.
    //
    // `contentHolder.childrenRect.width` is the union of all visible
    // descendants' bounding boxes. Widgets anchor their Row/Column at
    // `contentHolder.left` so `childrenRect.x == 0` and
    // `childrenRect.width == Row.implicitWidth`, making this binding
    // loop-free. Width changes animate through `Behavior on width`.
    property int targetWidth: {
        if (prominence === "hidden") return Theme.widthHidden;
        if (prominence === "badge")  return Theme.widthBadge;
        return Math.ceil(contentHolder.childrenRect.width)
               + 2 * Theme.widgetPaddingH;
    }

    readonly property bool isBadge: prominence === "badge"
    readonly property bool isVisibleAtAll:
        prominence !== "hidden" && status !== "unavailable"

    implicitWidth:  targetWidth
    implicitHeight: Theme.barHeight
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
    // Interactive hover wash — §7.1 click affordance, channel 1.
    //
    // Sits *below* contentHolder (declared first) so the wash renders
    // behind text/icons. Fills `root`, not `contentHolder`, so it stays
    // out of the width measurement.
    // -----------------------------------------------------------------
    Rectangle {
        anchors.fill: parent
        radius: 4
        color: Theme.fg
        visible: root.interactive
        opacity: root.interactive && clickArea.containsMouse ? 0.06 : 0.0
        Behavior on opacity { NumberAnimation { duration: Theme.motionFast } }
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
        anchors.leftMargin:  Theme.widgetPaddingH
        anchors.rightMargin: Theme.widgetPaddingH
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
    // Left-edge pill — spec design §9 + §7.3 channel 1.
    //
    // Two overlapping visual systems share this slot:
    //
    //   • Health pill: muted stalePill/errorPill tokens (§7.3) when
    //     the widget's data is stale or the module errored.
    //   • Escalation pill: FULL-saturation warning/error (§9 rule 6)
    //     when urgency crosses Attention or Critical.
    //
    // Per §10.1 stacking, escalation wins when both would apply —
    // a critical widget with stale data still shows critical red.
    // -----------------------------------------------------------------
    Rectangle {
        id: statusPill
        visible: root.escalated || root.status === "stale" || root.status === "error"
        width:   2
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        height:  Math.round(parent.height * 0.7)
        color: {
            if (root.escalation === "critical")  return Theme.error;
            if (root.escalation === "attention") return Theme.warning;
            if (root.status === "stale") return Theme.stalePill;
            if (root.status === "error") return Theme.errorPill;
            return "transparent";
        }

        Behavior on color {
            ColorAnimation { duration: Theme.motionNormal }
        }
    }

    // -----------------------------------------------------------------
    // One-time Critical flash — spec design §9 example for Critical.
    //
    // A brief full-saturation error pulse overlays the entire widget
    // on the tick it enters Critical, then settles. Never repeats
    // while the widget stays at Critical; a drop + re-entry fires
    // again. Implemented as an opacity envelope on a rectangle that
    // sits above `contentHolder` but below the status icon.
    // -----------------------------------------------------------------
    Rectangle {
        id: criticalFlash
        anchors.fill: parent
        color: Theme.error
        opacity: 0.0
        radius: 2
        visible: opacity > 0.01

        SequentialAnimation {
            id: flashAnim
            NumberAnimation {
                target: criticalFlash
                property: "opacity"
                from: 0.0
                to: 0.45
                duration: Theme.motionFast
                easing.type: Easing.OutCubic
            }
            NumberAnimation {
                target: criticalFlash
                property: "opacity"
                to: 0.0
                duration: Theme.motionSlow
                easing.type: Easing.OutCubic
            }
        }

        Connections {
            target: root
            function onEscalationChanged() {
                if (root.escalation === "critical") {
                    flashAnim.restart();
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Status indicator icon — §7.3 channel 3
    //
    // Small Phosphor glyph in the top-right corner. Rendered via the
    // bundled Phosphor font (`Theme.fontIcon`) at the spec's 10px
    // `statusIconSize`.
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
        font.family:    Theme.fontIcon
        font.pixelSize: Theme.statusIconSize
    }

    // -----------------------------------------------------------------
    // Click target — §7.1. Declared last so it sits above all content
    // and chrome. Fills `root`, so (like the hover wash) it is never
    // measured by `targetWidth`. Disabled when not interactive so it
    // neither captures events nor changes the cursor.
    // -----------------------------------------------------------------
    MouseArea {
        id: clickArea
        anchors.fill: parent
        enabled: root.interactive
        hoverEnabled: root.interactive
        cursorShape: Qt.PointingHandCursor
        onClicked: root.clicked()
    }
}
