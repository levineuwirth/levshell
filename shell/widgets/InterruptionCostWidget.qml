// InterruptionCostWidget — spec §2.12.3.
//
// Subtle pill rendered next to the workspace indicator when the user
// returns to a workspace after a meaningful absence. The daemon's
// interruption module publishes a fresh `shown_at_ms` every time it
// wants the pill surfaced; this widget watches that tripwire, animates
// in, holds for `displayDurationMs`, then animates out. When dormant
// the wrapper collapses to zero width so the bar doesn't reserve space.
//
// The widget intentionally ignores its `prominence` input — re-entry
// awareness is not a cascade concern, it's a transient signal owned
// by its own module.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: null

    readonly property real   shownAtMs:   widgetState ? (widgetState.shown_at_ms || 0) : 0
    readonly property int    awaySeconds: widgetState ? (widgetState.away_seconds || 0) : 0
    readonly property string workspace:   widgetState ? (widgetState.workspace    || "") : ""

    readonly property int displayDurationMs: 8000

    // State machine: `active` is the high-level on/off, `contentOpacity`
    // is the animated fade. Transitions are driven by `shownAtMs`
    // changes and `hideTimer`.
    property bool active: false
    property real contentOpacity: 0.0

    // Override the wrapper bindings — this widget's visibility is fully
    // owned by its own state (not cascade or widget health). targetWidth
    // 0 collapses it in the bar Row.
    targetWidth: active
        ? Math.ceil(content.implicitWidth) + 2 * Theme.widgetPaddingH
        : 0
    visible: active || contentOpacity > 0.01
    opacity: 1.0   // neutralize the wrapper's prominence-based opacity

    onShownAtMsChanged: {
        if (shownAtMs <= 0) return;
        // Only show if the update is fresh — a replay after a shell
        // reconnect (stale shown_at_ms) should stay dormant.
        const age = Date.now() - shownAtMs;
        if (age < 0 || age > displayDurationMs) return;
        active = true;
        contentOpacity = 1.0;
        hideTimer.restart();
    }

    Timer {
        id: hideTimer
        interval: root.displayDurationMs
        onTriggered: root.contentOpacity = 0.0
    }

    Behavior on contentOpacity {
        NumberAnimation {
            duration: Theme.motionNormal
            easing.type: Easing.OutCubic
        }
    }

    onContentOpacityChanged: {
        if (contentOpacity < 0.01) active = false;
    }

    function formatAway(secs) {
        if (secs < 60)   return secs + "s";
        if (secs < 3600) return Math.round(secs / 60) + "m";
        const h = Math.floor(secs / 3600);
        const m = Math.round((secs % 3600) / 60);
        return m > 0 ? h + "h " + m + "m" : h + "h";
    }

    Row {
        id: content
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        spacing: Theme.spaceXs
        opacity: root.contentOpacity

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "← away"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.formatAway(root.awaySeconds)
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
        }
    }
}
