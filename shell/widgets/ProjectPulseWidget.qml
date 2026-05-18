// ProjectPulseWidget — project activity pulse (spec §2.9.4).
//
// Compact: a projects glyph + count of projects active today; tinted
// warning when any project is dormant past threshold. Click opens
// ProjectPulsePanel (per-project dashboard + upcoming deadlines).
//
// state (from project_pulse::ProjectPulseModule):
//   { active_today, dormant, projects:[...], deadlines:[...] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int activeToday: (widgetState && widgetState.active_today) || 0
    readonly property int dormant: (widgetState && widgetState.dormant) || 0
    readonly property var projects: (widgetState && widgetState.projects) || []

    readonly property color pulseColor:
        root.escalated ? root.contentColor
        : root.dormant > 0 ? Theme.warning
        : root.contentColor

    interactive: true
    onClicked: shell.toggleProjectPulse()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm
        visible: root.projects.length > 0

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconSquaresFour
            color: root.pulseColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.activeToday + " active"
            color: root.pulseColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            visible: root.prominence !== "icon_only"
        }
    }
}
