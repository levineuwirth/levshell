// NetworkWidget — renders the network state from telemetry::NetworkModule.
//
// state shape:
//   {
//     "interfaces": [
//       { "name": "wlan0", "rx_bps": 1024, "tx_bps": 256, "quality_percent": 75 },
//       { "name": "eth0",  "rx_bps": 0,    "tx_bps": 0,    "quality_percent": null }
//     ]
//   }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property var interfaces: (widgetState && widgetState.interfaces) || []

    readonly property var primary: {
        if (interfaces.length === 0) return null;
        for (let i = 0; i < interfaces.length; i++) {
            if (interfaces[i].quality_percent !== null
                && interfaces[i].quality_percent !== undefined) {
                return interfaces[i];
            }
        }
        for (let i = 0; i < interfaces.length; i++) {
            if ((interfaces[i].rx_bps || 0) + (interfaces[i].tx_bps || 0) > 0) {
                return interfaces[i];
            }
        }
        return interfaces[0];
    }

    readonly property int totalBps: {
        if (!primary) return 0;
        return (primary.rx_bps || 0) + (primary.tx_bps || 0);
    }

    function formatBps(bps) {
        if (bps <= 0) return "idle";
        if (bps < 1024) return bps + "B/s";
        if (bps < 1024 * 1024) return (bps / 1024).toFixed(0) + "K/s";
        return (bps / 1024 / 1024).toFixed(1) + "M/s";
    }

    readonly property string icon: {
        if (!primary) return "⇅";
        if (primary.quality_percent !== null && primary.quality_percent !== undefined) return "⎌";
        return "⇅";
    }

    readonly property color qualityColor: {
        if (root.degraded) return root.contentColor;
        if (!primary || primary.quality_percent === null
            || primary.quality_percent === undefined) return root.contentColor;
        const q = primary.quality_percent;
        if (q < 30) return Theme.error;
        if (q < 60) return Theme.warning;
        return root.contentColor;
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.icon
            color: root.qualityColor
            font.pixelSize: Theme.iconSizeFull
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.primary ? root.primary.name : "—"
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.formatBps(root.totalBps)
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only" && root.prominence !== "badge"
        }
    }
}
