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
    // Notifications that arrived while Do-Not-Disturb was active are
    // still persisted (reviewable in the center) but excluded from the
    // unread badge — that exclusion is what "DnD silences" means here,
    // since this shell has no popup/sound surface to suppress.
    readonly property int unreadCount: {
        if (!notifServer) return 0;
        const vals = notifServer.trackedNotifications.values;
        const muted = shell.mutedNotifIds || ({});
        let n = 0;
        for (let i = 0; i < vals.length; i++)
            if (!muted[vals[i].id]) n++;
        return n;
    }

    readonly property bool dnd: shell.doNotDisturb

    readonly property string icon:
        dnd ? Theme.iconBellSlash : Theme.iconBell

    readonly property color bellColor: {
        if (dnd) return Theme.fgMuted;
        if (unreadCount > 0) return root.accentColor;
        return Theme.fgMuted;
    }

    interactive: true
    onClicked: shell.toggleNotificationCenter()

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
