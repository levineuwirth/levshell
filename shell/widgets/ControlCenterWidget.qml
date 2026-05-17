// ControlCenterWidget — bar entry point for the Quick-Settings flyout.
//
// Quick-Settings was previously only reachable by clicking the battery
// widget, which self-parks on desktops (no /sys/class/power_supply
// battery) — leaving the flyout unreachable on this hardware. This is a
// hardware-independent, always-present trigger in the right zone.
//
// Pure button: no daemon state. Like ClockWidget it carries a
// `widgetState` for API uniformity with telemetry widgets but never
// reads it.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property bool open: shell.quickSettingsOpen

    interactive: true
    onClicked: shell.toggleQuickSettings()

    Text {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        text: Theme.iconControlCenter
        color: root.open ? root.accentColor : Theme.fgMuted
        font.family:    Theme.fontIcon
        font.pixelSize: Theme.iconSize
    }
}
