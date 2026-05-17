// SshDashboard — SSH connection fleet status (spec §2.10.1).
//
// Compact bar widget: a network glyph + "reachable/total" count, tinted
// by the worst host state (green all-up, yellow degraded, red any-down).
// Click toggles the SshFleetPanel dropdown for per-host detail + the
// one-click reconnect action (spec §2.10.1).
//
// state shape (from remote::ssh_monitor::SshFleetState):
//   { "hosts": [ { "name", "display_name", "project", "reachable",
//                   "latency_ms", "status", "error" } ] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property var hosts: (widgetState && widgetState.hosts) || []
    readonly property int total: hosts.length
    readonly property int up: {
        let n = 0;
        for (let i = 0; i < hosts.length; i++)
            if (hosts[i].reachable) n++;
        return n;
    }
    readonly property bool anyOffline: up < total
    readonly property bool anyDegraded: {
        for (let i = 0; i < hosts.length; i++)
            if (hosts[i].status === "degraded") return true;
        return false;
    }

    // Worst-state tint. Escalation/health (handled by WidgetWrapper) wins
    // over this, so only colour when the wrapper isn't already overriding.
    readonly property color fleetColor:
        root.anyOffline ? Theme.error
        : root.anyDegraded ? Theme.warning
        : root.contentColor

    interactive: true
    onClicked: shell.toggleSshFleet()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconNetwork
            color: root.escalated ? root.contentColor : root.fleetColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.up + "/" + root.total
            color: root.escalated ? root.contentColor : root.fleetColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only" && root.total > 0
        }
    }
}
