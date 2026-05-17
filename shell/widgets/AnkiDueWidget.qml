// AnkiDueWidget — spaced-repetition review-due badge (spec §2.9.6).
//
// "Persistent badge in the bar showing cards due. Color-coded by
// urgency." Quiet until important (§1.3): the widget collapses to zero
// width when nothing is due, so it never adds visual noise on a clear
// review queue.
//
// state shape (from anki_due::AnkiDueModule): { "due": <int> }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int due: (widgetState && widgetState.due) || 0

    // Urgency tint (spec §2.9.6 "color-coded by urgency"). Escalation/
    // health from WidgetWrapper still wins when it applies.
    readonly property color dueColor:
        root.escalated ? root.contentColor
        : due >= 50 ? Theme.error
        : due >= 20 ? Theme.warning
        : Theme.primary

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm
        // Collapse the whole widget when the queue is clear.
        visible: root.due > 0

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconNote
            color: root.dueColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.due
            color: root.dueColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only"
        }
    }
}
