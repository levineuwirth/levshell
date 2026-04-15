// BatteryWidget — renders the battery state from telemetry::BatteryModule.
//
// state shape:
//   {
//     "percent": 28,
//     "status": "discharging",   // charging|discharging|full|not_charging|unknown
//     "on_battery": true,
//     "time_remaining_seconds": 3876
//   }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int percent: (widgetState && widgetState.percent) || 0
    readonly property string batteryStatus: (widgetState && widgetState.status) || "unknown"
    readonly property int timeRemaining: (widgetState && widgetState.time_remaining_seconds) || 0
    readonly property bool charging: batteryStatus === "charging"
    readonly property bool full: batteryStatus === "full"

    readonly property color batteryColor: {
        if (root.degraded) return root.contentColor;
        if (charging) return Theme.primary;
        if (percent <= 10) return Theme.error;
        if (percent <= 25) return Theme.warning;
        return root.contentColor;
    }

    readonly property string icon: {
        if (charging) return "⚡";
        if (full)      return "▰";
        if (percent > 66) return "▰";
        if (percent > 33) return "▱";
        return "▱";
    }

    readonly property string formattedTime: {
        if (timeRemaining <= 0) return "";
        const h = Math.floor(timeRemaining / 3600);
        const m = Math.floor((timeRemaining % 3600) / 60);
        return h + "h" + String(m).padStart(2, '0');
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.icon
            color: root.batteryColor
            font.pixelSize: Theme.iconSizeFull
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.percent + "%"
            color: root.batteryColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.formattedTime
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: (root.prominence === "visible" || root.prominence === "expanded")
                     && root.formattedTime.length > 0
        }
    }
}
