// CpuWidget — renders the cpu state from telemetry::CpuModule.
//
// state shape:
//   { "usage_percent": 40.0, "load_avg_1": 2.84 }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property real usagePercent: (widgetState && widgetState.usage_percent) || 0
    readonly property real loadAvg1: (widgetState && widgetState.load_avg_1) || 0

    // Phase 1.6: simple threshold-based coloring. A later phase will
    // route this through the urgency/escalation grammar (spec §9).
    readonly property color usageColor: {
        if (root.degraded) return root.contentColor;
        if (usagePercent > 85) return Theme.error;
        if (usagePercent > 60) return Theme.warning;
        return root.contentColor;
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "⚙"
            color: root.usageColor
            font.pixelSize: Theme.iconSizeFull
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Math.round(root.usagePercent) + "%"
            color: root.usageColor
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
