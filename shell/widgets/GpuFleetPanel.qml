// GpuFleetPanel — per-host / per-GPU detail (spec §2.10.4).
//
// Opened from the GpuDashboard bar widget. One section per host; within
// each, one row per GPU: utilization bar, memory used/total, temp.
//
// payload shape == remote::gpu::GpuFleetState:
//   { "hosts": [ { name, display_name, project, status, error,
//                   "gpus": [ { index, name, utilization_percent,
//                               memory_used_mb, memory_total_mb,
//                               temperature_c } ] } ] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ hosts: [] })
    readonly property var hosts: (payload && payload.hosts) || []

    implicitWidth: Math.round(380 * Theme.uiScale)
    implicitHeight: col.implicitHeight + 2 * Theme.panelInnerPadding

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.panelOpacityBattery : Theme.panelOpacity)
    radius: Theme.panelCornerRadius
    border.width: Theme.panelBorderWidth
    border.color: Theme.outline
    antialiasing: true

    layer.enabled: true
    layer.effect: MultiEffect {
        shadowEnabled: true; shadowColor: "#000000"
        blurMax: Theme.panelShadowBlur; shadowBlur: 1.0
        shadowVerticalOffset: Theme.panelShadowOffsetY
        shadowOpacity: Theme.panelShadowOpacity
        autoPaddingEnabled: true
    }

    opacity: 0.0; scale: 0.96
    transformOrigin: Item.TopRight
    states: [
        State { name: "open"; when: root.isOpen
            PropertyChanges { target: root; opacity: 1.0; scale: 1.0 } }
    ]
    transitions: [
        Transition { from: ""; to: "open"
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionFast }
                SpringAnimation { property: "scale"; spring: Theme.springDefault
                    damping: Theme.springDefaultDamping; mass: Theme.springMass; epsilon: 0.005 }
            }
        },
        Transition { from: "open"; to: ""
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionFast }
                SpringAnimation { property: "scale"; spring: Theme.springSnappy
                    damping: Theme.springSnappyDamping; mass: Theme.springMass; epsilon: 0.005 }
            }
        }
    ]

    MouseArea { anchors.fill: parent; onClicked: (e) => e.accepted = true }

    Column {
        id: col
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        Text {
            text: "GPU fleet"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            visible: root.hosts.length === 0
            text: "no GPU hosts configured"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Repeater {
            model: root.hosts
            delegate: Column {
                id: hostSection
                required property var modelData
                width: parent.width
                spacing: Theme.spaceXs

                readonly property color statusColor:
                    modelData.status === "offline" ? Theme.error
                    : modelData.status === "busy" ? Theme.warning
                    : Theme.success

                Row {
                    width: parent.width
                    spacing: Theme.spaceSm
                    Rectangle {
                        anchors.verticalCenter: parent.verticalCenter
                        width: 8; height: 8; radius: 4
                        color: hostSection.statusColor
                    }
                    Text {
                        text: hostSection.modelData.display_name
                              || hostSection.modelData.name
                        color: Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                        font.weight: Theme.typeBodyEmphasisWeight
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: !!hostSection.modelData.project
                        text: hostSection.modelData.project || ""
                        color: Theme.fgMuted
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                    }
                }

                Text {
                    visible: !!hostSection.modelData.error
                    text: hostSection.modelData.error || ""
                    color: Theme.error
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                    font.italic: true
                }

                Repeater {
                    model: hostSection.modelData.gpus || []
                    delegate: Item {
                        id: gpuRow
                        required property var modelData
                        width: hostSection.width
                        height: Math.round(34 * Theme.uiScale)

                        readonly property real memFrac:
                            modelData.memory_total_mb > 0
                            ? modelData.memory_used_mb / modelData.memory_total_mb
                            : 0

                        Column {
                            anchors.fill: parent
                            anchors.leftMargin: Theme.spaceMd
                            spacing: 2

                            Row {
                                width: parent.width
                                spacing: Theme.spaceSm
                                Text {
                                    width: parent.width - utilT.width - Theme.spaceSm
                                    elide: Text.ElideRight
                                    text: "[" + gpuRow.modelData.index + "] "
                                          + gpuRow.modelData.name
                                    color: Theme.fgSubtle
                                    font.family: Theme.fontText
                                    font.pixelSize: Theme.typeCaption
                                }
                                Text {
                                    id: utilT
                                    text: Math.round(gpuRow.modelData.utilization_percent)
                                          + "%  "
                                          + (gpuRow.modelData.memory_used_mb / 1024).toFixed(1)
                                          + "/"
                                          + (gpuRow.modelData.memory_total_mb / 1024).toFixed(0)
                                          + "G  " + gpuRow.modelData.temperature_c + "°"
                                    color: gpuRow.modelData.utilization_percent >= 80
                                           ? Theme.warning : Theme.fgSubtle
                                    font.family: Theme.fontMono
                                    font.pixelSize: Theme.typeCaption
                                    font.features: ({ "tnum": 1 })
                                }
                            }

                            // Utilization bar.
                            Rectangle {
                                width: parent.width
                                height: 3
                                radius: 1.5
                                color: Theme.surfaceRaised
                                Rectangle {
                                    height: parent.height
                                    radius: parent.radius
                                    width: parent.width
                                           * Math.max(0, Math.min(1,
                                             gpuRow.modelData.utilization_percent / 100))
                                    color: gpuRow.modelData.utilization_percent >= 80
                                           ? Theme.warning : Theme.primary
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
