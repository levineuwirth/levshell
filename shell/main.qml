//@ pragma UseQApplication
// Levshell QML shell — Phase 1.4 bar.
//
// `UseQApplication`: required so the system-tray SNI context menus
// (SystemTrayWidget → item.display()) can render — Quickshell platform
// /D-Bus menus need QApplication (QtWidgets) mode. No effect on the
// rest of the shell.
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
            // Visible confirmation in the quickshell log that the bus
            // claim succeeded — if `notify-send` produces no output here,
            // another daemon (dunst/mako/swaync) holds
            // org.freedesktop.Notifications.
            console.log("levshell: notif", notification.appName, "—",
                        notification.summary);
            // Always track so it persists in the center for later
            // review. If DnD is active, flag it muted: it won't count
            // toward the bell's unread badge (this shell has no toast/
            // sound surface, so badge-exclusion is the silence).
            notification.tracked = true;
            if (shell.doNotDisturb) {
                shell.mutedNotifIds[notification.id] = true;
                shell.mutedNotifIds = shell.mutedNotifIds; // trigger change
            } else {
                shell.notifArrivalTimes[notification.id] = Date.now();
                shell.notifArrivalTimes = shell.notifArrivalTimes; // trigger change
            }
        }
    }

    property bool notificationCenterOpen: false
    property bool doNotDisturb: false
    property var notifArrivalTimes: ({})
    // Ids of notifications received while DnD was active — persisted but
    // excluded from the unread badge (see NotificationsWidget).
    property var mutedNotifIds: ({})
    // Notification snooze/pin (§2.1.6), shell-local. pinnedNotifIds:
    // id -> true (immune to snooze, marked). snoozedNotifUntil:
    // id -> epoch-ms; hidden from the center until that time passes.
    property var pinnedNotifIds: ({})
    property var snoozedNotifUntil: ({})
    property bool clockHubOpen: false
    property bool quickSettingsOpen: false
    // SSH fleet detail dropdown (§2.10.1). Its host list is the
    // `ssh-fleet` widget state, read via shell.stateFor in the overlay.
    property bool sshFleetOpen: false
    // GPU fleet detail dropdown (§2.10.4); host/GPU list is the
    // `gpu-fleet` widget state.
    property bool gpuFleetOpen: false
    // Remote SLURM jobs dropdown (§2.10.3); host/job list is the
    // `remote-jobs` widget state.
    property bool remoteJobsOpen: false
    // Reference-library dropdown (§2.9.8); stats + recent papers from
    // the `reference-library` widget state.
    property bool refLibraryOpen: false
    // Project-pulse dropdown (§2.9.4/§2.9.12); project dashboard +
    // deadlines from the `project-pulse` widget state.
    property bool projectPulseOpen: false
    // arXiv watcher dropdown (§2.9.9); new papers from the
    // `arxiv-watch` widget state.
    property bool arxivWatchOpen: false
    // Bar density as chosen by the daemon. Theme.density is bound to an
    // *effective* value (see the Binding by the bar): in hidden mode,
    // pointing at the top screen edge transiently reveals the full bar
    // (spec §2.1.4) without disturbing this daemon-owned value.
    property string daemonDensity: "full"
    property bool barRevealed: false
    property bool warmupOpen: false
    property var warmupPayload: ({ fired_at: "", events: [], anki_due_count: 0, projects: [] })
    // Upcoming-events feed for the clock dropdown (DaemonMessage::ClockHub).
    property var clockHubPayload: ({ generated_at: "", events: [] })
    // CPU process sniper (§2.3.5, DaemonMessage::ProcessList).
    property bool processSniperOpen: false
    property var processListPayload: ({ generated_at: "", processes: [] })

    // Rubber-duck (§2.12.6). Messages are plain objects
    //   { role: "user" | "assistant", content: string }
    // Streaming tokens append to the latest assistant message; when
    // duckStreaming is true the input field disables send.
    property bool duckOpen: false
    property bool duckStreaming: false
    property var duckMessages: []

    // Presentation mode (spec §2.18): mute non-critical surfaces for
    // talks / screen-sharing. Driven by the daemon's theme service.
    property bool presentationMode: false

    // Ideation nudge toast (§2.9.2). Single-slot — nudges are Poisson-
    // spaced (λ≈45min) so the latest simply replaces any showing one.
    // Auto-dismisses; click dismisses early.
    property var currentNudge: ({ kind: "", title: "" })
    property bool nudgeVisible: false
    function showNudge(msg) {
        // The daemon already drops nudges in presentation mode and
        // during a work session; guard here too in case one was in
        // flight when the quiet state flipped.
        if (shell.quietMode) return;
        shell.currentNudge = { kind: msg.kind || "", title: msg.title || "" };
        shell.nudgeVisible = true;
        nudgeDismissTimer.restart();
    }
    Timer {
        id: nudgeDismissTimer
        interval: 7000
        onTriggered: shell.nudgeVisible = false
    }

    // Focus-mode indicator source (spec §10). Derived from the
    // session-timer widget state the shell already receives — no extra
    // wire surface. `sessionPhase` is "idle" | "work" | "break".
    readonly property string sessionPhase:
        (shell.widgetStates["session-timer"] || ({})).phase || "idle"
    readonly property bool sessionPaused:
        (shell.widgetStates["session-timer"] || ({})).paused === true
    readonly property bool sessionRunning:
        sessionPhase === "work" || sessionPhase === "break"

    // Spec §10: automatic content-muting. A work session quiets the
    // desktop the same way manual presentation mode does — that's the
    // whole point of starting a focus session. A mid-session *pause*
    // stays muted: you're still notionally in the session, and the
    // daemon's `focus_work` (derived from FocusSession events, which a
    // pause does not emit) stays set regardless — so the bar must not
    // visibly un-recede while notifications are still being swallowed.
    // The two are independent inputs OR'd into one effective quiet
    // state; ending the Pomodoro must not clear a manually-set
    // presentation mode, and vice-versa.
    readonly property bool focusWorkActive:
        sessionPhase === "work"
    readonly property bool quietMode:
        presentationMode || focusWorkActive

    // Close every bar dropdown except `keep` (a property name or "").
    function closeDropdownsExcept(keep) {
        if (keep !== "notificationCenterOpen") notificationCenterOpen = false;
        if (keep !== "clockHubOpen")           clockHubOpen = false;
        if (keep !== "quickSettingsOpen")      quickSettingsOpen = false;
        if (keep !== "sshFleetOpen")           sshFleetOpen = false;
        if (keep !== "gpuFleetOpen")           gpuFleetOpen = false;
        if (keep !== "remoteJobsOpen")         remoteJobsOpen = false;
        if (keep !== "refLibraryOpen")         refLibraryOpen = false;
        if (keep !== "projectPulseOpen")       projectPulseOpen = false;
        if (keep !== "arxivWatchOpen")         arxivWatchOpen = false;
    }
    function toggleNotificationCenter() {
        notificationCenterOpen = !notificationCenterOpen;
        if (notificationCenterOpen) closeDropdownsExcept("notificationCenterOpen");
    }
    function toggleClockHub() {
        clockHubOpen = !clockHubOpen;
        if (clockHubOpen) closeDropdownsExcept("clockHubOpen");
    }
    function toggleQuickSettings() {
        quickSettingsOpen = !quickSettingsOpen;
        if (quickSettingsOpen) closeDropdownsExcept("quickSettingsOpen");
    }
    function toggleSshFleet() {
        sshFleetOpen = !sshFleetOpen;
        if (sshFleetOpen) closeDropdownsExcept("sshFleetOpen");
    }
    function toggleGpuFleet() {
        gpuFleetOpen = !gpuFleetOpen;
        if (gpuFleetOpen) closeDropdownsExcept("gpuFleetOpen");
    }
    function toggleRemoteJobs() {
        remoteJobsOpen = !remoteJobsOpen;
        if (remoteJobsOpen) closeDropdownsExcept("remoteJobsOpen");
    }
    function toggleRefLibrary() {
        refLibraryOpen = !refLibraryOpen;
        if (refLibraryOpen) closeDropdownsExcept("refLibraryOpen");
    }
    function toggleProjectPulse() {
        projectPulseOpen = !projectPulseOpen;
        if (projectPulseOpen) closeDropdownsExcept("projectPulseOpen");
    }
    function toggleArxivWatch() {
        arxivWatchOpen = !arxivWatchOpen;
        if (arxivWatchOpen) {
            closeDropdownsExcept("arxivWatchOpen");
            // Opening the list is the acknowledgement — clear the badge.
            shell.sendShellMessage({
                type: "widget_action", widget_id: "arxiv-watch",
                action: "ack", data: {}
            });
        }
    }
    function sendArxivOpen(url) {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "arxiv-watch",
            action: "open", data: { url: url }
        });
    }
    function sendRefCopy(citekey) {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "reference-library",
            action: "copy", data: { citekey: citekey }
        });
    }
    function sendSshReconnect(host) {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "ssh-fleet",
            action: "reconnect", data: { host: host }
        });
    }
    function sendTimerToggle() {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "session-timer",
            action: "toggle", data: {}
        });
    }
    function sendPowerProfileCycle() {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "power-profile",
            action: "cycle", data: {}
        });
    }
    function sendLatexOpenLog() {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "latex-status",
            action: "open_log", data: {}
        });
    }
    function openProcessSniper() {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "cpu",
            action: "list_processes", data: {}
        });
        processSniperOpen = true;
    }
    function killProcess(pid, signal) {
        shell.sendShellMessage({
            type: "widget_action", widget_id: "cpu",
            action: "kill_process", data: { pid: pid, signal: signal }
        });
    }

    // ----------------------------------------------------------------------
    // Reactive state stores.
    // ----------------------------------------------------------------------
    // Using plain objects so that re-assignment through Object.assign
    // triggers QML's property-change notifications. Mutating in place
    // would not.
    property var widgetStates: ({})
    property var widgetStatuses: ({})
    property var widgetEscalations: ({})
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
        "interruption-cost": interruptionCostComponent,
        "clock": clockComponent,
        "cpu": cpuComponent,
        "memory": memoryComponent,
        "battery": batteryComponent,
        "network": networkComponent,
        "disk": diskComponent,
        "power-profile": powerProfileComponent,
        "notifications": notificationsComponent,
        "control-center": controlCenterComponent,
        "system-tray": systemTrayComponent,
        "ssh-fleet": sshDashboardComponent,
        "gpu-fleet": gpuDashboardComponent,
        "remote-jobs": remoteJobsComponent,
        "anki-due": ankiDueComponent,
        "session-timer": sessionTimerComponent,
        "reference-library": referenceLibraryComponent,
        "project-pulse": projectPulseComponent,
        "latex-status": latexStatusComponent,
        "arxiv-watch": arxivWatchComponent
    })

    Component { id: workspaceIndicatorComponent; WorkspaceIndicator {} }
    Component { id: interruptionCostComponent; InterruptionCostWidget {} }
    Component { id: clockComponent; ClockWidget {} }
    Component { id: cpuComponent; CpuWidget {} }
    Component { id: memoryComponent; MemoryWidget {} }
    Component { id: batteryComponent; BatteryWidget {} }
    Component { id: networkComponent; NetworkWidget {} }
    Component { id: diskComponent; DiskWidget {} }
    Component { id: powerProfileComponent; PowerProfileWidget {} }
    Component { id: notificationsComponent; NotificationsWidget {} }
    Component { id: controlCenterComponent; ControlCenterWidget {} }
    Component { id: systemTrayComponent; SystemTrayWidget {} }
    Component { id: sshDashboardComponent; SshDashboard {} }
    Component { id: gpuDashboardComponent; GpuDashboard {} }
    Component { id: remoteJobsComponent; RemoteJobsWidget {} }
    Component { id: ankiDueComponent; AnkiDueWidget {} }
    Component { id: sessionTimerComponent; SessionTimerWidget {} }
    Component { id: referenceLibraryComponent; ReferenceLibraryWidget {} }
    Component { id: projectPulseComponent; ProjectPulseWidget {} }
    Component { id: latexStatusComponent; LatexStatusWidget {} }
    Component { id: arxivWatchComponent; ArxivWatchWidget {} }

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

            const esc = Object.assign({}, shell.widgetEscalations);
            esc[id] = msg.escalation || "ambient";
            shell.widgetEscalations = esc;
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
            // Effective Theme.density is derived (see the Binding near
            // the bar) so hidden-mode edge reveal can transiently force
            // "full" without losing the daemon's chosen density.
            shell.daemonDensity = msg.mode || "full";
            break;
        }
        case "theme": {
            shell.applyTheme(msg);
            break;
        }
        case "warmup": {
            // Non-critical surface — suppressed during a talk or a
            // running work session.
            if (shell.quietMode) break;
            shell.warmupPayload = {
                fired_at: msg.fired_at || "",
                events: msg.events || [],
                anki_due_count: msg.anki_due_count || 0,
                projects: msg.projects || []
            };
            shell.warmupOpen = true;
            break;
        }
        case "clock_hub": {
            shell.clockHubPayload = {
                generated_at: msg.generated_at || "",
                events: msg.events || []
            };
            break;
        }
        case "process_list": {
            shell.processListPayload = {
                generated_at: msg.generated_at || "",
                processes: msg.processes || []
            };
            break;
        }
        case "duck_open": {
            // Guarded by presentation mode only, not the broader quiet
            // state: the duck is an explicitly user-invoked focus *aid*
            // (`ctl duck open`), so a running work session must not
            // swallow it — only a talk/screen-share should.
            if (shell.presentationMode) break;
            shell.duckOpen = true;
            break;
        }
        case "duck_close": {
            shell.duckOpen = false;
            break;
        }
        case "duck_reset": {
            shell.duckMessages = [];
            shell.duckStreaming = false;
            break;
        }
        case "duck_token": {
            shell.appendDuckToken(msg);
            break;
        }
        case "nudge": {
            shell.showNudge(msg);
            break;
        }
        case "presentation_mode": {
            shell.presentationMode = !!msg.on;
            // Entering presentation mode tears down any non-critical
            // surface that's currently up.
            if (shell.presentationMode) {
                shell.warmupOpen = false;
                shell.duckOpen = false;
            }
            break;
        }
        case "critical_escalation": {
            // Spec design §9 rule 3: a widget crossed into Critical.
            // The in-bar red pill + one-time flash is already rendered
            // from the escalation field on `widget_update`; this
            // separate channel is the user's escape hatch when the
            // widget is hidden. For now we log — a future phase will
            // hand it off to an OS notification bridge.
            console.warn("levshell: critical escalation",
                         msg.widget_id, "—", msg.title, ":", msg.body);
            break;
        }
        default:
            break;
        }
    }

    // Append a DuckToken frame to duckMessages. If there's no active
    // assistant turn yet (the last message is a user turn, or the
    // list is empty), open a new one with the delta; otherwise
    // concatenate onto the trailing assistant message. `done` flips
    // duckStreaming back off so the input re-enables.
    function appendDuckToken(msg) {
        const role = msg.role || "assistant";
        const delta = msg.delta || "";
        const done = msg.done === true;

        if (delta.length > 0) {
            const messages = shell.duckMessages.slice();
            const last = messages.length > 0 ? messages[messages.length - 1] : null;
            if (last && last.role === role) {
                messages[messages.length - 1] = {
                    role: last.role,
                    content: last.content + delta
                };
            } else {
                messages.push({ role: role, content: delta });
            }
            shell.duckMessages = messages;
        }

        if (done) {
            shell.duckStreaming = false;
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

    function escalationFor(widgetId) {
        return shell.widgetEscalations[widgetId] || "ambient";
    }

    function stateFor(widgetId) {
        return shell.widgetStates[widgetId] || null;
    }

    function componentFor(widgetId) {
        return shell.widgetRegistry[widgetId] || null;
    }

    // ----------------------------------------------------------------------
    // Hidden-bar edge reveal (§2.1.4).
    //
    // Theme.density is the *effective* density: normally the daemon's
    // value, but transiently "full" when the bar is hidden and the
    // pointer is at the top screen edge (or over the revealed bar).
    // Binding the singleton property means every density-derived token
    // (bar height, icon size, widget prominence) follows automatically,
    // and the existing implicitHeight SpringAnimation does the slide.
    // ----------------------------------------------------------------------
    Binding {
        target: Theme
        property: "density"
        value: (shell.daemonDensity === "hidden" && shell.barRevealed)
                ? "full" : shell.daemonDensity
    }

    property bool barEdgeHovered: false
    property bool barPanelHovered: false
    function recomputeBarReveal() {
        if (shell.barEdgeHovered || shell.barPanelHovered) {
            barRevealHideTimer.stop();
            shell.barRevealed = true;
        } else {
            barRevealHideTimer.restart();
        }
    }
    Timer {
        id: barRevealHideTimer
        interval: 500
        onTriggered: if (!shell.barEdgeHovered && !shell.barPanelHovered)
                         shell.barRevealed = false
    }

    // A 4px transparent strip pinned to the top edge, present only in
    // hidden density, that catches the pointer push and triggers the
    // reveal. exclusiveZone 0 so it reserves no layout space.
    Variants {
        model: Quickshell.screens
        delegate: PanelWindow {
            required property var modelData
            screen: modelData
            visible: shell.daemonDensity === "hidden"
            anchors { top: true; left: true; right: true }
            implicitHeight: 4
            exclusiveZone: 0
            color: "transparent"
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
            WlrLayershell.namespace: "levshell-bar-reveal"

            HoverHandler {
                onHoveredChanged: {
                    shell.barEdgeHovered = hovered;
                    shell.recomputeBarReveal();
                }
            }
        }
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

            // Persistent focus indicator — spec §10. A 2px bottom-edge
            // line while a focus session runs: primary for work, success
            // for a break, dimmed when paused. Additive (sits above the
            // contrast strip); no widget chrome is touched, so the §10.1
            // escalation > health > focus stacking is untouched here.
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: 2
                visible: opacity > 0.01
                opacity: shell.sessionRunning ? (shell.sessionPaused ? 0.35 : 1.0) : 0.0
                color: shell.sessionPhase === "break" ? Theme.success : Theme.primary
                Behavior on opacity {
                    NumberAnimation { duration: Theme.motionNormal; easing.type: Easing.OutCubic }
                }
                Behavior on color {
                    ColorAnimation { duration: Theme.motionNormal }
                }
            }

            // Keeps the bar revealed while the pointer is over it in
            // hidden mode; the linger timer hides it on leave.
            HoverHandler {
                onHoveredChanged: {
                    shell.barPanelHovered = hovered;
                    shell.recomputeBarReveal();
                }
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
                            item.escalation = Qt.binding(() => shell.escalationFor(modelData));
                            item.quiet = Qt.binding(() => shell.quietMode);
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
                            item.escalation = Qt.binding(() => shell.escalationFor(modelData));
                            item.quiet = Qt.binding(() => shell.quietMode);
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
                            item.escalation = Qt.binding(() => shell.escalationFor(modelData));
                            item.quiet = Qt.binding(() => shell.quietMode);
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

    // Rubber-duck send (§2.12.6). Appends the user turn locally so the
    // overlay reflects it immediately, then sends the text to the
    // daemon and flips duckStreaming true until the `done` token
    // frame arrives.
    function sendDuckMessage(text) {
        const trimmed = text.trim();
        if (trimmed.length === 0 || shell.duckStreaming) return;
        const messages = shell.duckMessages.slice();
        messages.push({ role: "user", content: trimmed });
        shell.duckMessages = messages;
        shell.duckStreaming = true;
        shell.sendShellMessage({ type: "duck_say", text: trimmed });
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
            pinnedIds: shell.pinnedNotifIds
            snoozedUntil: shell.snoozedNotifUntil
            onDndToggled: shell.doNotDisturb = !shell.doNotDisturb
            onCloseRequested: shell.notificationCenterOpen = false
            onPinToggled: (nId) => {
                if (shell.pinnedNotifIds[nId]) delete shell.pinnedNotifIds[nId];
                else shell.pinnedNotifIds[nId] = true;
                shell.pinnedNotifIds = shell.pinnedNotifIds; // notify
            }
            onSnoozeRequested: (nId) => {
                // 10-minute snooze; resurfaces automatically.
                shell.snoozedNotifUntil[nId] = Date.now() + 600000;
                shell.snoozedNotifUntil = shell.snoozedNotifUntil; // notify
            }
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
            payload: shell.clockHubPayload
        }
    }

    // ----------------------------------------------------------------------
    // Quick-settings flyout overlay (§2.1.7).
    //
    // Fully wired: PipeWire volume + xbacklight brightness sliders;
    // Wi-Fi/Bluetooth/DnD/Night-Light/Screen-Rec/VPN toggle tiles; Wi-Fi
    // and Bluetooth tiles expand to an inline detail card. See
    // QuickSettings.qml for the per-backend gating rationale.
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
    // CPU process sniper overlay (§2.3.5).
    // ----------------------------------------------------------------------
    PanelWindow {
        id: processSniperWindow

        property bool isClosing: false
        visible: shell.processSniperOpen || isClosing

        Connections {
            target: shell
            function onProcessSniperOpenChanged() {
                if (shell.processSniperOpen) {
                    processSniperWindow.isClosing = false;
                    snipeCloseTimer.stop();
                } else if (processSniperWindow.visible && !processSniperWindow.isClosing) {
                    processSniperWindow.isClosing = true;
                    snipeCloseTimer.restart();
                }
            }
        }

        Timer {
            id: snipeCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: processSniperWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-proc-sniper"

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.processSniperOpen = false
        }

        ProcessSniper {
            id: processSniperPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.processSniperOpen
            payload: shell.processListPayload
            onKill: (pid, signal) => shell.killProcess(pid, signal)
            onRefresh: shell.openProcessSniper()
        }
    }

    // ----------------------------------------------------------------------
    // SSH fleet detail overlay (§2.10.1).
    //
    // Top-right dropdown anchored under the bar, opened from the
    // `ssh-fleet` bar widget. Mirrors the notification-center / process-
    // sniper overlay pattern. Reconnect rows route a widget_action to
    // the daemon's ssh-monitor module via the M1.1 passthrough.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: sshFleetWindow

        property bool isClosing: false
        visible: shell.sshFleetOpen || isClosing

        Connections {
            target: shell
            function onSshFleetOpenChanged() {
                if (shell.sshFleetOpen) {
                    sshFleetWindow.isClosing = false;
                    sshFleetCloseTimer.stop();
                } else if (sshFleetWindow.visible && !sshFleetWindow.isClosing) {
                    sshFleetWindow.isClosing = true;
                    sshFleetCloseTimer.restart();
                }
            }
        }

        Timer {
            id: sshFleetCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: sshFleetWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-ssh-fleet"

        BackgroundEffect.blurRegion: Region {
            item: sshFleetPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.sshFleetOpen = false
        }

        SshFleetPanel {
            id: sshFleetPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.sshFleetOpen
            payload: shell.stateFor("ssh-fleet") || ({ hosts: [] })
            onReconnect: (host) => shell.sendSshReconnect(host)
        }
    }

    // ----------------------------------------------------------------------
    // GPU fleet detail overlay (§2.10.4). Monitor-only — no actions.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: gpuFleetWindow

        property bool isClosing: false
        visible: shell.gpuFleetOpen || isClosing

        Connections {
            target: shell
            function onGpuFleetOpenChanged() {
                if (shell.gpuFleetOpen) {
                    gpuFleetWindow.isClosing = false;
                    gpuFleetCloseTimer.stop();
                } else if (gpuFleetWindow.visible && !gpuFleetWindow.isClosing) {
                    gpuFleetWindow.isClosing = true;
                    gpuFleetCloseTimer.restart();
                }
            }
        }

        Timer {
            id: gpuFleetCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: gpuFleetWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-gpu-fleet"

        BackgroundEffect.blurRegion: Region {
            item: gpuFleetPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.gpuFleetOpen = false
        }

        GpuFleetPanel {
            id: gpuFleetPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.gpuFleetOpen
            payload: shell.stateFor("gpu-fleet") || ({ hosts: [] })
        }
    }

    // ----------------------------------------------------------------------
    // Remote jobs detail overlay (§2.10.3). Monitor-only — no actions.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: remoteJobsWindow

        property bool isClosing: false
        visible: shell.remoteJobsOpen || isClosing

        Connections {
            target: shell
            function onRemoteJobsOpenChanged() {
                if (shell.remoteJobsOpen) {
                    remoteJobsWindow.isClosing = false;
                    remoteJobsCloseTimer.stop();
                } else if (remoteJobsWindow.visible && !remoteJobsWindow.isClosing) {
                    remoteJobsWindow.isClosing = true;
                    remoteJobsCloseTimer.restart();
                }
            }
        }

        Timer {
            id: remoteJobsCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: remoteJobsWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-remote-jobs"

        BackgroundEffect.blurRegion: Region {
            item: remoteJobsPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.remoteJobsOpen = false
        }

        RemoteJobsPanel {
            id: remoteJobsPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.remoteJobsOpen
            payload: shell.stateFor("remote-jobs") || ({ hosts: [] })
        }
    }

    // ----------------------------------------------------------------------
    // Reference library detail overlay (§2.9.8).
    // ----------------------------------------------------------------------
    PanelWindow {
        id: refLibraryWindow

        property bool isClosing: false
        visible: shell.refLibraryOpen || isClosing

        Connections {
            target: shell
            function onRefLibraryOpenChanged() {
                if (shell.refLibraryOpen) {
                    refLibraryWindow.isClosing = false;
                    refLibraryCloseTimer.stop();
                } else if (refLibraryWindow.visible && !refLibraryWindow.isClosing) {
                    refLibraryWindow.isClosing = true;
                    refLibraryCloseTimer.restart();
                }
            }
        }

        Timer {
            id: refLibraryCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: refLibraryWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-reference-library"

        BackgroundEffect.blurRegion: Region {
            item: refLibraryPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.refLibraryOpen = false
        }

        ReferenceLibraryPanel {
            id: refLibraryPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.refLibraryOpen
            payload: shell.stateFor("reference-library")
                     || ({ total: 0, unread: 0, recent_count: 0, recent: [] })
            onCopyCitekey: (citekey) => shell.sendRefCopy(citekey)
        }
    }

    // ----------------------------------------------------------------------
    // Project pulse / deadline overlay (§2.9.4, §2.9.12).
    // ----------------------------------------------------------------------
    PanelWindow {
        id: projectPulseWindow

        property bool isClosing: false
        visible: shell.projectPulseOpen || isClosing

        Connections {
            target: shell
            function onProjectPulseOpenChanged() {
                if (shell.projectPulseOpen) {
                    projectPulseWindow.isClosing = false;
                    projectPulseCloseTimer.stop();
                } else if (projectPulseWindow.visible && !projectPulseWindow.isClosing) {
                    projectPulseWindow.isClosing = true;
                    projectPulseCloseTimer.restart();
                }
            }
        }

        Timer {
            id: projectPulseCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: projectPulseWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-project-pulse"

        BackgroundEffect.blurRegion: Region {
            item: projectPulsePanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.projectPulseOpen = false
        }

        ProjectPulsePanel {
            id: projectPulsePanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.projectPulseOpen
            payload: shell.stateFor("project-pulse")
                     || ({ active_today: 0, dormant: 0, projects: [], deadlines: [] })
        }
    }

    // ----------------------------------------------------------------------
    // arXiv new-papers overlay (§2.9.9).
    // ----------------------------------------------------------------------
    PanelWindow {
        id: arxivWatchWindow

        property bool isClosing: false
        visible: shell.arxivWatchOpen || isClosing

        Connections {
            target: shell
            function onArxivWatchOpenChanged() {
                if (shell.arxivWatchOpen) {
                    arxivWatchWindow.isClosing = false;
                    arxivWatchCloseTimer.stop();
                } else if (arxivWatchWindow.visible && !arxivWatchWindow.isClosing) {
                    arxivWatchWindow.isClosing = true;
                    arxivWatchCloseTimer.restart();
                }
            }
        }

        Timer {
            id: arxivWatchCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: arxivWatchWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-arxiv-watch"

        BackgroundEffect.blurRegion: Region {
            item: arxivWatchPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"

        MouseArea {
            anchors.fill: parent
            onClicked: shell.arxivWatchOpen = false
        }

        ArxivWatchPanel {
            id: arxivWatchPanel
            anchors.right: parent.right
            anchors.rightMargin: Theme.spaceLg
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceSm
            isOpen: shell.arxivWatchOpen
            payload: shell.stateFor("arxiv-watch") || ({ new_count: 0, items: [] })
            onOpenPaper: (url) => shell.sendArxivOpen(url)
        }
    }

    // ----------------------------------------------------------------------
    // Warmup overlay (§2.12.1).
    //
    // Centered card fired on first activity after a ≥4h gap, or via
    // `levshell-ctl warmup open`. Keyboard focus is grabbed so Escape
    // dismisses. Clicking outside also dismisses.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: warmupWindow

        property bool isClosing: false
        visible: shell.warmupOpen || isClosing

        Connections {
            target: shell
            function onWarmupOpenChanged() {
                if (shell.warmupOpen) {
                    warmupWindow.isClosing = false;
                    warmupCloseTimer.stop();
                } else if (warmupWindow.visible && !warmupWindow.isClosing) {
                    warmupWindow.isClosing = true;
                    warmupCloseTimer.restart();
                }
            }
        }

        Timer {
            id: warmupCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: warmupWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
        WlrLayershell.namespace: "levshell-warmup"

        BackgroundEffect.blurRegion: Region {
            item: warmupPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: Qt.rgba(0, 0, 0, 0.35)

        MouseArea {
            anchors.fill: parent
            onClicked: shell.warmupOpen = false
        }

        WarmupOverlay {
            id: warmupPanel
            anchors.centerIn: parent
            isOpen: shell.warmupOpen
            payload: shell.warmupPayload
            onDismissed: shell.warmupOpen = false
        }
    }

    // ----------------------------------------------------------------------
    // Rubber-duck overlay (spec §2.12.6).
    //
    // Same shell as warmup: full-screen darkened PanelWindow with a
    // centered chat card. Escape closes the overlay (conversation
    // persists; `ctl duck reset` wipes it). The TextInput holds
    // exclusive keyboard focus via WlrLayershell.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: duckWindow

        property bool isClosing: false
        visible: shell.duckOpen || isClosing

        Connections {
            target: shell
            function onDuckOpenChanged() {
                if (shell.duckOpen) {
                    duckWindow.isClosing = false;
                    duckCloseTimer.stop();
                } else if (duckWindow.visible && !duckWindow.isClosing) {
                    duckWindow.isClosing = true;
                    duckCloseTimer.restart();
                }
            }
        }

        Timer {
            id: duckCloseTimer
            interval: Theme.motionSlow + 50
            onTriggered: duckWindow.isClosing = false
        }

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
        WlrLayershell.namespace: "levshell-duck"

        BackgroundEffect.blurRegion: Region {
            item: duckPanel
        }

        anchors { top: true; left: true; right: true; bottom: true }
        color: Qt.rgba(0, 0, 0, 0.35)

        MouseArea {
            anchors.fill: parent
            onClicked: shell.duckOpen = false
        }

        RubberDuckOverlay {
            id: duckPanel
            anchors.centerIn: parent
            isOpen: shell.duckOpen
            messages: shell.duckMessages
            streaming: shell.duckStreaming
            onDismissed: shell.duckOpen = false
            onSubmit: (text) => shell.sendDuckMessage(text)
        }
    }

    // ----------------------------------------------------------------------
    // Ideation nudge toast (§2.9.2).
    //
    // Non-modal, keyboard-transparent, top-center under the bar. Fades
    // in/out via the inner card opacity; the PanelWindow lingers through
    // the fade-out so the animation isn't clipped.
    // ----------------------------------------------------------------------
    PanelWindow {
        id: nudgeWindow
        visible: shell.nudgeVisible || nudgeCard.opacity > 0.01

        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        WlrLayershell.namespace: "levshell-nudge"

        anchors { top: true; left: true; right: true; bottom: true }
        color: "transparent"
        // Click-through everywhere except the card itself.
        mask: Region { item: nudgeCard }

        BackgroundEffect.blurRegion: Region { item: nudgeCard }

        Rectangle {
            id: nudgeCard
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: Theme.barHeight + Theme.spaceMd

            implicitWidth: nudgeCol.implicitWidth + 2 * Theme.panelInnerPadding
            implicitHeight: nudgeCol.implicitHeight + 2 * Theme.panelInnerPadding

            color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                           Theme.onBattery ? Theme.panelOpacityBattery : Theme.panelOpacity)
            radius: Theme.panelCornerRadius
            border.width: Theme.panelBorderWidth
            border.color: Theme.outline
            antialiasing: true

            opacity: shell.nudgeVisible ? 1.0 : 0.0
            Behavior on opacity {
                NumberAnimation { duration: Theme.motionNormal; easing.type: Easing.OutCubic }
            }

            Row {
                id: nudgeCol
                anchors.centerIn: parent
                spacing: Theme.spaceSm

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: Theme.iconNote
                    color: Theme.primary
                    font.family: Theme.fontIcon
                    font.pixelSize: Theme.iconSize
                }
                Column {
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 1
                    Text {
                        text: (shell.currentNudge.kind || "nudge").replace(/_/g, " ")
                        color: Theme.fgMuted
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                        font.capitalization: Font.Capitalize
                    }
                    Text {
                        text: shell.currentNudge.title || ""
                        color: Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                        font.weight: Theme.typeBodyEmphasisWeight
                    }
                }
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: shell.nudgeVisible = false
            }
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
