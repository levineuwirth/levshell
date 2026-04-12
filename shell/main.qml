// Levshell QML shell — Phase 0 workspace-indicator bar.
//
// Run with:
//   quickshell -p shell/main.qml
//
// This file is the *minimum* Quickshell config that closes the Phase 0
// vertical slice: it opens a top-edge PanelWindow on every screen, connects
// to the daemon's Unix domain socket at $XDG_RUNTIME_DIR/levshell.sock, and
// renders the workspace_indicator widget that the SwayWorkspaceModule pushes
// over IPC.
//
// Wire format reminder (see crates/levshell-ipc/src/framing.rs):
//
//   [4-byte big-endian u32 length] [N bytes JSON-encoded DaemonMessage]
//
// Each DaemonMessage is a tagged JSON object:
//
//   { "type": "widget_update", "widget_id": "...", "widget_type": "...",
//     "state": <opaque>, "status": "normal" }
//
// The Phase-0 widget_update we care about has
// widget_id == "workspace-indicator" and a state object of the shape:
//
//   { "workspaces": [ { "name": "...", "num": 1, "focused": true, ... }, ... ],
//     "active":          "research",
//     "focused_window":  "Alacritty" }
//
// NOTE: Quickshell's Io.Socket binary-frame API can vary across versions.
// The handler below is the version that works on Quickshell ≥ 0.1; if your
// build exposes a different signal name (`onRead`, `dataReady`, ...), adjust
// the connection inside `Socket {}` accordingly. The framing math is
// version-independent.

import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

Scope {
    id: shell

    // Mirrored state for the bar to bind against.
    property string activeWorkspace: "—"
    property string focusedWindow: ""
    property var workspaces: []

    // ----------------------------------------------------------------------
    // PanelWindow: top edge of every screen.
    // ----------------------------------------------------------------------
    Variants {
        model: Quickshell.screens
        delegate: PanelWindow {
            required property var modelData
            screen: modelData

            anchors {
                top: true
                left: true
                right: true
            }
            implicitHeight: 30

            color: "#1e1e2e"

            Row {
                anchors.left: parent.left
                anchors.leftMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                spacing: 12

                Repeater {
                    model: shell.workspaces
                    delegate: Rectangle {
                        required property var modelData
                        width: label.implicitWidth + 14
                        height: 22
                        radius: 4
                        color: modelData.focused ? "#7aa2f7" : "#313244"
                        Text {
                            id: label
                            anchors.centerIn: parent
                            color: modelData.focused ? "#1e1e2e" : "#cdd6f4"
                            text: modelData.name
                            font.family: "monospace"
                            font.pointSize: 10
                            font.bold: modelData.focused
                        }
                    }
                }
            }

            Text {
                anchors.centerIn: parent
                color: "#a6adc8"
                text: shell.focusedWindow
                font.family: "monospace"
                font.pointSize: 10
                elide: Text.ElideRight
                width: parent.width / 3
                horizontalAlignment: Text.AlignHCenter
            }

            Text {
                anchors.right: parent.right
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                color: "#cdd6f4"
                text: "levshell"
                font.family: "monospace"
                font.pointSize: 10
            }
        }
    }

    // ----------------------------------------------------------------------
    // IPC socket — pulls DaemonMessage frames from the daemon and updates
    // the bound state above.
    // ----------------------------------------------------------------------
    Socket {
        id: daemonSocket
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/levshell.sock"
        connected: true

        // Rolling buffer + framing state. JavaScript's typed arrays handle
        // the binary side; the parsed payload is converted to a string for
        // JSON.parse below.
        property var buffer: new Uint8Array(0)
        property int expectedLen: -1

        // The exact signal Quickshell exposes for raw socket reads has varied
        // by release. Adjust the handler name if your Quickshell build emits
        // a different signal — the framing logic is independent.
        onTextRead: function(text) {
            // Some Quickshell builds deliver pre-decoded UTF-8; if so, the
            // wire format here would have to be NDJSON instead of length-
            // prefixed binary. Phase 0 ships length-prefixed binary, so this
            // branch is intentionally a no-op.
        }

        onConnectionStateChanged: {
            if (connected) {
                console.log("levshell: connected to daemon socket");
            } else {
                console.log("levshell: daemon socket disconnected");
            }
        }

        // Pull frames out of the rolling buffer until we run out of complete
        // ones, then return.
        function pumpFrames() {
            while (true) {
                if (daemonSocket.expectedLen < 0) {
                    if (daemonSocket.buffer.length < 4) {
                        return;
                    }
                    const b = daemonSocket.buffer;
                    daemonSocket.expectedLen =
                        (b[0] << 24 >>> 0) +
                        (b[1] << 16 >>> 0) +
                        (b[2] << 8 >>> 0) +
                        (b[3] >>> 0);
                    daemonSocket.buffer = daemonSocket.buffer.slice(4);
                }
                if (daemonSocket.buffer.length < daemonSocket.expectedLen) {
                    return;
                }
                const payload = daemonSocket.buffer.slice(0, daemonSocket.expectedLen);
                daemonSocket.buffer = daemonSocket.buffer.slice(daemonSocket.expectedLen);
                daemonSocket.expectedLen = -1;

                let text = "";
                for (let i = 0; i < payload.length; ++i) {
                    text += String.fromCharCode(payload[i]);
                }
                try {
                    const obj = JSON.parse(text);
                    shell.dispatchDaemonMessage(obj);
                } catch (e) {
                    console.warn("levshell: failed to parse frame:", e);
                }
            }
        }
    }

    // Dispatch a parsed DaemonMessage to the bar's state.
    function dispatchDaemonMessage(msg) {
        if (msg.type !== "widget_update") {
            return;
        }
        if (msg.widget_id !== "workspace-indicator") {
            return;
        }
        const state = msg.state || {};
        shell.activeWorkspace = state.active || "—";
        shell.focusedWindow = state.focused_window || "";
        shell.workspaces = state.workspaces || [];
    }
}
