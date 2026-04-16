// WorkspaceIndicator — renders the Sway workspace list as pills.
//
// Consumes a `widgetState` object matching the WorkspaceIndicatorState
// struct from levshell-modules::sway::indicator:
//
//   {
//     "workspaces": [{ "name": "...", "num": 1, "focused": true, ... }, ...],
//     "active": "research",
//     "focused_window": "Alacritty"
//   }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    // Daemon-published state payload. Named `widgetState` because `state`
    // collides with `QQuickItem.state`.
    property var widgetState: ({})

    readonly property var workspaces: (widgetState && widgetState.workspaces) || []

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Repeater {
            model: root.workspaces
            delegate: Rectangle {
                id: pill
                required property var modelData
                width: wsLabel.implicitWidth + 2 * Theme.spaceMd
                height: Theme.typeBody + 2 * Theme.spaceXs + 4
                // Pills are intra-widget tags — small radius is fine
                // (spec is silent on their styling, and they're not in
                // the bar's top-level surface hierarchy).
                radius: 4
                color: pill.modelData.focused ? root.accentColor : Theme.surfaceRaised
                border.width: pill.modelData.urgent ? 1 : 0
                // Urgent workspaces use the full-saturation error token
                // — this is an escalation signal, not a health state.
                border.color: Theme.error

                Text {
                    id: wsLabel
                    anchors.centerIn: parent
                    text: pill.modelData.name
                    color: pill.modelData.focused ? Theme.textOnPrimary : root.subtleColor
                    font.family: Theme.fontMono
                    font.pixelSize: Theme.typeLabel
                    font.weight: pill.modelData.focused
                                 ? Theme.typeBodyEmphasisWeight
                                 : Theme.typeLabelWeight
                    font.features: ({ "tnum": 1 })
                }
            }
        }
    }
}
