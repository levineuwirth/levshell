// ClockHub — clock & calendar dropdown scaffold (§2.1.5).
//
// Phase 1 scaffold: shows today's date, a placeholder calendar grid,
// and a stub for upcoming events. Full implementation (CalDAV sync,
// world-clock row, countdown timer) lands in Phase 2 when the sync
// adapter framework is available.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false

    // Current date for the calendar header.
    readonly property var now: new Date()
    readonly property string monthYear: {
        const months = ["January","February","March","April","May","June",
                        "July","August","September","October","November","December"];
        return months[now.getMonth()] + " " + now.getFullYear();
    }
    readonly property int today: now.getDate()

    // Build a simple 7-column calendar grid for the current month.
    readonly property var calendarDays: {
        const y = now.getFullYear();
        const m = now.getMonth();
        const firstDay = new Date(y, m, 1).getDay(); // 0=Sun
        const daysInMonth = new Date(y, m + 1, 0).getDate();
        const cells = [];
        for (let i = 0; i < firstDay; i++) cells.push(0);
        for (let d = 1; d <= daysInMonth; d++) cells.push(d);
        while (cells.length % 7 !== 0) cells.push(0);
        return cells;
    }

    implicitWidth: 300
    implicitHeight: headerCol.implicitHeight + 2 * Theme.panelInnerPadding

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.barOpacityBattery : Theme.barOpacity)
    Behavior on color { ColorAnimation { duration: Theme.motionNormal } }
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

    // Open/close animation.
    opacity: 0.0; scale: 0.96
    transformOrigin: Item.Top

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
        id: headerCol
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        // Month + year header.
        Text {
            text: root.monthYear
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        // Day-of-week header row.
        Row {
            width: parent.width
            Repeater {
                model: ["Su","Mo","Tu","We","Th","Fr","Sa"]
                delegate: Text {
                    required property string modelData
                    width: parent.width / 7
                    horizontalAlignment: Text.AlignHCenter
                    text: modelData
                    color: Theme.fgMuted
                    font.family: Theme.fontMono
                    font.pixelSize: Theme.typeCaption
                    font.weight: Theme.typeCaptionWeight
                }
            }
        }

        // Calendar grid.
        Grid {
            columns: 7
            width: parent.width
            Repeater {
                model: root.calendarDays
                delegate: Item {
                    required property int modelData
                    width: parent.width / 7
                    height: 28

                    Rectangle {
                        anchors.centerIn: parent
                        width: 24; height: 24; radius: 12
                        color: modelData === root.today ? Theme.primary : "transparent"
                        visible: modelData > 0
                    }

                    Text {
                        anchors.centerIn: parent
                        text: modelData > 0 ? modelData : ""
                        color: modelData === root.today ? Theme.textOnPrimary : Theme.fg
                        font.family: Theme.fontMono
                        font.pixelSize: Theme.typeCaption
                        font.weight: modelData === root.today
                                     ? Theme.typeBodyEmphasisWeight
                                     : Theme.typeCaptionWeight
                        font.features: ({ "tnum": 1 })
                    }
                }
            }
        }

        // Divider.
        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Upcoming events placeholder.
        Text {
            text: "no upcoming events"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }
    }
}
