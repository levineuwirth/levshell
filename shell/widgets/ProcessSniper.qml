// ProcessSniper — top resource-consuming processes (spec §2.3.5).
//
// Opened from the CPU widget. Each row: click = SIGTERM, Shift+click =
// SIGKILL (the daemon allow-lists those two signals). After a kill the
// daemon re-samples and pushes a fresh ProcessList, so the list
// self-refreshes; the header also has a manual refresh.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ generated_at: "", processes: [] })
    readonly property var procs: (payload && payload.processes) || []

    signal kill(int pid, string signal)
    signal refresh()

    implicitWidth: Math.round(360 * Theme.uiScale)
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

        Row {
            width: parent.width
            Text {
                text: "Top processes"
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeTitle
                font.weight: Theme.typeTitleWeight
            }
            Item { width: parent.width - 1; height: 1 }
        }

        Text {
            text: "Click = terminate · Shift+click = kill"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Text {
            visible: root.procs.length === 0
            text: "no process data"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Column {
            width: parent.width
            spacing: Theme.spaceXs
            Repeater {
                model: root.procs
                delegate: Rectangle {
                    required property var modelData
                    width: parent.width
                    height: Math.round(30 * Theme.uiScale)
                    radius: 4
                    color: rowHit.containsMouse ? Theme.surfaceRaised : "transparent"

                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: Theme.spaceSm
                        anchors.rightMargin: Theme.spaceSm
                        spacing: Theme.spaceSm

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            width: parent.width - cpuT.width - memT.width - 2 * Theme.spaceSm
                            elide: Text.ElideRight
                            text: modelData.name
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                        }
                        Text {
                            id: cpuT
                            anchors.verticalCenter: parent.verticalCenter
                            width: Math.round(52 * Theme.uiScale)
                            horizontalAlignment: Text.AlignRight
                            text: Math.round(modelData.cpu_percent) + "%"
                            color: modelData.cpu_percent >= 60 ? Theme.warning : Theme.fgSubtle
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                            font.features: ({ "tnum": 1 })
                        }
                        Text {
                            id: memT
                            anchors.verticalCenter: parent.verticalCenter
                            width: Math.round(64 * Theme.uiScale)
                            horizontalAlignment: Text.AlignRight
                            text: (modelData.mem_kb / 1024).toFixed(0) + "M"
                            color: Theme.fgSubtle
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                            font.features: ({ "tnum": 1 })
                        }
                    }

                    MouseArea {
                        id: rowHit
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        acceptedButtons: Qt.LeftButton
                        onClicked: (e) => {
                            const sig = (e.modifiers & Qt.ShiftModifier) ? "KILL" : "TERM";
                            root.kill(modelData.pid, sig);
                        }
                    }
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        Text {
            text: "↻ refresh"
            color: Theme.fgSubtle
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: root.refresh()
            }
        }
    }
}
