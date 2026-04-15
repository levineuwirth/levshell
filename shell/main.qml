// Levshell QML shell — Phase 1.4 bar.
//
// Run with:
//   quickshell -p shell/main.qml
//
// ## Architecture
//
// The shell holds three reactive maps as root properties:
//
//   - widgetStates:     widget_id → JSON state (from WidgetUpdate)
//   - widgetStatuses:   widget_id → health string ("normal"|"stale"|"error"|"unavailable")
//   - widgetVisibility: widget_id → { visible, prominence }
//   - barLayout:        { left: [ids], center: [ids], right: [ids] }
//
// A widget registry maps widget_id → Component. Each zone is a Row
// containing a Repeater over barLayout.{left|center|right}; the delegate
// is a Loader that picks the Component from the registry and passes the
// state/status/prominence as properties.
//
// ## Hello handshake
//
// The daemon enforces a one-frame Hello handshake since Phase 1.1
// (crates/levshell-ipc/src/handshake.rs). Without it the connection is
// idle forever — the daemon never sends any DaemonMessage, and the shell
// never renders anything beyond defaults. `onConnectionStateChanged`
// writes the Hello immediately on connect.
//
// ## Wire format
//
// One JSON-encoded DaemonMessage per line, '\n'-terminated (NDJSON).
// SplitParser with splitMarker "\n" is safe because compact serde_json
// output never contains unescaped newlines.

import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import "."
import "widgets"

