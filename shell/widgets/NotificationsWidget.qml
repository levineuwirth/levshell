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

    readonly property int unreadCount:
        notifServer ? notifServer.trackedNotifications.count : 0

    readonly property bool dnd: shell.doNotDisturb

    readonly property string icon:
        dnd ? Theme.iconBellSlash : Theme.iconBell

    readonly property color bellColor: {
        if (dnd) return Theme.fgMuted;
        if (unreadCount > 0) return root.accentColor;
        return Theme.fgMuted;
    }

    MouseArea {
        anchors.fill: parent
        z: 10
        onClicked: shell.toggleNotificationCenter()
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
