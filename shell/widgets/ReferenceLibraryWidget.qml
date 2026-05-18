// ReferenceLibraryWidget — Zotero/library stats pill (spec §2.9.8).
//
// Compact: book glyph + total paper count; subtitle carries the unread
// count. Click opens ReferenceLibraryPanel (recently-touched papers,
// copy @citekey on click). Citation quick-search proper is the
// command-palette `ref-search` provider (M3.10) — this complements it.
//
// state (from reference_library::ReferenceLibraryModule):
//   { total, unread, recent_count, recent: [{title, citekey, year}] }

import QtQuick
import ".."

WidgetWrapper {
    id: root

    property var widgetState: ({})

    readonly property int total: (widgetState && widgetState.total) || 0
    readonly property int unread: (widgetState && widgetState.unread) || 0
    readonly property int recentCount: (widgetState && widgetState.recent_count) || 0

    interactive: true
    onClicked: shell.toggleRefLibrary()

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm
        visible: root.total > 0

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: Theme.iconNote
            color: root.contentColor
            font.family:    Theme.fontIcon
            font.pixelSize: Theme.iconSize
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.total
            color: root.contentColor
            font.family: Theme.fontMono
            font.pixelSize: Theme.typeLabel
            font.weight: Theme.typeBodyEmphasisWeight
            font.features: ({ "tnum": 1 })
            visible: root.prominence !== "icon_only"
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.unread + " unread"
            color: root.subtleColor
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            visible: (root.prominence === "visible"
                      || root.prominence === "expanded") && root.unread > 0
        }
    }
}
