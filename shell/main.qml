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
import Quickshell.Wayland._BackgroundEffect
import Quickshell.Services.Notifications
import "."
import "widgets"

Scope {
    id: shell

    // ----------------------------------------------------------------------
    // Phosphor Icons font (§8). The family name "Phosphor" set in
    // Theme.fontIcon resolves to this bundled TTF. Widgets and the
    // palette reference `Theme.icon*` PUA codepoints with
    // `font.family: Theme.fontIcon` to render outlined icons at any
    // size. The FontLoader is owned by the top-level Scope so it
    // stays alive for the entire shell lifetime.
    // ----------------------------------------------------------------------
    FontLoader {
        id: phosphorFont
        source: "fonts/Phosphor.ttf"
    }

    // ----------------------------------------------------------------------
    // Freedesktop notification server (§2.1.6).
    //
    // Quickshell's NotificationServer claims org.freedesktop.Notifications
    // on the session D-Bus and receives all desktop notifications. Setting
    // `tracked = true` on each incoming notification keeps it in
    // `trackedNotifications` until the user explicitly dismisses it.
    // ----------------------------------------------------------------------
    NotificationServer {
        id: notifServer
        bodySupported: true
        imageSupported: true
        actionsSupported: true
        inlineReplySupported: true
        persistenceSupported: true

        onNotification: (notification) => {
            notification.tracked = true;
            shell.notifArrivalTimes[notification.id] = Date.now();
            shell.notifArrivalTimes = shell.notifArrivalTimes; // trigger change
        }
    }

    property bool notificationCenterOpen: false
    property bool doNotDisturb: false
    property var notifArrivalTimes: ({})
    property bool clockHubOpen: false
    property bool quickSettingsOpen: false

    function toggleNotificationCenter() {
        notificationCenterOpen = !notificationCenterOpen;
        if (notificationCenterOpen) { clockHubOpen = false; quickSettingsOpen = false; }
    }
    function toggleClockHub() {
        clockHubOpen = !clockHubOpen;
        if (clockHubOpen) { notificationCenterOpen = false; quickSettingsOpen = false; }
    }
    function toggleQuickSettings() {
        quickSettingsOpen = !quickSettingsOpen;
        if (quickSettingsOpen) { notificationCenterOpen = false; clockHubOpen = false; }
    }

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
        case "power_state": {
            Theme.onBattery = msg.on_battery || false;
            break;
        }
        case "bar_density_state": {
            Theme.density = msg.mode || "full";
            break;
        }
        case "theme": {
            shell.applyTheme(msg);
            break;
        }
        default:
            break;
        }
    }

    // Apply a DaemonMessage::Theme payload. The payload mirrors the
    // TOML theme file structure from spec design doc §11 — every
    // override field is optional, and missing fields leave the
    // existing Theme.qml property at its current value. That keeps
    // partial community themes valid without forcing them to
    // duplicate every hex value.
    function applyTheme(msg) {
        if (msg.name) Theme.themeName = msg.name;
        if (msg.variant) Theme.mode = msg.variant;

        const c = msg.colors || {};
        if (c.bg) Theme.bg = c.bg;
        if (c.bg_dark) Theme.bgDark = c.bg_dark;
        if (c.surface) Theme.surface = c.surface;
        if (c.surface_raised) Theme.surfaceRaised = c.surface_raised;
        if (c.overlay) Theme.overlay = c.overlay;
        if (c.fg) Theme.fg = c.fg;
        if (c.fg_muted) Theme.fgMuted = c.fg_muted;
        if (c.fg_subtle) Theme.fgSubtle = c.fg_subtle;
        if (c.on_primary) Theme.textOnPrimary = c.on_primary;
        if (c.on_surface) Theme.textOnSurface = c.on_surface;
        if (c.outline) Theme.outline = c.outline;
        if (c.primary) Theme.primary = c.primary;
        if (c.primary_variant) Theme.primaryVariant = c.primary_variant;
        if (c.secondary) Theme.secondary = c.secondary;
        if (c.secondary_variant) Theme.secondaryVariant = c.secondary_variant;
        if (c.tertiary) Theme.tertiary = c.tertiary;
        if (c.success) Theme.success = c.success;
        if (c.warning) Theme.warning = c.warning;
        if (c.error) Theme.error = c.error;
        if (c.info) Theme.info = c.info;

        const h = msg.health || {};
        if (h.stale_pill) Theme.stalePill = h.stale_pill;
        if (h.error_pill) Theme.errorPill = h.error_pill;

        const b = msg.bar || {};
        if (b.opacity !== undefined && b.opacity !== null) Theme.barOpacity = b.opacity;
        if (b.blur_radius !== undefined && b.blur_radius !== null) Theme.barBlurRadius = b.blur_radius;
        if (b.opacity_battery !== undefined && b.opacity_battery !== null) Theme.barOpacityBattery = b.opacity_battery;
        if (b.blur_radius_battery !== undefined && b.blur_radius_battery !== null) Theme.barBlurRadiusBattery = b.blur_radius_battery;
        if (b.height_full !== undefined && b.height_full !== null) Theme.barHeightFull = b.height_full;
        if (b.height_compact !== undefined && b.height_compact !== null) Theme.barHeightCompact = b.height_compact;

        const t = msg.typography || {};
        if (t.font_text) Theme.fontText = t.font_text;
        if (t.font_mono) Theme.fontMono = t.font_mono;
        if (t.font_icon) Theme.fontIcon = t.font_icon;
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
            implicitHeight: Theme.barHeight

            Behavior on implicitHeight {
                SpringAnimation {
                    spring:  Theme.springDefault
                    damping: Theme.springDefaultCriticalDamping
                    mass:    Theme.springMass
                    epsilon: 0.5
                }
            }

            // §3.1.3 — semi-transparent surface in blur mode, opaque on
            // battery. The alpha channel drives the translucency; the
            // BackgroundEffect attachment below requests compositor-side
            // blur (no-op on Sway today, active on KWin / when
            // ext_background_effect_v1 lands in wlroots).
            color: Qt.rgba(Theme.surface.r, Theme.surface.g,
                           Theme.surface.b,
                           Theme.onBattery ? Theme.barOpacityBattery
                                           : Theme.barOpacity)
            Behavior on color {
                ColorAnimation { duration: Theme.motionNormal }
            }

            BackgroundEffect.blurRegion: Region {
                width: panel.width
                height: panel.height
            }

            // Inner-shadow contrast floor — §3.2.1. A 1-2px dark strip
            // along the bar's bottom edge provides a subtle but
            // guaranteed contrast anchor even against bright wallpapers.
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
                spacing: Theme.interWidgetGap

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
                spacing: Theme.interWidgetGap

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
                spacing: Theme.interWidgetGap

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

        BackgroundEffect.blurRegion: Region {
            item: paletteOverlay
        }

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
            anchors.topMargin: Theme.barHeight + Theme.spaceLg
            paletteData: shell.paletteState
            onQueryChanged: (text) => shell.sendPaletteQuery(text)
            onSelect: (provider, itemId) => shell.sendPaletteSelect(provider, itemId)
            onClose: () => shell.sendPaletteClose()
        }
    }

    // ----------------------------------------------------------------------
    // Notification center overlay window (§12.3).
    //
    // Same pattern as the palette overlay: a full-screen transparent
    // PanelWindow with click-outside-to-close, hosting the
    // NotificationCenter card anchored top-right below the bar.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: notifWindow

        property bool isClosing: false
        visible: shell.notificationCenterOpen || isClosing

        Connections {
            target: shell
            function onNotificationCenterOpenChanged() {
                if (shell.notificationCenterOpen) {
                    notifWindow.isClosing = false;
                    notifCloseTimer.stop();
                } else if (notifWindow.visible && !notifWindow.isClosing) {
                    notifWindow.isClosing = true;
                    notifCloseTimer.restart();
                }
            }
        }

        Timer {
            id: notifCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: notifWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-notifications"

        BackgroundEffect.blurRegion: Region {
            item: notifCenter
        }

        anchors {
            top: true
            left: true
            right: true
            bottom: true
        }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.notificationCenterOpen = false
        }

        NotificationCenter {
            id: notifCenter
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            notifModel: notifServer.trackedNotifications
            arrivalTimes: shell.notifArrivalTimes
            doNotDisturb: shell.doNotDisturb
            onDndToggled: shell.doNotDisturb = !shell.doNotDisturb
            onCloseRequested: shell.notificationCenterOpen = false
            isOpen: shell.notificationCenterOpen
        }
    }

    // ----------------------------------------------------------------------
    // Clock & calendar hub overlay (§2.1.5 scaffold).
    //
    // Placeholder: mini calendar, upcoming events, world-clock row.
    // Content will be populated when CalDAV sync adapter lands in Phase 2.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: clockHubWindow

        property bool isClosing: false
        visible: shell.clockHubOpen || isClosing

        Connections {
            target: shell
            function onClockHubOpenChanged() {
                if (shell.clockHubOpen) {
                    clockHubWindow.isClosing = false;
                    clockHubCloseTimer.stop();
                } else if (clockHubWindow.visible && !clockHubWindow.isClosing) {
                    clockHubWindow.isClosing = true;
                    clockHubCloseTimer.restart();
                }
            }
        }

        Timer {
            id: clockHubCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: clockHubWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-clock-hub"

        BackgroundEffect.blurRegion: Region {
            item: clockHubPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.clockHubOpen = false
        }

        ClockHub {
            id: clockHubPanel
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.clockHubOpen
        }
    }

    // ----------------------------------------------------------------------
    // Quick-settings flyout overlay (§2.1.7 scaffold).
    //
    // Placeholder: volume/brightness sliders, toggle tiles for Wi-Fi,
    // Bluetooth, DnD, night-light. Will wire to PipeWire and
    // brightnessctl in a later phase.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: quickSettingsWindow

        property bool isClosing: false
        visible: shell.quickSettingsOpen || isClosing

        Connections {
            target: shell
            function onQuickSettingsOpenChanged() {
                if (shell.quickSettingsOpen) {
                    quickSettingsWindow.isClosing = false;
                    qsCloseTimer.stop();
                } else if (quickSettingsWindow.visible && !quickSettingsWindow.isClosing) {
                    quickSettingsWindow.isClosing = true;
                    qsCloseTimer.restart();
                }
            }
        }

        Timer {
            id: qsCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: quickSettingsWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-quick-settings"

        BackgroundEffect.blurRegion: Region {
            item: quickSettingsPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.quickSettingsOpen = false
        }

        QuickSettings {
            id: quickSettingsPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.quickSettingsOpen
            doNotDisturb: shell.doNotDisturb
            onDndToggled: shell.doNotDisturb = !shell.doNotDisturb
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
