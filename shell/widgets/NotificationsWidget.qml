// NotificationsWidget — placeholder for Phase 1.5's notification center.
//
// Renders a bell icon + a count of unread notifications. Currently the
// daemon doesn't publish any state for this widget, so `widgetState` is
// expected to be empty and the count defaults to 0.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int unreadCount: (widgetState && widgetState.unread) || 0

    readonly property color bellColor:
        unreadCount > 0 ? root.accentColor : Theme.fgMuted

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceXs

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "◈"
            color: root.bellColor
            font.pixelSize: Theme.iconSizeFull
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.unreadCount
            color: root.subtleColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeCaptionWeight
            font.features: ({ "tnum": 1 })
            visible: root.unreadCount > 0 && root.prominence !== "icon_only"
        }
    }
}
