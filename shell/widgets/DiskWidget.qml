// DiskWidget — renders disk-space usage from telemetry::DiskModule.
//
// state shape (mounts sorted tightest-first by the daemon):
//   {
//     "mounts": [
//       { "path": "/", "total_bytes": 5e11, "used_bytes": 4.6e11,
//         "avail_bytes": 4e10, "used_percent": 92 }
//     ]
//   }
//
// The headline is the tightest mount (mounts[0]); escalation is already
// folded into root.escalation by the daemon, so the wrapper colours
// itself — this widget only chooses an icon and renders the number.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property var mounts: (widgetState && widgetState.mounts) || []
    readonly property var tightest: mounts.length > 0 ? mounts[0] : null
    readonly property int usedPercent: tightest ? (tightest.used_percent || 0) : 0

    function formatBytes(b) {
        if (b === null || b === undefined) return "—";
        const units = ["B", "K", "M", "G", "T"];
        let v = b, i = 0;
        while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
        return (i === 0 ? v : v.toFixed(1)) + units[i];
    }

    // Colour the figure red/amber as it approaches full, but defer to
    // the wrapper's escalation colour once it has taken over (degraded).
    readonly property color usageColor: {
        if (root.degraded) return root.contentColor;
        if (usedPercent >= 92) return Theme.error;
        if (usedPercent >= 85) return Theme.warning;
        return root.contentColor;
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconHardDrive
            color: root.usageColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tightest ? root.tightest.path : "—"
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tightest ? root.usedPercent + "%" : "—"
            color: root.usageColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only" && root.prominence !== "badge"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tightest
                  ? root.formatBytes(root.tightest.avail_bytes) + " free"
                  : ""
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence === "expanded"
        }
    }
}
