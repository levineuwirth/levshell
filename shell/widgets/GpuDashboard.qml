// GpuDashboard — remote GPU utilization fleet (spec §2.10.4).
//
// Compact bar widget: a compute glyph + peak utilization across all
// GPUs on all hosts, tinted by the worst host state (busy → yellow,
// offline → red). Click opens GpuFleetPanel for per-GPU detail.
//
// state shape (from remote::gpu::GpuFleetState):
//   { "hosts": [ { name, display_name, project, status, error,
//                   "gpus": [ { index, name, utilization_percent,
//                               memory_used_mb, memory_total_mb,
//                               temperature_c } ] } ] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property var hosts: (widgetState && widgetState.hosts) || []

    // Peak utilization across every GPU on every host (0 when none).
    readonly property real peakUtil: {
        let peak = 0;
        for (let h = 0; h < hosts.length; h++) {
            const gpus = hosts[h].gpus || [];
            for (let g = 0; g < gpus.length; g++)
                if (gpus[g].utilization_percent > peak)
                    peak = gpus[g].utilization_percent;
        }
        return peak;
    }
    readonly property int gpuCount: {
        let n = 0;
        for (let h = 0; h < hosts.length; h++)
            n += (hosts[h].gpus || []).length;
        return n;
    }
    readonly property bool anyOffline: {
        for (let i = 0; i < hosts.length; i++)
            if (hosts[i].status === "offline") return true;
        return false;
    }
    readonly property bool anyBusy: {
        for (let i = 0; i < hosts.length; i++)
            if (hosts[i].status === "busy") return true;
        return false;
    }

    readonly property color fleetColor:
        root.anyOffline ? Theme.error
        : root.anyBusy ? Theme.warning
        : root.contentColor

    interactive: true
    onClicked: shell.toggleGpuFleet()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            // No dedicated GPU glyph in the bundle; the CPU/compute
            // glyph reads correctly for "accelerator utilization".
            text: Theme.iconCpu
            color: root.escalated ? root.contentColor : root.fleetColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Math.round(root.peakUtil) + "%"
            color: root.escalated ? root.contentColor : root.fleetColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only" && root.gpuCount > 0
        }
    }
}
