// CpuWidget — renders the cpu state from telemetry::CpuModule.
//
// state shape:
//   { "usage_percent": 40.0, "load_avg_1": 2.84, "history": [12.0, ...] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property real usagePercent: (widgetState && widgetState.usage_percent) || 0
    readonly property real loadAvg1: (widgetState && widgetState.load_avg_1) || 0
    readonly property var history: (widgetState && widgetState.history) || []

    // Click opens the process sniper (spec §2.3.5).
    interactive: true
    onClicked: shell.openProcessSniper()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconCpu
            color: root.contentColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Sparkline {
            anchors.verticalCenter: parent.verticalCenter
            height: Theme.iconSize
            width: 40
            values: root.history
            maxValue: 100
            lineColor: root.accentColor
            // Spec §2.3.1: trend visualization belongs to the expanded
            // states, not the icon-only resting widget.
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Math.round(root.usagePercent) + "%"
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "load " + root.loadAvg1.toFixed(2)
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }
    }
}
