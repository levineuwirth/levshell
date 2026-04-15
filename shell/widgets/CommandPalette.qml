// CommandPalette — the palette overlay's inner surface.
//
// This is the *contents* of the palette popup, not the popup window
// itself. `main.qml` hosts it inside a second PanelWindow that becomes
// visible when `paletteState.open === true`. The PanelWindow provides
// the layer-shell keyboard focus and window positioning; this file
// renders the card surface, search input, and categorized results.
//
// ## Spec alignment (§12.1 shared overlay + §12.2 command palette)
//
//   * Width 560px (spec 550–650)
//   * Corner radius 8px (panelCornerRadius)
//   * Background: Theme.surface (NOT Theme.overlay — overlay is
//     reserved for tooltips/popovers per §3.2. §12.1 explicitly
//     specifies `surface` for overlay panels.)
//   * Border: 1px Theme.outline
//   * Drop shadow: 0 4px 16px rgba(0,0,0,0.3) via MultiEffect
//   * Search input typeTitle (17px) in Spectral
//   * Results typeBody (13px) with typeBodyEmphasisWeight when selected
//   * Subtitles typeCaption (11px) in IBM Plex Mono
//   * Selected row background: Theme.surfaceRaised
//   * 24px icons in result rows per §8.2
//   * Results grouped into **categorized sections** per provider —
//     the daemon's `merge_results` ranks by score globally, then this
//     file re-orders into provider-contiguous buckets so
//     ListView.section.delegate can render APPS / WORKSPACES / NOTES
//     headers between them.
//
// ## Inputs
//
//   paletteData:    { open: bool, query: string, results: [PaletteItem] }
//   onQueryChanged: callback fired when the user types
//   onSelect:       callback fired with (provider, itemId) on Enter / click
//   onClose:        callback fired on Esc / click outside

import QtQuick
import QtQuick.Effects
import QtQuick.Window
import ".."

