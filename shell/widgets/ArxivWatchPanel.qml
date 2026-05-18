// ArxivWatchPanel — new arXiv matches (spec §2.9.9).
//
// Opened from ArxivWatchWidget. One row per new paper: title,
// published date, truncated abstract. Click opens the PDF
// (`arxiv-watch open`). Monitor surface otherwise.
//
// payload == arxiv_watch state: { new_count, items:[{title,summary,
//   url,published}] }

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ new_count: 0, items: [] })
    readonly property var items: (payload && payload.items) || []

    signal openPaper(string url)

    implicitWidth: Math.round(420 * Theme.uiScale)
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
            text: "New on arXiv"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Text {
            visible: root.items.length === 0
            text: "nothing new — set keywords in modules/arxiv.toml"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        Column {
            width: parent.width
            spacing: Theme.spaceSm
            Repeater {
                model: root.items
                delegate: Rectangle {
                    id: arow
                    required property var modelData
                    width: parent.width
                    implicitHeight: acol.implicitHeight + Theme.spaceSm
                    radius: 4
                    color: ahit.containsMouse ? Theme.surfaceRaised : "transparent"

                    Column {
                        id: acol
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.leftMargin: Theme.spaceSm
                        anchors.rightMargin: Theme.spaceSm
                        spacing: 2

                        Text {
                            width: parent.width
                            wrapMode: Text.WordWrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                            text: arow.modelData.title
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                            font.weight: Theme.typeBodyEmphasisWeight
                        }
                        Text {
                            width: parent.width
                            wrapMode: Text.WordWrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                            text: arow.modelData.summary
                            color: Theme.fgMuted
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                        }
                        Text {
                            text: (arow.modelData.published || "").slice(0, 10)
                                  + "  ·  open PDF ⧉"
                            color: ahit.containsMouse ? Theme.primary : Theme.fgSubtle
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                        }
                    }

                    MouseArea {
                        id: ahit
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.openPaper(arow.modelData.url)
                    }
                }
            }
        }
    }
}
