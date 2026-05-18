// ReferenceLibraryPanel — recent papers + library stats (spec §2.9.8).
//
// Opened from ReferenceLibraryWidget. Header line is the stats; rows are
// the most recently-touched papers. Clicking a row copies its
// `@citekey` (routed as a `reference-library copy` widget action).
//
// payload == reference_library state:
//   { total, unread, recent_count, recent: [{title, citekey, year}] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ total: 0, unread: 0, recent_count: 0, recent: [] })
    readonly property var recent: (payload && payload.recent) || []

    // Emitted with the bare citekey (no leading @).
    signal copyCitekey(string citekey)

    implicitWidth: 360
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
            text: "Library"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            text: root.payload.total + " papers  ·  "
                  + root.payload.unread + " unread  ·  "
                  + root.payload.recent_count + " new (14d)"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
        }

        Text {
            visible: root.recent.length === 0
            text: "no papers — sync a Zotero library"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Column {
            width: parent.width
            spacing: Theme.spaceXs
            Repeater {
                model: root.recent
                delegate: Rectangle {
                    id: rrow
                    required property var modelData
                    width: parent.width
                    height: 38
                    radius: 4
                    color: rhit.containsMouse ? Theme.surfaceRaised : "transparent"

                    Column {
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.left: parent.left
                        anchors.leftMargin: Theme.spaceSm
                        anchors.right: copyHint.left
                        anchors.rightMargin: Theme.spaceSm
                        spacing: 1
                        Text {
                            width: parent.width
                            elide: Text.ElideRight
                            text: rrow.modelData.title
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                            font.weight: Theme.typeBodyEmphasisWeight
                        }
                        Text {
                            width: parent.width
                            elide: Text.ElideRight
                            text: "@" + rrow.modelData.citekey
                                  + (rrow.modelData.year ? "  ·  " + rrow.modelData.year : "")
                            color: Theme.fgMuted
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                        }
                    }

                    Text {
                        id: copyHint
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.right: parent.right
                        anchors.rightMargin: Theme.spaceSm
                        text: "⧉"
                        color: rhit.containsMouse ? Theme.primary : Theme.fgSubtle
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                    }

                    MouseArea {
                        id: rhit
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.copyCitekey(rrow.modelData.citekey)
                    }
                }
            }
        }
    }
}