Rectangle {
    id: root

    // =================================================================
    // INPUTS
    // =================================================================
    // Named `paletteData`, not `palette`, because `palette` is a
    // QQuickItem-inherited property.
    property var paletteData: ({ open: false, query: "", results: [] })
    property var onQueryChanged: (text) => {}
    property var onSelect: (provider, itemId) => {}
    property var onClose: () => {}

    property int selectedIndex: 0

    // =================================================================
    // RESULT GROUPING
    //
    // The daemon emits flat results sorted by descending score across
    // all providers. Spec §12.2 wants them displayed in categorized
    // sections, which needs a provider-contiguous ordering. We keep
    // the daemon-side merge simple and do the re-grouping here.
    //
    // `groupedPayload` returns { items, providerCount } in a single
    // walk so we don't iterate twice. `items` is flat but
    // provider-contiguous — suitable for ListView.section.property.
    // =================================================================
    readonly property var groupedPayload: {
        const flat = (paletteData && paletteData.results) || [];
        const byProvider = {};
        const order = [];
        for (let i = 0; i < flat.length; i++) {
            const p = flat[i].provider;
            if (!byProvider[p]) {
                byProvider[p] = [];
                order.push(p);
            }
            byProvider[p].push(flat[i]);
        }
        const out = [];
        for (let i = 0; i < order.length; i++) {
            const bucket = byProvider[order[i]];
            for (let j = 0; j < bucket.length; j++) {
                out.push(bucket[j]);
            }
        }
        return { items: out, providerCount: order.length };
    }
    readonly property var displayedResults: groupedPayload.items
    readonly property int sectionCount: groupedPayload.providerCount

    // Map internal provider identifiers to human-readable section
    // headers. Unknown providers fall through to the raw identifier.
    function prettyProviderName(p) {
        switch (p) {
        case "app-launcher":       return "apps";
        case "workspace-switcher": return "workspaces";
        case "note-search":        return "notes";
        case "stub":               return "stub";
        default:                   return p || "";
        }
    }

    // =================================================================
    // LAYOUT CONSTANTS
    // =================================================================
    readonly property int searchRowHeight:     48
    readonly property int resultRowHeight:     44
    readonly property int sectionHeaderHeight: 22

    // =================================================================
    // CARD DIMENSIONS & CHROME
    //
    // Per spec §12.2, palette height is capped at up to 50% screen.
    // The computed content height is bounded by `maxCardHeight`; beyond
    // that, the ListView scrolls within the fixed frame.
    // =================================================================
    readonly property int maxCardHeight: Math.min(
        540,
        Math.floor(Screen.height * 0.5)
    )
    readonly property int chromeHeight: searchRowHeight
                                      + 2 * Theme.panelInnerPadding
                                      + Theme.spaceMd

    implicitWidth: 560
    implicitHeight: {
        const rowsHeight = displayedResults.length * resultRowHeight;
        const sectionsHeight = sectionCount * sectionHeaderHeight;
        const contentHeight = chromeHeight + rowsHeight + sectionsHeight;
        return Math.min(maxCardHeight, Math.max(chromeHeight + resultRowHeight, contentHeight));
    }
    // Spec §12.1: overlay panels use `surface`, not `overlay`. This
    // was incorrect in the Phase 1.6 first pass.
    color: Theme.surface
    radius: Theme.panelCornerRadius
    border.width: Theme.panelBorderWidth
    border.color: Theme.outline
    antialiasing: true

    // Drop shadow — §12.1 "0 4px 16px rgba(0,0,0,0.3)".
    // MultiEffect with autoPaddingEnabled handles the bounds extension
    // so the shadow can bloom outside the Rectangle's edges without
    // clipping.
    layer.enabled: true
    layer.effect: MultiEffect {
        shadowEnabled:        true
        shadowColor:          "#000000"
        blurMax:              Theme.panelShadowBlur
        shadowBlur:           1.0
        shadowVerticalOffset: Theme.panelShadowOffsetY
        shadowOpacity:        Theme.panelShadowOpacity
        autoPaddingEnabled:   true
    }

    // Height animates with a critically-damped spring — the palette's
    // card is anchored vertically below the bar, and overshoot on a
    // vertically anchored element reads as jitter per §6.3.
    Behavior on implicitHeight {
        SpringAnimation {
            spring:  Theme.springDefault
            damping: Theme.springDefaultCriticalDamping
            mass:    Theme.springMass
            epsilon: 0.5
        }
    }

    onPaletteDataChanged: {
        if (!paletteData) return;
        if (selectedIndex >= displayedResults.length) {
            selectedIndex = 0;
        }
        if (paletteData.open) {
            if (searchInput.text !== paletteData.query) {
                searchInput.text = paletteData.query;
            }
            searchInput.forceActiveFocus();
        }
    }

    // =================================================================
    // KEYBOARD HANDLING
    // =================================================================
    Keys.onEscapePressed: root.onClose()
    Keys.onDownPressed: {
        const n = root.displayedResults.length;
        if (n > 0) root.selectedIndex = (root.selectedIndex + 1) % n;
    }
    Keys.onUpPressed: {
        const n = root.displayedResults.length;
        if (n > 0) root.selectedIndex = (root.selectedIndex - 1 + n) % n;
    }
    Keys.onReturnPressed: root.commitSelection()
    Keys.onEnterPressed:  root.commitSelection()

    function commitSelection() {
        const results = root.displayedResults;
        if (results.length === 0 || root.selectedIndex < 0
            || root.selectedIndex >= results.length) {
            return;
        }
        const item = results[root.selectedIndex];
        root.onSelect(item.provider, item.id);
    }

    function iconFor(hint) {
        switch (hint) {
        case "app":       return "▶";
        case "workspace": return "◫";
        case "note":      return "✎";
        default:          return "•";
        }
    }

    // =================================================================
    // CONTENT
    // =================================================================
    Column {
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        // -------------------------------------------------------------
        // Search input row
        // -------------------------------------------------------------
        Rectangle {
            id: searchRow
            width: parent.width
            height: root.searchRowHeight
            color: Theme.surfaceRaised
            radius: Theme.panelCornerRadius
            border.width: 1
            border.color: searchInput.activeFocus ? Theme.primary : Theme.outline

            Behavior on border.color {
                ColorAnimation { duration: Theme.motionFast }
            }

            Row {
                anchors.fill: parent
                anchors.leftMargin:  Theme.spaceLg
                anchors.rightMargin: Theme.spaceLg
                spacing: Theme.spaceMd

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "❯"
                    color: Theme.primary
                    font.family: Theme.fontMono
                    font.pixelSize: Theme.typeTitle
                    font.weight: Theme.typeTitleWeight
                }

                TextInput {
                    id: searchInput
                    anchors.verticalCenter: parent.verticalCenter
                    width: parent.width - Theme.typeTitle - Theme.spaceMd
                    color: Theme.fg
                    selectionColor: Theme.primary
                    selectedTextColor: Theme.textOnPrimary
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeTitle
                    font.weight: Theme.typeTitleWeight
                    clip: true
                    onTextChanged: {
                        if (text !== root.paletteData.query) {
                            root.onQueryChanged(text);
                        }
                        root.selectedIndex = 0;
                    }
                    Component.onCompleted: forceActiveFocus()
                }
            }
        }

        // -------------------------------------------------------------
        // Results list with category sections
        //
        // ListView.section renders a header before the first item of
        // each contiguous `section.property` value. Because we
        // pre-grouped `displayedResults` into provider buckets,
        // headers split naturally into "apps" / "workspaces" / "notes".
        //
        // `interactive: true` enables mouse-wheel and drag scrolling.
        // Keyboard navigation also scrolls the viewport to keep the
        // selected row visible via `positionViewAtIndex` fired on
        // every `currentIndex` change.
        // -------------------------------------------------------------
        ListView {
            id: resultsView
            width: parent.width
            height: parent.height - searchRow.height - Theme.spaceMd
            clip: true
            currentIndex: root.selectedIndex
            model: root.displayedResults
            spacing: 0
            boundsBehavior: Flickable.StopAtBounds
            interactive: true
            onCurrentIndexChanged: positionViewAtIndex(currentIndex, ListView.Contain)

            section.property: "provider"
            section.criteria: ViewSection.FullString
            section.delegate: Item {
                id: sectionHeader
                required property string section
                width: ListView.view ? ListView.view.width : 0
                height: root.sectionHeaderHeight

                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: Theme.spaceMd
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: Theme.spaceXs
                    text: root.prettyProviderName(sectionHeader.section)
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeLabel
                    font.weight: Theme.typeLabelWeight
                    font.capitalization: Font.AllUppercase
                    font.letterSpacing: 0.8
                }

                // Thin divider under the section label to visually
                // anchor the group. Opacity-softened so it doesn't
                // compete with the selected-row background.
                Rectangle {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    anchors.leftMargin:  Theme.spaceMd
                    anchors.rightMargin: Theme.spaceMd
                    height: 1
                    color: Theme.outline
                    opacity: 0.5
                }
            }

            delegate: Rectangle {
                id: resultRow
                required property var modelData
                required property int index
                width: resultsView.width
                height: root.resultRowHeight
                color: resultRow.index === root.selectedIndex
                       ? Theme.surfaceRaised
                       : "transparent"
                radius: Theme.panelCornerRadius

                Behavior on color {
                    ColorAnimation { duration: Theme.motionFast }
                }

                Row {
                    anchors.fill: parent
                    anchors.leftMargin:  Theme.spaceMd
                    anchors.rightMargin: Theme.spaceMd
                    spacing: Theme.spaceMd

                    // §8.2: command palette result icons are 24px.
                    //
                    // When the daemon resolved an `icon_path` for this
                    // item (currently only the AppLauncherProvider
                    // does), we render the real image file. Otherwise
                    // we fall through to a Unicode glyph keyed on the
                    // provider-supplied `icon` category hint.
                    Item {
                        id: iconSlot
                        width: 24
                        height: 24
                        anchors.verticalCenter: parent.verticalCenter

                        readonly property string resolvedIconPath: resultRow.modelData
                            ? (resultRow.modelData.icon_path || "")
                            : ""
                        readonly property bool iconLoaded:
                            resolvedIconPath.length > 0
                            && realIcon.status === Image.Ready

                        Image {
                            id: realIcon
                            anchors.fill: parent
                            visible: iconSlot.iconLoaded
                            source: iconSlot.resolvedIconPath.length > 0
                                    ? "file://" + iconSlot.resolvedIconPath
                                    : ""
                            sourceSize.width:  24
                            sourceSize.height: 24
                            fillMode: Image.PreserveAspectFit
                            smooth: true
                            asynchronous: true
                            mipmap: true
                        }

                        // Text-glyph fallback: shown whenever no path
                        // is available OR the Image hasn't loaded
                        // yet. Keeps rows from going blank during
                        // async image loading.
                        Text {
                            anchors.centerIn: parent
                            visible: !iconSlot.iconLoaded
                            text: root.iconFor(resultRow.modelData.icon)
                            color: Theme.primary
                            font.pixelSize: 24
                        }
                    }

                    Column {
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 0
                        width: parent.width - 24 - Theme.spaceMd

                        Text {
                            width: parent.width
                            text: resultRow.modelData.title || ""
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeBody
                            font.weight: resultRow.index === root.selectedIndex
                                         ? Theme.typeBodyEmphasisWeight
                                         : Theme.typeBodyWeight
                            elide: Text.ElideRight
                        }

                        Text {
                            width: parent.width
                            text: resultRow.modelData.subtitle || ""
                            color: Theme.fgMuted
                            font.family: Theme.fontMono
                            font.pixelSize: Theme.typeCaption
                            font.weight: Theme.typeCaptionWeight
                            elide: Text.ElideRight
                            visible: text.length > 0
                        }
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    onEntered: root.selectedIndex = resultRow.index
                    onClicked: {
                        root.selectedIndex = resultRow.index;
                        root.commitSelection();
                    }
                }
            }

            // Empty-state placeholder.
            Text {
                anchors.centerIn: parent
                visible: resultsView.count === 0
                text: root.paletteData.query.length === 0
                      ? "start typing to search apps, workspaces, and notes…"
                      : "no results"
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.italic: true
            }
        }
    }
}
