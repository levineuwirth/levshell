// NetworkWidget — renders the network state from telemetry::NetworkModule.
//
// state shape:
//   {
//     "interfaces": [
//       { "name": "wlan0", "rx_bps": 1024, "tx_bps": 256, "quality_percent": 75 },
//       { "name": "eth0",  "rx_bps": 0,    "tx_bps": 0,    "quality_percent": null }
//     ],
//     "latency_ms": 42,
//     "quality": "good"   // good | fair | poor | down | null (probe off)
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

    // Phosphor network icon — wifi signal tiers when quality metadata
    // is present (wireless interface), generic network plug for wired
    // or metadata-free interfaces, slash when no primary connection.
    readonly property string icon: {
        if (!primary) return Theme.iconWifiSlash;
        const q = primary.quality_percent;
        if (q === null || q === undefined) return Theme.iconNetwork;
        if (q < 33) return Theme.iconWifiLow;
        if (q < 66) return Theme.iconWifiMedium;
        return Theme.iconWifiHigh;
    }

    // End-to-end reachability probe (spec §2.3.3). Distinct from the
    // wifi-association bars above: a full-signal link behind a dead
    // uplink still reads "poor"/"down".
    readonly property string linkQuality: (widgetState && widgetState.quality) || ""
    readonly property var latencyMs: widgetState ? widgetState.latency_ms : null

    readonly property color dotColor: {
        switch (linkQuality) {
            case "good": return Theme.success;
            case "fair": return Theme.warning;
            case "poor":
            case "down": return Theme.error;
            default:      return "transparent";
        }
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

        Item {
            anchors.verticalCenter: parent.verticalCenter
            width: icon.width
            height: icon.height

            Text {
                id: icon
                text: root.icon
                color: root.qualityColor
                font.family:    Theme.fontIcon
                font.pixelSize: Theme.iconSize
            }

            // Reachability dot, bottom-right of the wifi/wired glyph.
            // Hidden entirely when the probe is disabled (transparent).
            Rectangle {
                visible: root.linkQuality !== ""
                width: 6
                height: 6
                radius: 3
                color: root.dotColor
                anchors.right: icon.right
                anchors.bottom: icon.bottom
                border.width: 1
                border.color: Theme.bg
            }
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

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.linkQuality === "down"
                  ? "offline"
                  : (root.latencyMs !== null && root.latencyMs !== undefined
                     ? root.latencyMs + "ms" : "")
            color: root.dotColor === "transparent" ? root.subtleColor : root.dotColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence === "expanded" && root.linkQuality !== ""
        }
    }
}
