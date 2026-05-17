// RemoteJobsPanel — per-host / per-job SLURM detail (spec §2.10.3).
//
// Opened from the RemoteJobsWidget bar widget. One section per host;
// within each, one row per job: name, state (colour-coded), elapsed /
// limit, and the scheduler reason for pending jobs.
//
// payload shape == remote::jobs::RemoteJobsState:
//   { "hosts": [ { name, display_name, project, status, error,
//                   "jobs": [ { id, name, state, reason,
//                               time_used, time_limit } ] } ] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ hosts: [] })
    readonly property var hosts: (payload && payload.hosts) || []

    implicitWidth: 400
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
            text: "Remote jobs"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            visible: root.hosts.length === 0
            text: "no job hosts configured"
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
                    : modelData.status === "running" ? Theme.success
                    : Theme.fgSubtle

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

                Text {
                    visible: !hostSection.modelData.error
                             && (hostSection.modelData.jobs || []).length === 0
                    text: "no jobs in queue"
                    color: Theme.fgMuted
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                    font.italic: true
                }

                Repeater {
                    model: hostSection.modelData.jobs || []
                    delegate: Item {
                        id: jobRow
                        required property var modelData
                        width: hostSection.width
                        height: 32

                        readonly property color stateColor:
                            modelData.state === "RUNNING" ? Theme.success
                            : modelData.state === "PENDING" ? Theme.warning
                            : Theme.fgSubtle

                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: Theme.spaceMd
                            spacing: Theme.spaceSm

                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                width: 64
                                elide: Text.ElideRight
                                text: jobRow.modelData.state
                                color: jobRow.stateColor
                                font.family: Theme.fontMono
                                font.pixelSize: Theme.typeCaption
                            }
                            Column {
                                anchors.verticalCenter: parent.verticalCenter
                                width: parent.width - 64 - timeT.width - 2 * Theme.spaceSm
                                spacing: 1
                                Text {
                                    width: parent.width
                                    elide: Text.ElideRight
                                    text: jobRow.modelData.name
                                          + "  #" + jobRow.modelData.id
                                    color: Theme.fg
                                    font.family: Theme.fontText
                                    font.pixelSize: Theme.typeCaption
                                }
                                Text {
                                    width: parent.width
                                    elide: Text.ElideRight
                                    visible: jobRow.modelData.reason
                                             && jobRow.modelData.reason !== "None"
                                    text: jobRow.modelData.reason
                                    color: Theme.fgMuted
                                    font.family: Theme.fontText
                                    font.pixelSize: Theme.typeCaption
                                }
                            }
                            Text {
                                id: timeT
                                anchors.verticalCenter: parent.verticalCenter
                                width: 96
                                horizontalAlignment: Text.AlignRight
                                text: jobRow.modelData.time_used
                                      + " / " + jobRow.modelData.time_limit
                                color: Theme.fgSubtle
                                font.family: Theme.fontMono
                                font.pixelSize: Theme.typeCaption
                                font.features: ({ "tnum": 1 })
                            }
                        }
                    }
                }
            }
        }
    }
}
