// ClockWidget — local wall clock, no daemon state.
//
// The context engine lists "clock" in its default widgets and emits
// layout/visibility messages for it, but no module publishes a state
// payload. This widget generates its own time via a local Timer.
//
// The Column width is pinned to a TextMetrics measurement of the
// fixed-width time string ("00:00:00") rather than letting Column's
// implicit width track child widths. Without that pin, the date text
// width (variable-width font) and subpixel rendering caused the
// content's bounding box to fluctuate by ±1px between frames; the
// WidgetWrapper's Behavior on width fired on every fluctuation, the
// centerZone Row recentered each frame, and the visible result was a
// constant leftward drift after the first density change kicked off
// the spring animation.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    // Present for API uniformity with telemetry widgets; never written.
    property var widgetState: null

    property string currentTime: ""
    property string currentDate: ""

    function tick() {
        const now = new Date();
        const h = String(now.getHours()).padStart(2, '0');
        const m = String(now.getMinutes()).padStart(2, '0');
        const s = String(now.getSeconds()).padStart(2, '0');
        currentTime = h + ":" + m + ":" + s;
        const months = ["Jan","Feb","Mar","Apr","May","Jun",
                        "Jul","Aug","Sep","Oct","Nov","Dec"];
        currentDate = months[now.getMonth()] + " " + now.getDate();
    }

    Component.onCompleted: tick()

    TextMetrics {
        id: timeMetrics
        font.family: Theme.fontMono
        font.pixelSize: Theme.typeBody
        font.weight: Theme.typeBodyEmphasisWeight
        text: "00:00:00"
    }

    MouseArea {
        id: clickArea
        anchors.fill: parent
        z: 10
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: shell.toggleClockHub()
    }

    Rectangle {
        anchors.fill: parent
        radius: 4
        color: Theme.fg
        opacity: clickArea.containsMouse ? 0.06 : 0.0
        z: -1
        Behavior on opacity { NumberAnimation { duration: Theme.motionFast } }
    }

    Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: root.tick()
    }

    Column {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.verticalCenter: parent.verticalCenter
        // Pinned to the time-string metric so this column's width is
        // stable regardless of date-font subpixel variance. The two
        // child Texts center inside this fixed-width column.
        width: Math.ceil(timeMetrics.advanceWidth)
        spacing: 0

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.currentTime
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeBody
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
        }
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.currentDate
            color: root.subtleColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            visible: root.prominence === "visible" || root.prominence === "expanded"
        }
    }
}