Scope {
    id: shell

    // ----------------------------------------------------------------------
    // Reactive state stores.
    // ----------------------------------------------------------------------
    // Using plain objects so that re-assignment through Object.assign
    // triggers QML's property-change notifications. Mutating in place
    // would not.
    property var widgetStates: ({})
    property var widgetStatuses: ({})
    property var widgetVisibility: ({})
    property var barLayout: ({ left: [], center: [], right: [] })
    // Command-palette overlay state, driven by the `command-palette`
    // widget_update messages. The palette is intentionally NOT in
    // widgetRegistry / widgetStates — it renders as an overlay window,
    // not as a widget inside any of the bar zones.
    property var paletteState: ({ open: false, query: "", results: [] })

    // ----------------------------------------------------------------------
    // Widget registry: map widget_id → Component.
    // ----------------------------------------------------------------------
    // Each entry is a QML Component we hand to a Loader. Keys must match
    // the widget_id that the daemon publishes in BarLayout.
    property var widgetRegistry: ({
        "workspace-indicator": workspaceIndicatorComponent,
        "clock": clockComponent,
        "cpu": cpuComponent,
        "memory": memoryComponent,
        "battery": batteryComponent,
        "network": networkComponent,
        "notifications": notificationsComponent
    })

    Component { id: workspaceIndicatorComponent; WorkspaceIndicator {} }
    Component { id: clockComponent; ClockWidget {} }
    Component { id: cpuComponent; CpuWidget {} }
    Component { id: memoryComponent; MemoryWidget {} }
    Component { id: batteryComponent; BatteryWidget {} }
    Component { id: networkComponent; NetworkWidget {} }
    Component { id: notificationsComponent; NotificationsWidget {} }

    // ----------------------------------------------------------------------
    // Dispatch a parsed DaemonMessage into the state stores.
    // ----------------------------------------------------------------------
    function dispatchDaemonMessage(msg) {
        if (!msg || !msg.type) return;
        switch (msg.type) {
        case "widget_update": {
            const id = msg.widget_id;
            // Route the command-palette widget into the dedicated
            // overlay state instead of the bar widget store.
            if (id === "command-palette") {
                shell.paletteState = msg.state || { open: false, query: "", results: [] };
                break;
            }
            const s = Object.assign({}, shell.widgetStates);
            s[id] = msg.state || {};
            shell.widgetStates = s;

            const st = Object.assign({}, shell.widgetStatuses);
            st[id] = msg.status || "normal";
            shell.widgetStatuses = st;
            break;
        }
        case "widget_visibility": {
            const v = Object.assign({}, shell.widgetVisibility);
            v[msg.widget_id] = {
                visible: msg.visible,
                prominence: msg.prominence || "visible"
            };
            shell.widgetVisibility = v;
            break;
        }
        case "bar_layout": {
            shell.barLayout = {
                left: msg.left || [],
                center: msg.center || [],
                right: msg.right || []
            };
            break;
        }
        default:
            // Unknown message types (e.g. a future Theme variant) are
            // silently ignored. The daemon's non_exhaustive enums are
            // designed for forward-compat; the shell matches that.
            break;
        }
    }

    function prominenceFor(widgetId) {
        const v = shell.widgetVisibility[widgetId];
        if (!v) return "visible";
        if (!v.visible) return "hidden";
        return v.prominence || "visible";
    }

    function statusFor(widgetId) {
        return shell.widgetStatuses[widgetId] || "normal";
    }

    function stateFor(widgetId) {
        return shell.widgetStates[widgetId] || null;
    }

    function componentFor(widgetId) {
        return shell.widgetRegistry[widgetId] || null;
    }

    // ----------------------------------------------------------------------
    // PanelWindow — one top-edge bar per screen.
    // ----------------------------------------------------------------------
    Variants {
        model: Quickshell.screens
        delegate: PanelWindow {
            id: panel
            required property var modelData
            screen: modelData

            anchors {
                top: true
                left: true
                right: true
            }
            // Full-density bar height per spec §5.2. Density modes
            // (compact/hidden) will be wired to this in a later phase.
            implicitHeight: Theme.barHeightFull
            // The bar itself uses `surface` per §3.2 — not `bg`, which
            // is the desktop layer behind the bar.
            color: Theme.surface

            // Inner-shadow contrast floor — §3.2.1. A 1-2px dark strip
            // along the bar's bottom edge provides a subtle but
            // guaranteed contrast anchor even against bright wallpapers
            // (matters more once blur mode lands).
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: 1
                color: Theme.bgDark
                opacity: 0.30
            }

            // Left zone.
            Row {
                id: leftZone
                anchors.left: parent.left
                anchors.leftMargin: Theme.spaceLg
                anchors.verticalCenter: parent.verticalCenter
                spacing: Theme.interWidgetGapFull

                Repeater {
                    model: shell.barLayout.left
                    delegate: Loader {
                        required property var modelData
                        sourceComponent: shell.componentFor(modelData)
                        onLoaded: {
                            if (!item) return;
                            item.widgetState = Qt.binding(() => shell.stateFor(modelData));
                            item.status = Qt.binding(() => shell.statusFor(modelData));
                            item.prominence = Qt.binding(() => shell.prominenceFor(modelData));
                        }
                    }
                }
            }

            // Center zone.
            Row {
                id: centerZone
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.verticalCenter: parent.verticalCenter
                spacing: Theme.interWidgetGapFull

                Repeater {
                    model: shell.barLayout.center
                    delegate: Loader {
                        required property var modelData
                        sourceComponent: shell.componentFor(modelData)
                        onLoaded: {
                            if (!item) return;
                            item.widgetState = Qt.binding(() => shell.stateFor(modelData));
                            item.status = Qt.binding(() => shell.statusFor(modelData));
                            item.prominence = Qt.binding(() => shell.prominenceFor(modelData));
                        }
                    }
                }
            }

            // Right zone.
            Row {
                id: rightZone
                anchors.right: parent.right
                anchors.rightMargin: Theme.spaceLg
                anchors.verticalCenter: parent.verticalCenter
                spacing: Theme.interWidgetGapFull

                Repeater {
                    model: shell.barLayout.right
                    delegate: Loader {
                        required property var modelData
                        sourceComponent: shell.componentFor(modelData)
                        onLoaded: {
                            if (!item) return;
                            item.widgetState = Qt.binding(() => shell.stateFor(modelData));
                            item.status = Qt.binding(() => shell.statusFor(modelData));
                            item.prominence = Qt.binding(() => shell.prominenceFor(modelData));
                        }
                    }
                }
            }
        }
    }

    // ----------------------------------------------------------------------
    // Shell → daemon helpers.
    // Each wraps `daemonSocket.write()` with a JSON-encoded ShellMessage.
    // ----------------------------------------------------------------------
    function sendShellMessage(obj) {
        try {
            daemonSocket.write(JSON.stringify(obj) + "\n");
            daemonSocket.flush();
        } catch (e) {
            console.warn("levshell: failed to send shell message:", e);
        }
    }

    function sendPaletteQuery(query) {
        shell.sendShellMessage({
            type: "command_palette_query",
            query: query
        });
    }

    function sendPaletteSelect(provider, itemId) {
        shell.sendShellMessage({
            type: "command_palette_select",
            provider: provider,
            item_id: itemId
        });
    }

    function sendPaletteClose() {
        shell.sendShellMessage({ type: "command_palette_close" });
    }

    // ----------------------------------------------------------------------
    // Command-palette overlay window.
    //
    // A second PanelWindow that becomes visible when paletteState.open
    // goes true. Uses WlrLayershell.keyboardFocus = Exclusive so key
    // events reach the TextInput inside CommandPalette.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: paletteWindow

        // Retain visibility through the close fade so the inner
        // CommandPalette Rectangle can animate its opacity/scale
        // to zero before the PanelWindow is hidden. `closeTimer`
        // fires after the opacity fade + spring settle window
        // and flips `isClosing` back to false, dropping the
        // window off the layer.
        property bool isClosing: false
        visible: shell.paletteState.open === true || isClosing

        Connections {
            target: shell
            function onPaletteStateChanged() {
                if (shell.paletteState.open) {
                    paletteWindow.isClosing = false;
                    closeTimer.stop();
                } else if (paletteWindow.visible && !paletteWindow.isClosing) {
                    paletteWindow.isClosing = true;
                    closeTimer.restart();
                }
            }
        }

        Timer {
            id: closeTimer
            // motionFast fade + spring settle (~350ms) + a small
            // safety margin so we never clip the animation tail.
            interval: Theme.motionSlow + 50
            onTriggered: paletteWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
        WlrLayershell.namespace: "levshell-palette"

        anchors {
            top: true
            left: true
            right: true
            bottom: true
        }
        color: "transparent"

        // Click-outside to close: a transparent full-screen rectangle
        // under the palette that dismisses on any click that didn't
        // land on the palette itself.
        MouseArea {
            anchors.fill: parent
            onClicked: shell.sendPaletteClose()
        }

        CommandPalette {
            id: paletteOverlay
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeightFull + Theme.spaceLg
            paletteData: shell.paletteState
            onQueryChanged: (text) => shell.sendPaletteQuery(text)
            onSelect: (provider, itemId) => shell.sendPaletteSelect(provider, itemId)
            onClose: () => shell.sendPaletteClose()
        }
    }

    // ----------------------------------------------------------------------
    // IPC socket: sends Hello on connect, parses NDJSON frames, dispatches.
    // ----------------------------------------------------------------------
    Socket {
        id: daemonSocket
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/levshell.sock"
        connected: true

        onConnectionStateChanged: {
            if (connected) {
                console.log("levshell: connected to daemon socket");
                // Identify as the shell. PROTOCOL_VERSION lives in
                // crates/levshell-ipc/src/handshake.rs.
                const hello = JSON.stringify({
                    type: "hello",
                    role: "shell",
                    protocol_version: 1
                });
                daemonSocket.write(hello + "\n");
                daemonSocket.flush();
            } else {
                console.log("levshell: daemon socket disconnected");
            }
        }

        parser: SplitParser {
            splitMarker: "\n"
            onRead: (segment) => {
                try {
                    const obj = JSON.parse(segment);
                    shell.dispatchDaemonMessage(obj);
                } catch (e) {
                    console.warn("levshell: failed to parse frame:", e, "segment:", segment);
                }
            }
        }
    }
}
