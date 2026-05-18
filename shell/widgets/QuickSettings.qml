// QuickSettings — quick-settings flyout (§2.1.7, §12.4).
//
// Spec §2.1.7 mandates: volume + brightness sliders, night-light warmth,
// and toggle tiles for Wi-Fi, Bluetooth, Do Not Disturb, screen recording
// and VPN, where "each tile can expand for detail (e.g. Wi-Fi shows
// available networks, Bluetooth shows paired devices)".
//
// Every backend here follows the same house rule established by the
// rfkill/xbacklight/gammastep tiles: probe for the backend, gate the
// tile's `available` on it, and render an unavailable tile dimmed + inert
// rather than letting it silently no-op. This box's achievable ceiling:
//
//   - Wi-Fi / Bluetooth → `rfkill` radio kill-switch (no NetworkManager
//     /iwd, bluetoothd not running). rfkill is unprivileged here via the
//     elogind seat uaccess ACL on /dev/rfkill. Polled every 5s while open
//     so the airplane-mode key / external `rfkill` reflect.
//   - Night Light → a managed `gammastep` process (constant 3500K). Inert
//     until `gammastep` is on PATH.
//   - Do Not Disturb → shell.doNotDisturb.
//   - Screen recording → a managed `wf-recorder` process (wlroots
//     screencopy, the sway-native recorder) writing to $XDG_VIDEOS_DIR.
//     Stopped with SIGINT so the container is finalized cleanly. Inert
//     until `wf-recorder` is on PATH.
//   - VPN → `wg-quick up|down` on the first /etc/wireguard/*.conf. Gated
//     on `wg-quick` + a readable conf. wg-quick needs root for the
//     netlink/route changes; like every other tile the gate is
//     presence-based, so where privilege is absent the toggle is a
//     no-op and the post-exit `wg show interfaces` refresh reverts the
//     optimistic state rather than lying about it.
//
// Tile expansion: Wi-Fi and Bluetooth tiles carry a caret affordance
// that opens an inline detail card below the grid (accordion — one open
// at a time). Wi-Fi detail lists scanned SSIDs via `nmcli` when a
// network manager is present (radio-only box → an honest "radio toggle
// only" note instead). Bluetooth detail lists paired devices with
// connection state + battery via `bluetoothctl` (empty/"service
// unavailable" when bluetoothd is down).
//
// Volume → Quickshell PipeWire default sink (writable while held by the
// PwObjectTracker). Brightness → acpilight `xbacklight` (sysfs, Wayland
// -safe, `video`-group udev rule); gated on a real /sys/class/backlight
// device (desktop external monitors have none → inert, not broken).
//
// The slider is a minimal custom Item (no QtQuick.Controls dep).

