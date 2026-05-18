// ProjectPulsePanel — project dashboard + deadline tracker
// (spec §2.9.4, §2.9.12).
//
// Two sections: per-project rows (status dot, name, last-active, focus
// hours, open-question count; dormant projects dimmed) and an upcoming-
// deadlines list colour-coded by urgency (overdue = error, ≤3d =
// warning, else subtle). Monitor-only — no row actions.
//
// payload == project_pulse state:
//   { active_today, dormant,
//     projects:[{name,status,active_today,dormant,last_active,
//                focus_secs,open_questions}],
//     deadlines:[{title,due,kind,overdue}] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ active_today: 0, dormant: 0, projects: [], deadlines: [] })
    readonly property var projects: (payload && payload.projects) || []
    readonly property var deadlines: (payload && payload.deadlines) || []

    function relTime(iso) {
        if (!iso) return "never";
        const then = new Date(iso).getTime();
        if (isNaN(then)) return "never";
        const d = (Date.now() - then) / 1000;
        const a = Math.abs(d);
        let s;
        if (a < 3600) s = Math.round(a / 60) + "m";
        else if (a < 86400) s = Math.round(a / 3600) + "h";
        else s = Math.round(a / 86400) + "d";
        return d >= 0 ? s + " ago" : "in " + s;
    }

    implicitWidth: 380
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
            text: "Projects"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            visible: root.projects.length === 0
            text: "no projects registered"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Column {
            width: parent.width
            spacing: Theme.spaceXs
            Repeater {
                model: root.projects
                delegate: Item {
                    id: prow
                    required property var modelData
                    width: parent.width
                    height: 36
                    opacity: prow.modelData.dormant ? 0.55 : 1.0

                    Row {
                        anchors.fill: parent
                        spacing: Theme.spaceSm
                        Rectangle {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 8; height: 8; radius: 4
                            color: prow.modelData.active_today ? Theme.success
                                   : prow.modelData.dormant ? Theme.warning
                                   : Theme.fgSubtle
                        }
                        Column {
                            anchors.verticalCenter: parent.verticalCenter
                            width: parent.width - 8 - metaT.width - 2 * Theme.spaceSm
                            spacing: 1
                            Text {
                                width: parent.width
                                elide: Text.ElideRight
                                text: prow.modelData.name
                                color: Theme.fg
                                font.family: Theme.fontText
                                font.pixelSize: Theme.typeCaption
                                font.weight: Theme.typeBodyEmphasisWeight
                            }
                            Text {
                                width: parent.width
                                elide: Text.ElideRight
                                text: prow.modelData.status
                                      + "  ·  " + root.relTime(prow.modelData.last_active)
                                      + (prow.modelData.open_questions > 0
                                         ? "  ·  " + prow.modelData.open_questions + " open Q"
                                         : "")
                                color: Theme.fgMuted
                                font.family: Theme.fontText
                                font.pixelSize: Theme.typeCaption
                            }
                        }
                        Text {
                            id: metaT
                            anchors.verticalCenter: parent.verticalCenter
                            width: 56
                            horizontalAlignment: Text.AlignRight
                            text: Math.round((prow.modelData.focus_secs || 0) / 3600)
                                  + "h"
                            color: Theme.fgSubtle
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                            font.features: ({ "tnum": 1 })
                        }
                    }
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5
            visible: root.deadlines.length > 0 }

        Text {
            visible: root.deadlines.length > 0
            text: "Upcoming deadlines"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeBody
            font.weight: Theme.typeBodyEmphasisWeight
        }

        Column {
            width: parent.width
            spacing: Theme.spaceXs
            Repeater {
                model: root.deadlines
                delegate: Row {
                    id: drow
                    required property var modelData
                    width: parent.width
                    spacing: Theme.spaceSm

                    readonly property bool soon: {
                        const t = new Date(drow.modelData.due).getTime();
                        return !isNaN(t) && (t - Date.now()) < 3 * 86400000;
                    }
                    readonly property color urgency:
                        drow.modelData.overdue ? Theme.error
                        : drow.soon ? Theme.warning
                        : Theme.fgSubtle

                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: drow.modelData.kind === "event" ? "◷" : "▸"
                        color: drow.urgency
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        width: parent.width - 16 - dueT.width - 2 * Theme.spaceSm
                        elide: Text.ElideRight
                        text: drow.modelData.title
                        color: Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                    }
                    Text {
                        id: dueT
                        anchors.verticalCenter: parent.verticalCenter
                        width: 76
                        horizontalAlignment: Text.AlignRight
                        text: root.relTime(drow.modelData.due)
                        color: drow.urgency
                        font.family: Theme.fontMono
                        font.pixelSize: Theme.typeCaption
                        font.features: ({ "tnum": 1 })
                    }
                }
            }
        }
    }
}
