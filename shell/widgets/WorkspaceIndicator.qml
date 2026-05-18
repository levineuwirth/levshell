// WorkspaceIndicator — renders the Sway workspace list as pills.
//
// Consumes a `widgetState` object matching the WorkspaceIndicatorState
// struct from levshell-modules::sway::indicator:
//
//   {
//     "workspaces": [{ "name": "...", "num": 1, "focused": true, ... }, ...],
//     "active": "research",
//     "focused_window": "Alacritty",
//     "project": "Sparse Attention"   // owning project, or null
//   }
//
// The pills are the workspace list; a breadcrumb trail
// `› project › window` (spec §2.1.3) follows it, segments dropped when
// absent. Hidden below `visible` prominence so a tight bar keeps just
// the pills.

import QtQuick
import ".."

WidgetWrapper {
    id: root

    // Daemon-published state payload. Named `widgetState` because `state`
    // collides with `QQuickItem.state`.
    property var widgetState: ({})

    readonly property var workspaces: (widgetState && widgetState.workspaces) || []
    readonly property string activeProject: (widgetState && widgetState.project) || ""
    readonly property string focusedWindow: (widgetState && widgetState.focused_window) || ""

    // `project › window`, each segment included only when present.
    readonly property string breadcrumb: {
        const parts = [];
        if (activeProject.length > 0) parts.push(activeProject);
        if (focusedWindow.length > 0) parts.push(focusedWindow);
        return parts.join("  ›  ");
    }
    readonly property bool showBreadcrumb:
        breadcrumb.length > 0
        && (root.prominence === "visible" || root.prominence === "expanded")

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

                // Per-pill click → ask the daemon to switch (QML never
                // talks to sway directly). Fills the pill, whose size
                // comes from wsLabel, not the measured content slot, so
                // this introduces no targetWidth binding loop.
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: shell.sendShellMessage({
                        type: "widget_action",
                        widget_id: "workspace-indicator",
                        action: "switch",
                        data: { name: pill.modelData.name }
                    })
                }
            }
        }

        // Breadcrumb: where the focused workspace sits (project) and
        // what's in front of you (window). The leading `›` separates it
        // from the pill strip.
        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: root.showBreadcrumb
            text: "›  " + root.breadcrumb
            color: root.subtleColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeLabelWeight
            elide: Text.ElideRight
        }
    }
}
