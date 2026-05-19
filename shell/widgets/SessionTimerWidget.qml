// SessionTimerWidget — Pomodoro / focus-session pill (spec §2.2.1).
//
// "A compact pill in the bar showing elapsed focus time." Click toggles
// the timer (idle→start, running→pause, paused→resume) by routing a
// `session-timer toggle` widget action to the daemon's SessionTimerModule
// via the M1.1 passthrough. Phase tints the pill; the dot blinks while
// paused so a frozen timer reads as paused, not stopped.
//
// state shape (from session_timer::SessionTimerModule):
//   { phase: "idle"|"work"|"break", paused: bool,
//     elapsed_secs: int, planned_secs: int, work_intervals: int }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property string phase: (widgetState && widgetState.phase) || "idle"
    readonly property bool paused: (widgetState && widgetState.paused) === true
    readonly property int elapsed: (widgetState && widgetState.elapsed_secs) || 0
    readonly property bool running: phase === "work" || phase === "break"

    function mmss(s) {
        const m = Math.floor(s / 60);
        const sec = s % 60;
        return m + ":" + (sec < 10 ? "0" : "") + sec;
    }

    readonly property color phaseColor:
        root.escalated ? root.contentColor
        : root.paused ? Theme.fgMuted
        : root.phase === "work" ? Theme.primary
        : root.phase === "break" ? Theme.success
        : Theme.fgSubtle

    interactive: true
    onClicked: shell.sendTimerToggle()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            id: phaseGlyph
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconClockCountdown
            color: root.phaseColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize

            // Blink while paused so a frozen timer reads as paused, not
            // stopped. When the animation stops (resume/stop) it can
            // leave opacity mid-cycle, so restore it explicitly.
            SequentialAnimation {
                id: pausedBlink
                running: root.paused
                loops: Animation.Infinite
                NumberAnimation { target: phaseGlyph; property: "opacity"
                    to: 0.35; duration: Theme.motionSlow }
                NumberAnimation { target: phaseGlyph; property: "opacity"
                    to: 1.0;  duration: Theme.motionSlow }
                onRunningChanged: if (!running) phaseGlyph.opacity = 1.0
            }
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.running ? root.mmss(root.elapsed) : "focus"
            color: root.phaseColor
            font.family: root.running ? Theme.fontMono : Theme.fontText
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            // Idle → just the clock glyph (WidgetWrapper auto-shrinks to
            // icon width); running/paused → glyph + mm:ss. So the timer
            // is a quiet icon until a session is actually going.
            visible: root.prominence !== "icon_only" && root.running
        }
    }
}
