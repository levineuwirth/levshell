// MemoryWidget — renders the memory state from telemetry::MemoryModule.
//
// state shape:
//   {
//     "total_kb": 61341412,
//     "available_kb": 49607908,
//     "used_kb": 11733504,
//     "used_percent": 19.128,
//     "history": [18.0, ...]
//   }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property real usedPercent: (widgetState && widgetState.used_percent) || 0
    readonly property real totalGb: ((widgetState && widgetState.total_kb) || 0) / 1024.0 / 1024.0
    readonly property real usedGb: ((widgetState && widgetState.used_kb) || 0) / 1024.0 / 1024.0
    readonly property var history: (widgetState && widgetState.history) || []

    // Click toggles the process sniper ranked by memory (spec §2.3.5),
    // mirroring the CPU widget — same shared overlay, re-click
    // collapses, opening it switches the ranking.
    interactive: true
    onClicked: shell.toggleProcessSniper("mem")

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconMemory
            color: root.contentColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Sparkline {
            anchors.verticalCenter: parent.verticalCenter
            height: Theme.iconSize
            width: Math.round(40 * Theme.uiScale)
            values: root.history
            maxValue: 100
            lineColor: root.accentColor
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Math.round(root.usedPercent) + "%"
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.usedGb.toFixed(1) + "/" + root.totalGb.toFixed(0) + "G"
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }
    }
}
