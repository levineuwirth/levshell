// RemoteJobsWidget — remote SLURM job monitor (spec §2.10.3).
//
// Compact bar widget: a countdown glyph + running/total job count
// across all hosts, tinted by host state (offline → red, running →
// accent). Click opens RemoteJobsPanel for per-job detail.
//
// state shape (from remote::jobs::RemoteJobsState):
//   { "hosts": [ { name, display_name, project, status, error,
//                   "jobs": [ { id, name, state, reason,
//                               time_used, time_limit } ] } ] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property var hosts: (widgetState && widgetState.hosts) || []

    readonly property int totalJobs: {
        let n = 0;
        for (let h = 0; h < hosts.length; h++)
            n += (hosts[h].jobs || []).length;
        return n;
    }
    readonly property int runningJobs: {
        let n = 0;
        for (let h = 0; h < hosts.length; h++) {
            const jobs = hosts[h].jobs || [];
            for (let j = 0; j < jobs.length; j++)
                if (jobs[j].state === "RUNNING") n++;
        }
        return n;
    }
    readonly property bool anyOffline: {
        for (let i = 0; i < hosts.length; i++)
            if (hosts[i].status === "offline") return true;
        return false;
    }

    readonly property color jobsColor:
        root.anyOffline ? Theme.error
        : root.runningJobs > 0 ? Theme.primary
        : root.contentColor

    interactive: true
    onClicked: shell.toggleRemoteJobs()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconClockCountdown
            color: root.escalated ? root.contentColor : root.jobsColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.runningJobs + "/" + root.totalJobs
            color: root.escalated ? root.contentColor : root.jobsColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only" && root.totalJobs > 0
        }
    }
}
