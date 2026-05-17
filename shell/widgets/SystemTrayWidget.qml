// SystemTrayWidget — StatusNotifierItem (SNI) host (spec §context-engine
// base layer; design §Quick-Settings anchor).
//
// The spec mandates a system tray as a permanent base-layer widget but
// never specifies the protocol. Modern Wayland trays are SNI over D-Bus
// (org.kde.StatusNotifierWatcher); Quickshell's Quickshell.Services.
// SystemTray IS an SNI host. Like NotificationsWidget consuming the
// notification server directly, this consumes the tray service in QML —
// no daemon module (the accepted Quickshell-service exception).
//
// `SystemTray.items` is an UntypedObjectModel: iterate `.values`
// (JS-array-shaped), exactly the trackedNotifications pattern.
//
// Interaction (full SNI):
//   left click  → activate()   (or the menu, for menu-only items)
//   middle click→ secondaryActivate()
//   right click → item.display(window, x, y) — Quickshell renders the
//                 item's D-Bus context menu natively (submenus,
//                 checkboxes, etc. handled by the toolkit).
//   scroll      → scroll(delta, horizontal)

import QtQuick
import Quickshell
import Quickshell.Services.SystemTray
import ".."

WidgetWrapper {
    id: root

    // Present for API uniformity with telemetry widgets; never written
    // (tray state comes from the Quickshell service, not the daemon).
    property var widgetState: null

    readonly property var items:
        SystemTray.items ? SystemTray.items.values : []

    Row {
        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        spacing: Theme.spaceSm

        Repeater {
            model: root.items
            delegate: Item {
                id: cell
                required property var modelData
                width: Theme.iconSize
                height: Theme.iconSize
                anchors.verticalCenter: parent.verticalCenter

                Image {
                    id: iconImg
                    anchors.fill: parent
                    source: cell.modelData.icon || ""
                    sourceSize.width: Theme.iconSize
                    sourceSize.height: Theme.iconSize
                    fillMode: Image.PreserveAspectFit
                    smooth: true
                    asynchronous: true
                    visible: status === Image.Ready
                }

                // Fallback glyph if the SNI gave no usable icon.
                Text {
                    anchors.centerIn: parent
                    visible: iconImg.status !== Image.Ready
                    text: Theme.iconAppWindow
                    color: root.contentColor
                    font.family: Theme.fontIcon
                    font.pixelSize: Theme.iconSize
                }

                function openMenu() {
                    if (!cell.modelData.hasMenu) return;
                    // Position the menu just below the icon. mapToItem
                    // with null target → window/scene coordinates, which
                    // is what display()'s relativeX/Y expect.
                    const p = iconImg.mapToItem(null, 0, iconImg.height);
                    cell.modelData.display(QsWindow.window,
                                           Math.round(p.x), Math.round(p.y));
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.LeftButton | Qt.MiddleButton | Qt.RightButton
                    cursorShape: Qt.PointingHandCursor
                    onClicked: (e) => {
                        if (e.button === Qt.RightButton) {
                            cell.openMenu();
                        } else if (e.button === Qt.MiddleButton) {
                            cell.modelData.secondaryActivate();
                        } else {
                            // Menu-only items have no meaningful
                            // activate() — open their menu instead.
                            if (cell.modelData.onlyMenu) cell.openMenu();
                            else cell.modelData.activate();
                        }
                    }
                    onWheel: (w) => {
                        const dy = w.angleDelta.y;
                        const dx = w.angleDelta.x;
                        if (dy !== 0) cell.modelData.scroll(dy, false);
                        if (dx !== 0) cell.modelData.scroll(dx, true);
                    }
                    // No hover tooltip: QtQuick.Controls is intentionally
                    // not a dependency of this shell. A themed custom
                    // tooltip can come later if wanted.
                }
            }
        }
    }
}
