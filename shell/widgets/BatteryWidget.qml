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
        if (root.escalated) return root.contentColor;
        if (root.degraded)  return root.contentColor;
        if (charging)       return Theme.primary;
        return root.contentColor;
    }

    // Phosphor battery icons — six-level granularity per §8.
    // Charging shows the lightning-bolt overlay icon regardless of
    // current percent (matches the user's mental model of "it's
    // charging, not draining").
    readonly property string icon: {
        if (charging)     return Theme.iconBatteryCharging;
        if (full)         return Theme.iconBatteryFull;
        if (percent >= 90) return Theme.iconBatteryFull;
        if (percent >= 70) return Theme.iconBatteryHigh;
        if (percent >= 40) return Theme.iconBatteryMedium;
        if (percent >= 15) return Theme.iconBatteryLow;
        return Theme.iconBatteryEmpty;
    }

    readonly property string formattedTime: {
        if (timeRemaining <= 0) return "";
        const h = Math.floor(timeRemaining / 3600);
        const m = Math.floor((timeRemaining % 3600) / 60);
        return h + "h" + String(m).padStart(2, '0');
    }

    MouseArea {
        id: clickArea
        anchors.fill: parent
        z: 10
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: shell.toggleQuickSettings()
    }

    // Faint hover wash so the click affordance isn't pure-cursor-only.
    Rectangle {
        anchors.fill: parent
        radius: 4
        color: Theme.fg
        opacity: clickArea.containsMouse ? 0.06 : 0.0
        z: -1
        Behavior on opacity { NumberAnimation { duration: Theme.motionFast } }
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.icon
            color: root.batteryColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
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