import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import Quickshell.Services.Pipewire
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property bool doNotDisturb: false
    signal dndToggled()

    // -------------------------------------------------------------------
    // PipeWire default audio sink — tracked so audio.volume / audio.muted
    // accept writes. Unlike the `audio` read-only properties, writes go
    // through the tracker.
    // -------------------------------------------------------------------
    PwObjectTracker {
        objects: Pipewire.defaultAudioSink ? [Pipewire.defaultAudioSink] : []
    }

    readonly property PwNode sink: Pipewire.defaultAudioSink
    readonly property bool sinkReady: sink !== null && sink.ready && sink.audio !== null
    readonly property real audioVolume: sinkReady ? sink.audio.volume : 0.0
    readonly property bool audioMuted:  sinkReady ? sink.audio.muted : false

    function setVolume(v) {
        if (!sinkReady) return;
        sink.audio.volume = Math.max(0, Math.min(1, v));
    }
    function toggleMute() {
        if (!sinkReady) return;
        sink.audio.muted = !sink.audio.muted;
    }

    // -------------------------------------------------------------------
    // Brightness via acpilight's `xbacklight`. No device argument —
    // xbacklight auto-selects the first sysfs backlight, matching what
    // the keyboard brightness keys (e.g. XF86MonBrightnessUp) target.
    // `-get` prints a bare percentage (may be fractional). Inert+dimmed
    // unless a real /sys/class/backlight device exists (see below).
    // -------------------------------------------------------------------
    property real brightnessFrac: 0.0   // 0..1
    // acpilight only drives /sys/class/backlight (laptop panels). This is
    // a desktop with external DP/HDMI monitors and no such device, so
    // `xbacklight -get` returns 0 and `-set` is a silent no-op. Gate the
    // slider on an actual backlight existing — without it the control is
    // physically meaningless and should be dimmed/inert, not look broken.
    // (External-monitor brightness would need DDC/CI via ddcutil — a
    // different mechanism, not wired here.)
    property bool backlightPresent: false
    property bool brightnessParsed: false
    readonly property bool brightnessReady: backlightPresent && brightnessParsed

    function refreshBrightness() {
        backlightProbe.running = true;
        brightnessInfoProc.running = true;
    }
    function setBrightness(frac) {
        if (!root.brightnessReady) return;
        const pct = Math.max(1, Math.min(100, Math.round(frac * 100)));
        brightnessSetProc.command = ["xbacklight", "-set", String(pct)];
        brightnessSetProc.running = true;
        root.brightnessFrac = pct / 100;
    }

    Process {
        id: backlightProbe
        command: ["sh", "-c", "ls -1 /sys/class/backlight 2>/dev/null | head -1"]
        stdout: StdioCollector {
            onStreamFinished: root.backlightPresent = text.trim().length > 0
        }
    }
    Process {
        id: brightnessInfoProc
        command: ["xbacklight", "-get"]
        stdout: StdioCollector {
            onStreamFinished: {
                const pct = parseFloat(text.trim());
                if (!isNaN(pct)) {
                    root.brightnessFrac = Math.max(0, Math.min(1, pct / 100));
                    root.brightnessParsed = true;
                }
            }
        }
    }
    Process { id: brightnessSetProc }

    Timer {
        id: brightnessPoll
        interval: 5000
        running: root.isOpen && root.brightnessReady
        repeat: true
        onTriggered: root.refreshBrightness()
    }
    Component.onCompleted: { refreshBrightness(); refreshRfkill() }
    onIsOpenChanged: {
        if (isOpen) { refreshBrightness(); refreshRfkill(); refreshVpn(); }
        else root.expandedTile = "";
    }

    // -------------------------------------------------------------------
    // Radio kill-switch state via rfkill. `rfkill --output TYPE,SOFT
    // --noheadings` prints one "<type> <blocked|unblocked>" line per
    // device; we care about the `wlan` and `bluetooth` types. SOFT
    // blocked == radio off; we expose the inverse (`*RadioOn`) so the
    // tile's `active` reads naturally.
    // -------------------------------------------------------------------
    property bool wifiRadioOn: false
    property bool btRadioOn:   false

    function refreshRfkill() { rfkillInfoProc.running = true }
    function toggleRfkill(type, on) {
        // on == desired post-state (true → unblock the radio).
        rfkillSetProc.command = ["rfkill", on ? "unblock" : "block", type];
        rfkillSetProc.running = true;
        if (type === "wlan")           root.wifiRadioOn = on;
        else if (type === "bluetooth") root.btRadioOn = on;
    }

    Process {
        id: rfkillInfoProc
        command: ["rfkill", "--output", "TYPE,SOFT", "--noheadings"]
        stdout: StdioCollector {
            onStreamFinished: {
                const lines = text.trim().split("\n");
                for (const raw of lines) {
                    const p = raw.trim().split(/\s+/);
                    if (p.length < 2) continue;
                    const on = p[1] === "unblocked";
                    if (p[0] === "wlan")           root.wifiRadioOn = on;
                    else if (p[0] === "bluetooth") root.btRadioOn = on;
                }
            }
        }
    }
    Process { id: rfkillSetProc; onExited: root.refreshRfkill() }

    Timer {
        id: rfkillPoll
        interval: 5000
        running: root.isOpen
        repeat: true
        onTriggered: root.refreshRfkill()
    }

    // -------------------------------------------------------------------
    // Night Light via a managed gammastep process (constant warm tint:
    // `-P` resets gamma ramps first, equal day/night temps `-t 3500:3500`
    // → no time-of-day ramp). nightLightReady gates the tile.
    // -------------------------------------------------------------------
    property bool nightLightOn:    false
    property bool nightLightReady: false

    Process {
        id: nightLightProbe
        command: ["sh", "-c", "command -v gammastep"]
        Component.onCompleted: running = true
        stdout: StdioCollector {
            onStreamFinished: root.nightLightReady = text.trim().length > 0
        }
    }
    Process {
        id: nightLightProc
        running: root.nightLightOn && root.nightLightReady
        command: ["gammastep", "-P", "-t", "3500:3500"]
    }

    // -------------------------------------------------------------------
    // Screen recording via a managed wf-recorder process (wlr-screencopy,
    // the sway-native recorder). Started with a timestamped file under
    // $XDG_VIDEOS_DIR (or ~/Videos); `exec` so the SIGINT we send on stop
    // reaches wf-recorder itself (not the wrapping sh), letting it flush
    // and finalize the container. screenRecordReady gates the tile.
    // -------------------------------------------------------------------
    property bool screenRecording: false
    property bool screenRecordReady: false

    Process {
        id: screenRecordProbe
        command: ["sh", "-c", "command -v wf-recorder"]
        Component.onCompleted: running = true
        stdout: StdioCollector {
            onStreamFinished: root.screenRecordReady = text.trim().length > 0
        }
    }
    Process {
        id: screenRecorderProc
        command: ["sh", "-c",
            "d=\"${XDG_VIDEOS_DIR:-$HOME/Videos}\"; mkdir -p \"$d\"; " +
            "exec wf-recorder -f \"$d/levshell-$(date +%Y%m%d-%H%M%S).mp4\""]
        onStarted: root.screenRecording = true
        onExited: root.screenRecording = false
    }
    function toggleScreenRecording() {
        if (!root.screenRecordReady) return;
        if (root.screenRecording) screenRecorderProc.signal(2);  // SIGINT
        else screenRecorderProc.running = true;
    }

    // -------------------------------------------------------------------
    // VPN via wg-quick on the first /etc/wireguard/*.conf. The probe both
    // gates availability and captures the interface name (conf basename).
    // Live state is read from `wg show interfaces` (best-effort: prints
    // nothing without privilege, in which case the optimistic toggle
    // state stands until the next successful read).
    // -------------------------------------------------------------------
    property bool vpnOn:    false
    property bool vpnReady: false
    property string vpnIface: ""

    Process {
        id: vpnProbe
        command: ["sh", "-c",
            "command -v wg-quick >/dev/null 2>&1 && " +
            "f=$(ls /etc/wireguard/*.conf 2>/dev/null | head -1) && " +
            "[ -n \"$f\" ] && basename \"$f\" .conf"]
        Component.onCompleted: running = true
        stdout: StdioCollector {
            onStreamFinished: {
                const s = text.trim();
                if (s.length > 0) {
                    root.vpnIface = s;
                    root.vpnReady = true;
                    root.refreshVpn();
                }
            }
        }
    }
    Process {
        id: vpnStateProc
        command: ["sh", "-c", "wg show interfaces 2>/dev/null"]
        stdout: StdioCollector {
            onStreamFinished: {
                const ifs = text.trim().split(/\s+/);
                root.vpnOn = root.vpnIface !== "" && ifs.indexOf(root.vpnIface) >= 0;
            }
        }
    }
    function refreshVpn() { if (root.vpnReady) vpnStateProc.running = true; }
    Process { id: vpnSetProc; onExited: root.refreshVpn() }
    function toggleVpn() {
        if (!root.vpnReady) return;
        vpnSetProc.command = ["wg-quick", root.vpnOn ? "down" : "up", root.vpnIface];
        vpnSetProc.running = true;
        root.vpnOn = !root.vpnOn;  // optimistic; vpnSetProc.onExited reconciles
    }

    // -------------------------------------------------------------------
    // Tile expansion (accordion). Wi-Fi → scanned networks (nmcli when
    // present); Bluetooth → paired devices + battery (bluetoothctl).
    // -------------------------------------------------------------------
    property string expandedTile: ""   // "" | "wifi" | "bluetooth"
    function toggleExpand(id) {
        root.expandedTile = (root.expandedTile === id) ? "" : id;
        if (root.expandedTile === "wifi")           root.scanWifi();
        else if (root.expandedTile === "bluetooth") root.scanBt();
    }

    property bool wifiScanReady: false
    property var  wifiNetworks: []   // [{ssid,signal,active}]

    Process {
        id: nmcliProbe
        command: ["sh", "-c", "command -v nmcli"]
        Component.onCompleted: running = true
        stdout: StdioCollector {
            onStreamFinished: root.wifiScanReady = text.trim().length > 0
        }
    }
    Process {
        id: wifiScanProc
        // SSID last so a ':' inside an SSID can't break the terse split.
        // timeout-wrapped: nmcli blocks if NetworkManager is unreachable.
        command: ["sh", "-c",
            "timeout 4 nmcli -t -f IN-USE,SIGNAL,SSID device wifi list"]
        stdout: StdioCollector {
            onStreamFinished: {
                const out = [];
                for (const l of text.trim().split("\n")) {
                    if (!l) continue;
                    const f = l.split(":");
                    if (f.length < 3) continue;
                    const ssid = f.slice(2).join(":");
                    if (!ssid) continue;   // hidden network
                    const sig = parseInt(f[1]);
                    out.push({ ssid: ssid,
                               signal: isNaN(sig) ? 0 : sig,
                               active: f[0] === "*" });
                }
                out.sort((a, b) => (b.active - a.active) || (b.signal - a.signal));
                root.wifiNetworks = out.slice(0, 8);
            }
        }
    }
    function scanWifi() {
        if (root.wifiScanReady) wifiScanProc.running = true;
        else root.wifiNetworks = [];
    }

    property var btDevices: []   // [{mac,name,connected,battery}]

    Process {
        id: btScanProc
        // Every bluetoothctl call is `timeout`-wrapped: with bluetoothd
        // down `bluetoothctl` blocks indefinitely instead of erroring,
        // which would leak a stuck process on every Bluetooth expand.
        command: ["sh", "-c",
            "command -v bluetoothctl >/dev/null 2>&1 || exit 0; " +
            "timeout 2 bluetoothctl devices Paired 2>/dev/null | " +
            "while read -r _ mac name; do " +
            "  info=$(timeout 2 bluetoothctl info \"$mac\" 2>/dev/null); " +
            "  printf '%s %s' \"$info\" | grep -q 'Connected: yes' && c=1 || c=0; " +
            "  b=$(printf '%s' \"$info\" | sed -n " +
            "       's/.*Battery Percentage:.*(\\([0-9]\\{1,3\\}\\)).*/\\1/p'); " +
            "  printf '%s\\t%s\\t%s\\t%s\\n' \"$mac\" \"$c\" \"${b:-}\" \"$name\"; " +
            "done"]
        stdout: StdioCollector {
            onStreamFinished: {
                const out = [];
                for (const l of text.trim().split("\n")) {
                    if (!l) continue;
                    const f = l.split("\t");
                    if (f.length < 4) continue;
                    const b = parseInt(f[2]);
                    out.push({ mac: f[0],
                               connected: f[1] === "1",
                               battery: isNaN(b) ? -1 : b,
                               name: f[3] });
                }
                out.sort((a, b) => (b.connected - a.connected));
                root.btDevices = out.slice(0, 8);
            }
        }
    }
    function scanBt() { btScanProc.running = true; }

    // -------------------------------------------------------------------
    // Tile model. `available` gates input + full-strength rendering;
    // `expandable` adds the caret affordance + detail card.
    // -------------------------------------------------------------------
    readonly property var tiles: [
        { id: "wifi",       icon: root.wifiRadioOn ? Theme.iconWifiHigh
                                                   : Theme.iconWifiSlash,
          label: "Wi-Fi",          active: root.wifiRadioOn,
          available: true,                  expandable: true },
        { id: "bluetooth",  icon: Theme.iconBluetooth,
          label: "Bluetooth",      active: root.btRadioOn,
          available: true,                  expandable: true },
        { id: "dnd",        icon: Theme.iconBellSlash,
          label: "Do Not Disturb", active: root.doNotDisturb,
          available: true,                  expandable: false },
        { id: "nightlight", icon: Theme.iconMoon,
          label: "Night Light",    active: root.nightLightOn,
          available: root.nightLightReady,  expandable: false },
        { id: "screenrec",  icon: Theme.iconVideoCamera,
          label: "Screen Rec",     active: root.screenRecording,
          available: root.screenRecordReady, expandable: false },
        { id: "vpn",        icon: Theme.iconShieldCheck,
          label: "VPN",            active: root.vpnOn,
          available: root.vpnReady,         expandable: false },
    ]

    function activateTile(id) {
        switch (id) {
        case "wifi":       root.toggleRfkill("wlan", !root.wifiRadioOn); break;
        case "bluetooth":  root.toggleRfkill("bluetooth", !root.btRadioOn); break;
        case "dnd":        root.dndToggled(); break;
        case "nightlight": root.nightLightOn = !root.nightLightOn; break;
        case "screenrec":  root.toggleScreenRecording(); break;
        case "vpn":        root.toggleVpn(); break;
        }
    }

    implicitWidth: Math.round(400 * Theme.uiScale)
    implicitHeight: contentCol.implicitHeight + 2 * Theme.panelInnerPadding
    Behavior on implicitHeight {
        NumberAnimation { duration: Theme.motionFast; easing.type: Easing.OutCubic }
    }

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.panelOpacityBattery : Theme.panelOpacity)
    Behavior on color { ColorAnimation { duration: Theme.motionNormal } }
    radius: Theme.panelCornerRadius
    border.width: Theme.panelBorderWidth
    border.color: Theme.outline
    antialiasing: true

    layer.enabled: true
    layer.effect: MultiEffect {
        shadowEnabled: true; shadowColor: "#000000"
        blurMax: Theme.panelShadowBlur; shadowBlur: 1.0
        shadowVerticalOffset: Theme.panelShadowOffsetY
        shadowOpacity: Theme.panelShadowOpacity
        autoPaddingEnabled: true
    }

    opacity: 0.0; scale: 0.96
    transformOrigin: Item.TopRight

    states: [
        State { name: "open"; when: root.isOpen
            PropertyChanges { target: root; opacity: 1.0; scale: 1.0 } }
    ]
    transitions: [
        Transition { from: ""; to: "open"
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionFast }
                SpringAnimation { property: "scale"; spring: Theme.springDefault
                    damping: Theme.springDefaultDamping; mass: Theme.springMass; epsilon: 0.005 }
            }
        },
        Transition { from: "open"; to: ""
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionFast }
                SpringAnimation { property: "scale"; spring: Theme.springSnappy
                    damping: Theme.springSnappyDamping; mass: Theme.springMass; epsilon: 0.005 }
            }
        }
    ]

    MouseArea { anchors.fill: parent; onClicked: (e) => e.accepted = true }

    // -------------------------------------------------------------------
    // Inline slider component. Emits moved() continuously while dragging
    // and released() on mouse-up so consumers can choose to write on every
    // frame (volume — cheap) or only on release (brightness — shells out).
    // -------------------------------------------------------------------
    component LevSlider: Item {
        id: slider
        property real value: 0.0
        property color fillColor: Theme.primary
        property bool dim: false
        signal moved(real v)
        signal released(real v)

        implicitHeight: Math.round(18 * Theme.uiScale)

        Rectangle {
            id: track
            anchors.verticalCenter: parent.verticalCenter
            width: parent.width
            height: Math.round(6 * Theme.uiScale)
            radius: height / 2
            color: Theme.surfaceRaised

            Rectangle {
                width: parent.width * Math.max(0, Math.min(1, slider.value))
                height: parent.height
                radius: parent.radius
                color: slider.dim ? Theme.fgSubtle : slider.fillColor
                Behavior on color { ColorAnimation { duration: Theme.motionFast } }
            }
        }

        MouseArea {
            id: hit
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            preventStealing: true
            onPressed: (e) => applyX(e.x)
            onPositionChanged: (e) => { if (pressed) applyX(e.x) }
            onReleased: slider.released(slider.value)
            function applyX(x) {
                const v = Math.max(0, Math.min(1, x / width));
                slider.value = v;
                slider.moved(v);
            }
        }
    }

    // A single detail row (used by both Wi-Fi and Bluetooth lists).
    component DetailRow: Rectangle {
        property string primary: ""
        property string trailing: ""
        property bool highlight: false
        width: parent ? parent.width : 0
        height: Math.round(28 * Theme.uiScale)
        radius: Theme.panelCornerRadius / 2
        color: highlight ? Qt.rgba(Theme.primary.r, Theme.primary.g,
                                   Theme.primary.b, 0.16) : "transparent"
        Text {
            anchors.left: parent.left
            anchors.leftMargin: Theme.spaceSm
            anchors.verticalCenter: parent.verticalCenter
            width: parent.width - trailingText.width - 3 * Theme.spaceSm
            elide: Text.ElideRight
            text: parent.primary
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeBody
        }
        Text {
            id: trailingText
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceSm
            anchors.verticalCenter: parent.verticalCenter
            text: parent.trailing
            color: Theme.fgSubtle
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
        }
    }

    Column {
        id: contentCol
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        Text {
            text: "Quick Settings"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        Grid {
            columns: 2
            width: parent.width
            spacing: Theme.spaceSm

            Repeater {
                model: root.tiles
                delegate: Rectangle {
                    id: tile
                    required property var modelData
                    required property int index
                    readonly property bool avail: modelData.available
                    readonly property bool isExpanded:
                        root.expandedTile === modelData.id
                    width: (parent.width - Theme.spaceSm) / 2
                    height: Math.round(56 * Theme.uiScale)
                    radius: Theme.panelCornerRadius
                    color: modelData.active ? Theme.primary : Theme.surfaceRaised
                    border.width: modelData.active ? 0
                                  : (isExpanded ? 1.5 : 1)
                    border.color: isExpanded ? Theme.primary : Theme.outline
                    opacity: avail ? 1.0 : 0.45
                    Behavior on color { ColorAnimation { duration: Theme.motionFast } }

                    Row {
                        anchors.centerIn: parent
                        spacing: Theme.spaceSm

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: tile.modelData.icon
                            color: tile.modelData.active ? Theme.textOnPrimary : Theme.fg
                            font.family: Theme.fontIcon
                            font.pixelSize: Theme.iconSize
                        }

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: tile.modelData.label
                            color: tile.modelData.active ? Theme.textOnPrimary : Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeLabel
                            font.weight: Theme.typeLabelWeight
                        }
                    }

                    // Primary action — fills the tile, sits *under* the
                    // caret hit-area so the caret wins in its corner.
                    MouseArea {
                        anchors.fill: parent
                        enabled: tile.avail
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.activateTile(tile.modelData.id)
                    }

                    // Expand caret (Wi-Fi / Bluetooth only). Its own
                    // hit-area in the top-right corner; rotates when open.
                    Text {
                        id: caret
                        visible: tile.modelData.expandable
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.rightMargin: Theme.spaceXs
                        anchors.topMargin: Theme.spaceXs
                        text: Theme.iconCaretDown
                        color: tile.modelData.active ? Theme.textOnPrimary
                                                     : Theme.fgSubtle
                        font.family: Theme.fontIcon
                        font.pixelSize: Math.round(13 * Theme.uiScale)
                        rotation: tile.isExpanded ? 180 : 0
                        Behavior on rotation {
                            NumberAnimation { duration: Theme.motionFast }
                        }
                        MouseArea {
                            anchors.fill: parent
                            anchors.margins: -Math.round(6 * Theme.uiScale)   // easier to hit
                            enabled: tile.modelData.expandable
                            cursorShape: Qt.PointingHandCursor
                            onClicked: (e) => {
                                e.accepted = true;
                                root.toggleExpand(tile.modelData.id);
                            }
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------------
        // Inline detail card (accordion). Visible only when a tile is
        // expanded; content switches on root.expandedTile.
        // ---------------------------------------------------------------
        Rectangle {
            id: detailCard
            visible: root.expandedTile !== ""
            width: parent.width
            implicitHeight: visible
                ? detailCol.implicitHeight + 2 * Theme.spaceSm : 0
            height: implicitHeight
            radius: Theme.panelCornerRadius
            color: Theme.surfaceRaised
            border.width: 1
            border.color: Theme.outline
            clip: true

            Column {
                id: detailCol
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.margins: Theme.spaceSm
                spacing: Theme.spaceXs

                // Header: title + refresh.
                Item {
                    width: parent.width
                    height: Math.round(22 * Theme.uiScale)
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.expandedTile === "wifi"
                              ? "Available networks"
                              : "Paired devices"
                        color: Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeLabel
                        font.weight: Theme.typeLabelWeight
                    }
                    Text {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: Theme.iconMagnifyingGlass
                        color: Theme.fgSubtle
                        font.family: Theme.fontIcon
                        font.pixelSize: Math.round(14 * Theme.uiScale)
                        MouseArea {
                            anchors.fill: parent
                            anchors.margins: -Math.round(6 * Theme.uiScale)
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.expandedTile === "wifi"
                                       ? root.scanWifi() : root.scanBt()
                        }
                    }
                }

                Rectangle {
                    width: parent.width; height: 1
                    color: Theme.outline; opacity: 0.5
                }

                // Wi-Fi: scanned SSIDs, or an honest no-backend note.
                Text {
                    visible: root.expandedTile === "wifi"
                             && !root.wifiScanReady
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: "No network manager (NetworkManager/iwd absent) "
                          + "— radio toggle only."
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                }
                Text {
                    visible: root.expandedTile === "wifi"
                             && root.wifiScanReady
                             && root.wifiNetworks.length === 0
                    text: "No networks found."
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                }
                Repeater {
                    model: root.expandedTile === "wifi"
                           ? root.wifiNetworks : []
                    delegate: DetailRow {
                        required property var modelData
                        primary: modelData.ssid
                        trailing: modelData.signal + "%"
                        highlight: modelData.active
                    }
                }

                // Bluetooth: paired devices + connection/battery.
                Text {
                    visible: root.expandedTile === "bluetooth"
                             && root.btDevices.length === 0
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: "No paired devices (is bluetoothd running?)."
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                }
                Repeater {
                    model: root.expandedTile === "bluetooth"
                           ? root.btDevices : []
                    delegate: DetailRow {
                        required property var modelData
                        primary: modelData.name
                        highlight: modelData.connected
                        trailing: (modelData.connected ? "connected" : "paired")
                            + (modelData.battery >= 0
                               ? "  ·  " + modelData.battery + "%" : "")
                    }
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Volume row: mute-toggle icon + slider.
        Row {
            width: parent.width
            spacing: Theme.spaceSm

            Text {
                id: volIcon
                width: Math.round(28 * Theme.uiScale)
                anchors.verticalCenter: parent.verticalCenter
                horizontalAlignment: Text.AlignHCenter
                text: root.audioMuted || !root.sinkReady
                    ? Theme.iconSpeakerSlash
                    : Theme.iconSpeakerHigh
                color: root.sinkReady ? Theme.fg : Theme.fgSubtle
                font.family: Theme.fontIcon
                font.pixelSize: Theme.iconSize

                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    enabled: root.sinkReady
                    onClicked: root.toggleMute()
                }
            }

            LevSlider {
                id: volSlider
                width: parent.width - volIcon.width - Theme.spaceSm
                anchors.verticalCenter: parent.verticalCenter
                dim: root.audioMuted || !root.sinkReady
                // Bidirectional: reflect sink volume unless the user is dragging.
                value: root.audioVolume
                onMoved: (v) => root.setVolume(v)
                onReleased: (v) => root.setVolume(v)
            }
        }

        // Brightness row: sun icon + slider.
        Row {
            width: parent.width
            spacing: Theme.spaceSm

            Text {
                id: brightIcon
                width: Math.round(28 * Theme.uiScale)
                anchors.verticalCenter: parent.verticalCenter
                horizontalAlignment: Text.AlignHCenter
                text: Theme.iconSun
                color: root.brightnessReady ? Theme.fg : Theme.fgSubtle
                font.family: Theme.fontIcon
                font.pixelSize: Theme.iconSize
            }

            LevSlider {
                id: brightSlider
                width: parent.width - brightIcon.width - Theme.spaceSm
                anchors.verticalCenter: parent.verticalCenter
                dim: !root.brightnessReady
                value: root.brightnessFrac
                // Drag feedback is instant in-QML; the shell-out happens
                // on release so we don't spawn a process per frame.
                onReleased: (v) => root.setBrightness(v)
            }
        }
    }
}
