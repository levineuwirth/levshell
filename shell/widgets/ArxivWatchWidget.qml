// ArxivWatchWidget — new-paper badge (spec §2.9.9).
//
// Quiet until important (§1.3): hidden when nothing new. When matches
// arrive it shows a paper glyph + count; clicking opens the list (and
// acknowledges, clearing the badge).
//
// state (from arxiv_watch::ArxivWatchModule):
//   { new_count, items: [{title, summary, url, published}] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int newCount: (widgetState && widgetState.new_count) || 0

    interactive: true
    onClicked: shell.toggleArxivWatch()

    readonly property color badgeColor:
        root.escalated ? root.contentColor : Theme.primary

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm
        visible: root.newCount > 0

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconMagnifyingGlass
            color: root.badgeColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.newCount + " new"
            color: root.badgeColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            visible: root.prominence !== "icon_only"
        }
    }
}
