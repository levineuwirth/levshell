// NotificationsWidget — bar bell icon + unread count.
//
// Reads from the Quickshell NotificationServer's trackedNotifications
// model (owned by main.qml's `notifServer`). Clicking toggles the
// notification center overlay.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    // trackedNotifications is a Quickshell UntypedObjectModel: no `count`
    // property — its public iterable is `values` (QObjectList, JS-array-
    // shaped). The previous `.count` binding was always `undefined → 0`.
    readonly property int unreadCount:
        notifServer ? notifServer.trackedNotifications.values.length : 0

    readonly property bool dnd: shell.doNotDisturb

    readonly property string icon:
        dnd ? Theme.iconBellSlash : Theme.iconBell

    readonly property color bellColor: {
        if (dnd) return Theme.fgMuted;
        if (unreadCount > 0) return root.accentColor;
        return Theme.fgMuted;
    }

    MouseArea {
        id: clickArea
        anchors.fill: parent
        z: 10
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: shell.toggleNotificationCenter()
    }

    Rectangle {
        anchors.fill: parent
        radius: 4
        color: Theme.fg
        opacity: clickArea.containsMouse ? 0.06 : 0.0
        z: -1
        Behavior on opacity { NumberAnimation { duration: Theme.motionFast } }
    }

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceXs

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.icon
            color: root.bellColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
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
