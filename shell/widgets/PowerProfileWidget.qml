// PowerProfileWidget — active power profile from
// telemetry::PowerProfilesModule (spec §2.3.2). Click cycles
// power-saver → balanced → performance (the daemon decides the order).
//
// state shape:
//   { "active": "balanced",
//     "available": ["power-saver", "balanced", "performance"] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property string active: (widgetState && widgetState.active) || ""
    readonly property var available: (widgetState && widgetState.available) || []

    readonly property string icon: {
        switch (active) {
            case "power-saver": return Theme.iconLeaf;
            case "performance": return Theme.iconLightning;
            default:            return Theme.iconGauge; // balanced / unknown
        }
    }

    // Short label — "saver" / "balanced" / "perf" — the bar is tight.
    readonly property string label: {
        switch (active) {
            case "power-saver": return "saver";
            case "performance": return "perf";
            case "balanced":    return "balanced";
            default:            return active;
        }
    }

    readonly property color profileColor: {
        if (root.degraded) return root.contentColor;
        if (active === "performance") return Theme.warning;
        if (active === "power-saver") return Theme.success;
        return root.contentColor;
    }

    interactive: active !== "" && available.length > 1
    onClicked: shell.sendPowerProfileCycle()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.icon
            color: root.profileColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.label
            color: root.profileColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            visible: root.prominence !== "icon_only" && root.prominence !== "badge"
        }
    }
}
