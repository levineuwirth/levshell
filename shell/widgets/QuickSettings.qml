// QuickSettings — quick-settings flyout scaffold (§2.1.7).
//
// Phase 1 scaffold: 2-column toggle-tile grid with placeholder tiles
// for Wi-Fi, Bluetooth, DnD, Night Light. Volume and brightness
// sliders are stubs. Full implementation (PipeWire for audio,
// brightnessctl for display, BlueZ D-Bus for Bluetooth) lands in a
// later phase.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property bool doNotDisturb: false
    signal dndToggled()

    // Tile model: each tile has an icon, label, and active state.
    // Phase 1: only DnD is functional; others are visual stubs.
    readonly property var tiles: [
        { icon: Theme.iconWifiHigh,   label: "Wi-Fi",      active: true,  stub: true },
        { icon: "\uE0A0",            label: "Bluetooth",   active: false, stub: true },
        { icon: Theme.iconBellSlash,  label: "Do Not Disturb", active: root.doNotDisturb, stub: false },
        { icon: "\uE334",            label: "Night Light", active: false, stub: true },
    ]

    implicitWidth: 320
    implicitHeight: contentCol.implicitHeight + 2 * Theme.panelInnerPadding

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.barOpacityBattery : Theme.barOpacity)
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

    // Open/close animation.
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

    Column {
        id: contentCol
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        // Header.
        Text {
            text: "Quick Settings"
            color: Theme.fg
            font.family: Theme.fontText
            font.pixelSize: Theme.typeTitle
            font.weight: Theme.typeTitleWeight
        }

        // Toggle tile grid (2 columns).
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

                    Behavior on color {
                        ColorAnimation { duration: Theme.motionFast }
                    }

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
                            if (modelData.label === "Do Not Disturb") {
                                root.dndToggled();
                            }
                        }
                    }
                }
            }
        }

        // Divider.
        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Volume slider placeholder.
        Column {
            width: parent.width
            spacing: Theme.spaceXs

            Text {
                text: "Volume"
                color: Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeLabel
                font.weight: Theme.typeLabelWeight
            }

            Rectangle {
                width: parent.width
                height: 6
                radius: 3
                color: Theme.surfaceRaised

                Rectangle {
                    width: parent.width * 0.65
                    height: parent.height
                    radius: parent.radius
                    color: Theme.primary
                }
            }
        }

        // Brightness slider placeholder.
        Column {
            width: parent.width
            spacing: Theme.spaceXs

            Text {
                text: "Brightness"
                color: Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeLabel
                font.weight: Theme.typeLabelWeight
            }

            Rectangle {
                width: parent.width
                height: 6
                radius: 3
                color: Theme.surfaceRaised

                Rectangle {
                    width: parent.width * 0.80
                    height: parent.height
                    radius: parent.radius
                    color: Theme.primary
                }
            }
        }
    }
}
