// SettingsPanel — on-shell settings overlay.
//
// A centred card opened by `levshell-ctl settings open|toggle`. It has
// no socket of its own: every control emits `action(name, data)`, which
// main.qml forwards as a ShellMessage::SettingsAction. The daemon's
// handle_settings_action re-dispatches through the *same* runtime paths
// as the `scale` / `density` / `theme` ctl commands, so behaviour is
// identical regardless of entry point. State shown here is daemon-
// authoritative (pushed down via props) — the panel never assumes a
// change took effect; it waits for the daemon to echo it back.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property real uiScale: 1.0
    property string density: "full"
    property bool presentationOn: false
    property string themeName: ""
    property string themeVariant: "dark"

    // action: a settings_action verb; data: optional JSON object.
    signal action(string name, var data)
    signal dismiss()

    readonly property var scaleSteps: [1.0, 1.25, 1.5, 1.75, 2.0]
    readonly property var densitySteps: ["full", "compact", "hidden"]

    implicitWidth: Math.round(420 * Theme.uiScale)
    implicitHeight: col.implicitHeight + 2 * Theme.panelInnerPadding

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.panelOpacityBattery : Theme.panelOpacity)
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
    transformOrigin: Item.Center
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

    // Swallow clicks on the card so the scrim's close handler only
    // fires for genuine outside clicks.
    MouseArea { anchors.fill: parent; onClicked: (e) => e.accepted = true }

    // A labelled row of mutually-exclusive choice chips.
    component SegmentedRow: Column {
        id: seg
        property string label: ""
        property var options: []          // [{ key, text }]
        property string current: ""
        signal pick(string key)
        width: parent ? parent.width : 0
        spacing: Theme.spaceSm

        Text {
            text: seg.label
            color: Theme.fgSubtle
            font.family: Theme.fontText
            font.pixelSize: Theme.typeCaption
            font.weight: Theme.typeTitleWeight
        }
        Row {
            width: parent.width
            spacing: Theme.spaceSm
            Repeater {
                model: seg.options
                delegate: Rectangle {
                    required property var modelData
                    readonly property bool active: modelData.key === seg.current
                    width: (seg.width - (seg.options.length - 1) * Theme.spaceSm)
                           / Math.max(1, seg.options.length)
                    height: Math.round(34 * Theme.uiScale)
                    radius: 6
                    color: active ? Theme.primary
                          : (chipHit.containsMouse ? Theme.surfaceRaised : "transparent")
                    border.width: 1
                    border.color: active ? Theme.primary : Theme.outline
                    Behavior on color { ColorAnimation { duration: Theme.motionFast } }

                    Text {
                        anchors.centerIn: parent
                        text: modelData.text
                        color: parent.active ? Theme.textOnPrimary : Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                    }
                    MouseArea {
                        id: chipHit
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: seg.pick(modelData.key)
                    }
                }
            }
        }
    }

    // A labelled on/off row.
    component ToggleRow: Row {
        id: tog
        property string label: ""
        property bool on: false
        signal toggled()
        width: parent ? parent.width : 0
        spacing: Theme.spaceSm

        Text {
            anchors.verticalCenter: parent.verticalCenter
            width: parent.width - track.width - Theme.spaceSm
            text: tog.label
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeBody
        }
        Rectangle {
            id: track
            anchors.verticalCenter: parent.verticalCenter
            width: Math.round(44 * Theme.uiScale)
            height: Math.round(24 * Theme.uiScale)
            radius: height / 2
            color: tog.on ? Theme.primary : Theme.outline
            Behavior on color { ColorAnimation { duration: Theme.motionFast } }
            Rectangle {
                width: parent.height - 4
                height: parent.height - 4
                radius: height / 2
                y: 2
                x: tog.on ? parent.width - width - 2 : 2
                color: Theme.surface
                Behavior on x { NumberAnimation { duration: Theme.motionFast } }
            }
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: tog.toggled()
            }
        }
    }

    Column {
        id: col
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceLg

        Row {
            width: parent.width
            Text {
                text: "Settings"
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeHeadline
                font.weight: Theme.typeTitleWeight
            }
            Item { width: parent.width - 1; height: 1 }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "✕"
                color: closeHit.containsMouse ? Theme.fg : Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeTitle
                MouseArea {
                    id: closeHit
                    anchors.fill: parent
                    anchors.margins: -Theme.spaceSm
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.dismiss()
                }
            }
        }

        SegmentedRow {
            label: "UI SCALE"
            current: root.uiScale.toFixed(2)
            options: root.scaleSteps.map(function (s) {
                return { key: s.toFixed(2), text: Math.round(s * 100) + "%" };
            })
            onPick: (key) => root.action("set_scale", { value: key })
        }

        SegmentedRow {
            label: "BAR DENSITY"
            current: root.density
            options: root.densitySteps.map(function (d) {
                return { key: d, text: d.charAt(0).toUpperCase() + d.slice(1) };
            })
            onPick: (key) => root.action("set_density", { mode: key })
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        Row {
            width: parent.width
            Column {
                width: parent.width - modeBtn.width - Theme.spaceSm
                spacing: 2
                Text {
                    text: "Theme"
                    color: Theme.fg
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeBody
                }
                Text {
                    text: (root.themeName || "—") + " · " + root.themeVariant
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                }
            }
            Rectangle {
                id: modeBtn
                anchors.verticalCenter: parent.verticalCenter
                width: Math.round(96 * Theme.uiScale)
                height: Math.round(34 * Theme.uiScale)
                radius: 6
                color: modeHit.containsMouse ? Theme.surfaceRaised : "transparent"
                border.width: 1
                border.color: Theme.outline
                Text {
                    anchors.centerIn: parent
                    text: "Toggle mode"
                    color: Theme.fg
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                }
                MouseArea {
                    id: modeHit
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.action("theme_toggle_mode", {})
                }
            }
        }

        ToggleRow {
            label: "Presentation mode"
            on: root.presentationOn
            onToggled: root.action("presentation", { on: !root.presentationOn })
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Persist the *current* runtime scale + density into
        // levshell.toml (comments preserved daemon-side). Two writes
        // because the keys live in different sections.
        Rectangle {
            width: parent.width
            height: Math.round(38 * Theme.uiScale)
            radius: 6
            color: saveHit.containsMouse ? Theme.surfaceRaised : "transparent"
            border.width: 1
            border.color: Theme.outline
            Text {
                anchors.centerIn: parent
                text: "Save current as defaults"
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeBody
            }
            MouseArea {
                id: saveHit
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    root.action("persist", {
                        key: "appearance.ui_scale",
                        value: root.uiScale.toFixed(2)
                    });
                    root.action("persist", {
                        key: "shell.density",
                        value: root.density
                    });
                }
            }
        }
    }
}
