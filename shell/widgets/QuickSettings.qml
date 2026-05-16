// QuickSettings — quick-settings flyout (§2.1.7, §12.4).
//
// Toggle tiles (Wi-Fi / Bluetooth / DnD / Night-Light) remain placeholders
// until the respective subsystems are wired in. Volume and brightness
// sliders are backed by real state:
//
//   - Volume  → Quickshell.Services.Pipewire default sink. `audio.volume`
//               and `audio.muted` are writable so long as the node is
//               held by a PwObjectTracker. The track fill reads from the
//               sink; dragging or tapping the slider writes back.
//
//   - Brightness → `brightnessctl -m info` parses to
//                  `device,class,current,percent,max`. We read the 4th
//                  field on flyout open (and every 5s while open so
//                  keyboard brightness keys reflect) and write via
//                  `brightnessctl -q set N%`.
//
// The slider is a minimal custom Item (no QtQuick.Controls dep) — plain
// Rectangle track with a dragged handle and a filling Rectangle to the
// handle's left.

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
    // Brightness via brightnessctl. No device argument — brightnessctl
    // auto-selects the first backlight class device, which matches what
    // keyboard brightness keys (e.g. XF86MonBrightnessUp) target.
    // -------------------------------------------------------------------
    property real brightnessFrac: 0.0   // 0..1
    property bool brightnessReady: false

    function refreshBrightness() { brightnessInfoProc.running = true }
    function setBrightness(frac) {
        const pct = Math.max(1, Math.min(100, Math.round(frac * 100)));
        brightnessSetProc.command = ["brightnessctl", "-q", "set", pct + "%"];
        brightnessSetProc.running = true;
        root.brightnessFrac = pct / 100;
    }

    Process {
        id: brightnessInfoProc
        command: ["brightnessctl", "-m", "info"]
        stdout: StdioCollector {
            onStreamFinished: {
                const line = text.trim();
                const parts = line.split(",");
                if (parts.length >= 4) {
                    const pct = parseFloat(parts[3].replace("%", ""));
                    if (!isNaN(pct)) {
                        root.brightnessFrac = pct / 100;
                        root.brightnessReady = true;
                    }
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
    Component.onCompleted: refreshBrightness()
    onIsOpenChanged: if (isOpen) refreshBrightness()

    // -------------------------------------------------------------------
    // Tile model — only DnD is functional.
    // -------------------------------------------------------------------
    readonly property var tiles: [
        { icon: Theme.iconWifiHigh,   label: "Wi-Fi",          active: true,             stub: true  },
        { icon: "\uE0A0",             label: "Bluetooth",      active: false,            stub: true  },
        { icon: Theme.iconBellSlash,  label: "Do Not Disturb", active: root.doNotDisturb, stub: false },
        { icon: "\uE334",             label: "Night Light",    active: false,            stub: true  },
    ]

    implicitWidth: 400
    implicitHeight: contentCol.implicitHeight + 2 * Theme.panelInnerPadding

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

        implicitHeight: 18

        Rectangle {
            id: track
            anchors.verticalCenter: parent.verticalCenter
            width: parent.width
            height: 6
            radius: 3
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
                    required property var modelData
                    required property int index
                    width: (parent.width - Theme.spaceSm) / 2
                    height: 56
                    radius: Theme.panelCornerRadius
                    color: modelData.active ? Theme.primary : Theme.surfaceRaised
                    border.width: modelData.active ? 0 : 1
                    border.color: Theme.outline
                    Behavior on color { ColorAnimation { duration: Theme.motionFast } }

                    Row {
                        anchors.centerIn: parent
                        spacing: Theme.spaceSm

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: modelData.icon
                            color: modelData.active ? Theme.textOnPrimary : Theme.fg
                            font.family: Theme.fontIcon
                            font.pixelSize: 20
                        }

                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: modelData.label
                            color: modelData.active ? Theme.textOnPrimary : Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeLabel
                            font.weight: Theme.typeLabelWeight
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            if (modelData.stub) return;
                            if (modelData.label === "Do Not Disturb")
                                root.dndToggled();
                        }
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
                width: 28
                anchors.verticalCenter: parent.verticalCenter
                horizontalAlignment: Text.AlignHCenter
                text: root.audioMuted || !root.sinkReady
                    ? Theme.iconSpeakerSlash
                    : Theme.iconSpeakerHigh
                color: root.sinkReady ? Theme.fg : Theme.fgSubtle
                font.family: Theme.fontIcon
                font.pixelSize: 20

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
                width: 28
                anchors.verticalCenter: parent.verticalCenter
                horizontalAlignment: Text.AlignHCenter
                text: Theme.iconSun
                color: root.brightnessReady ? Theme.fg : Theme.fgSubtle
                font.family: Theme.fontIcon
                font.pixelSize: 20
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
