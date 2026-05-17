// ClockHub — clock & calendar dropdown (§2.1.5).
//
// Calendar grid is local-date derived (no daemon round-trip needed for
// a month grid). Upcoming events + the next-event countdown are fed
// live by the daemon's `clock` module via DaemonMessage::ClockHub
// (`payload`), sourced from the unified store (CalDAV-synced or any
// other event source). World-clock row remains future work.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false

    // Live upcoming-events feed (DaemonMessage::ClockHub). events[] is
    // ordered soonest-first; each has title, start_at, end_at (RFC 3339
    // UTC), optional location.
    property var payload: ({ generated_at: "", events: [] })
    readonly property var events: payload && payload.events ? payload.events : []

    // Ticks every 30s so the countdown stays current while the hub is
    // open without a per-second timer.
    property double nowMs: Date.now()
    Timer {
        interval: 30000
        running: root.isOpen
        repeat: true
        onTriggered: root.nowMs = Date.now()
    }
    onIsOpenChanged: if (isOpen) nowMs = Date.now()

    // First event that hasn't started yet, for the countdown line.
    readonly property var nextEvent: {
        for (let i = 0; i < events.length; i++) {
            const t = Date.parse(events[i].start_at);
            if (!isNaN(t) && t > nowMs) return events[i];
        }
        return null;
    }

    function fmtTime(iso) {
        const d = new Date(iso);
        if (isNaN(d.getTime())) return "";
        return Qt.formatDateTime(d, "ddd HH:mm");
    }

    // Compact "in 2h 15m" / "in 6m" / "now" relative label.
    function fmtCountdown(iso) {
        const t = Date.parse(iso);
        if (isNaN(t)) return "";
        let mins = Math.round((t - nowMs) / 60000);
        if (mins <= 0) return "now";
        if (mins < 60) return "in " + mins + "m";
        const h = Math.floor(mins / 60);
        const m = mins % 60;
        if (h < 24) return "in " + h + "h" + (m > 0 ? " " + m + "m" : "");
        const days = Math.floor(h / 24);
        return "in " + days + "d";
    }

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

    implicitWidth: 380
    implicitHeight: headerCol.implicitHeight + 2 * Theme.panelInnerPadding

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.panelOpacityBattery : Theme.panelOpacity)
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

        // Upcoming section header + next-event countdown.
        Row {
            width: parent.width
            Text {
                text: "Upcoming"
                color: Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.weight: Theme.typeLabelWeight
                font.capitalization: Font.AllUppercase
            }
            Item { width: parent.width - 1; height: 1 } // spacer
        }

        Text {
            visible: root.nextEvent !== null
            width: parent.width
            elide: Text.ElideRight
            text: root.nextEvent
                  ? root.nextEvent.title + " · " + root.fmtCountdown(root.nextEvent.start_at)
                  : ""
            color: Theme.primary
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeBodyEmphasisWeight
        }

        // Empty state.
        Text {
            visible: root.events.length === 0
            text: "no upcoming events"
            color: Theme.fgMuted
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.italic: true
        }

        // Upcoming list (cap at 6 rows for the dropdown).
        Column {
            width: parent.width
            spacing: Theme.spaceSm
            Repeater {
                model: Math.min(root.events.length, 6)
                delegate: Row {
                    required property int index
                    readonly property var ev: root.events[index]
                    width: parent.width
                    spacing: Theme.spaceMd

                    Text {
                        width: 84
                        text: root.fmtTime(ev.start_at)
                        color: Theme.fgSubtle
                        font.family: Theme.fontMono
                        font.pixelSize: Theme.typeCaption
                        font.features: ({ "tnum": 1 })
                    }
                    Column {
                        width: parent.width - 84 - Theme.spaceMd
                        Text {
                            width: parent.width
                            elide: Text.ElideRight
                            text: ev.title
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                        }
                        Text {
                            width: parent.width
                            elide: Text.ElideRight
                            visible: !!ev.location
                            text: ev.location || ""
                            color: Theme.fgMuted
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                            font.italic: true
                        }
                    }
                }
            }
        }
    }
}
