// ClockWidget — local wall clock, no daemon state.
//
// The context engine lists "clock" in its default widgets and emits
// layout/visibility messages for it, but no module publishes a state
// payload. This widget generates its own time via a local Timer.

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

    Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: root.tick()
    }

    Column {
        anchors.centerIn: parent
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
