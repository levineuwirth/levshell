// LatexStatusWidget — LaTeX build status pill (spec §2.9.10).
//
// Quiet until important (§1.3): nothing renders while idle. While a TeX
// engine runs it shows a pulsing "TeX" with the document glyph; on
// completion a green check or a red "!". Clicking the error state opens
// the .log (routed as `latex-status open_log` via the M1.1 passthrough).
//
// state (from latex_status::LatexStatusModule):
//   { phase: "idle"|"compiling"|"success"|"error", log_path, error }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property string phase: (widgetState && widgetState.phase) || "idle"
    readonly property string err: (widgetState && widgetState.error) || ""
    readonly property bool isError: phase === "error"

    // Only the error state is actionable (opens the log).
    interactive: isError
    onClicked: if (root.isError) shell.sendLatexOpenLog()

    readonly property color phaseColor:
        root.escalated ? root.contentColor
        : root.phase === "error" ? Theme.error
        : root.phase === "success" ? Theme.success
        : root.contentColor

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm
        visible: root.phase !== "idle"

        Text {
            id: glyph
            anchors.verticalCenter: parent.verticalCenter
            text: root.phase === "error" ? Theme.iconWarning : Theme.iconNote
            color: root.phaseColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize

            // Pulse while compiling.
            SequentialAnimation {
                running: root.phase === "compiling"
                loops: Animation.Infinite
                NumberAnimation { target: glyph; property: "opacity"
                    to: 0.4; duration: Theme.motionSlow }
                NumberAnimation { target: glyph; property: "opacity"
                    to: 1.0; duration: Theme.motionSlow }
                onRunningChanged: if (!running) glyph.opacity = 1.0
            }
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.phase === "compiling" ? "TeX…"
                  : root.phase === "success" ? "TeX ✓"
                  : root.phase === "error" ? "TeX !"
                  : ""
            color: root.phaseColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            visible: root.prominence !== "icon_only"
        }
    }
}
