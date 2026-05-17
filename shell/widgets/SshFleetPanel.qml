// SshFleetPanel — per-host SSH fleet detail (spec §2.10.1).
//
// Opened from the SshDashboard bar widget. One row per configured host:
// status dot (green/yellow/red), display name, associated project, and
// latency. Offline/degraded hosts get a one-click "reconnect" affordance
// that asks the daemon to re-probe immediately (spec §2.10.1).
//
// payload shape == remote::ssh_monitor::SshFleetState:
//   { "hosts": [ { name, display_name, project, reachable,
//                   latency_ms, status, error } ] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ hosts: [] })
    readonly property var hosts: (payload && payload.hosts) || []

    // Emitted with the host's `name` (the stable id, not display_name).
    signal reconnect(string host)

    implicitWidth: 340
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
            text: "SSH fleet"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            visible: root.hosts.length === 0
            text: "no hosts configured"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Column {
            width: parent.width
            spacing: Theme.spaceXs
            Repeater {
                model: root.hosts
                delegate: Rectangle {
                    id: hostRow
                    required property var modelData
                    width: parent.width
                    height: 38
                    radius: 4
                    color: rowHit.containsMouse ? Theme.surfaceRaised : "transparent"

                    readonly property color dotColor:
                        !modelData.reachable ? Theme.error
                        : modelData.status === "degraded" ? Theme.warning
                        : Theme.success

                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: Theme.spaceSm
                        anchors.rightMargin: Theme.spaceSm
                        spacing: Theme.spaceSm

                        Rectangle {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 8; height: 8; radius: 4
                            color: hostRow.dotColor
                        }

                        Column {
                            anchors.verticalCenter: parent.verticalCenter
                            width: parent.width - 8 - latC.width - reconnectBtn.width
                                   - 3 * Theme.spaceSm
                            spacing: 1
                            Text {
                                width: parent.width
                                elide: Text.ElideRight
                                text: modelData.display_name || modelData.name
                                color: Theme.fg
                                font.family: Theme.fontText
                                font.pixelSize: Theme.typeCaption
                                font.weight: Theme.typeBodyEmphasisWeight
                            }
                            Text {
                                width: parent.width
                                elide: Text.ElideRight
                                visible: !!modelData.project || !!modelData.error
                                text: modelData.error
                                      ? modelData.error
                                      : (modelData.project || "")
                                color: modelData.error ? Theme.error : Theme.fgMuted
                                font.family: Theme.fontText
                                font.pixelSize: Theme.typeCaption
                            }
                        }

                        Text {
                            id: latC
                            anchors.verticalCenter: parent.verticalCenter
                            width: 52
                            horizontalAlignment: Text.AlignRight
                            text: modelData.reachable && modelData.latency_ms !== null
                                  ? modelData.latency_ms + "ms" : "—"
                            color: modelData.status === "degraded"
                                   ? Theme.warning : Theme.fgSubtle
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                            font.features: ({ "tnum": 1 })
                        }

                        Text {
                            id: reconnectBtn
                            anchors.verticalCenter: parent.verticalCenter
                            width: visible ? implicitWidth : 0
                            visible: !modelData.reachable
                                     || modelData.status === "degraded"
                            text: "↻"
                            color: Theme.primary
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeBody
                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: (e) => {
                                    e.accepted = true;
                                    root.reconnect(modelData.name);
                                }
                            }
                        }
                    }

                    MouseArea {
                        id: rowHit
                        anchors.fill: parent
                        hoverEnabled: true
                        acceptedButtons: Qt.NoButton
                    }
                }
            }
        }
    }
}
