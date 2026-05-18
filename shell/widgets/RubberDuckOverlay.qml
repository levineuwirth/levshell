// RubberDuckOverlay — spec §2.12.6.
//
// Minimal chat card. ListView of messages (user right, assistant left),
// TextField for input, Send button + Enter submit. The daemon streams
// tokens via DaemonMessage::DuckToken; the shell concatenates deltas
// onto the trailing assistant message. `streaming` is true between
// send and the final `done` frame — Send disables during that window.
//
// Dismissed by Escape or click-outside (handled by the parent
// PanelWindow). No auto-timeout — the duck is a deliberate chat
// surface, not an ambient notification.

import QtQuick
import QtQuick.Controls
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var messages: []
    property bool streaming: false

    signal dismissed()
    signal submit(string text)

    implicitWidth: Math.round(640 * Theme.uiScale)
    implicitHeight: Math.round(560 * Theme.uiScale)

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

    opacity: 0.0; scale: 0.96
    transformOrigin: Item.Center

    states: [
        State { name: "open"; when: root.isOpen
            PropertyChanges { target: root; opacity: 1.0; scale: 1.0 } }
    ]
    transitions: [
        Transition { from: ""; to: "open"
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionNormal }
                SpringAnimation { property: "scale"; spring: Theme.springDefault
                    damping: Theme.springDefaultDamping; mass: Theme.springMass; epsilon: 0.005 }
            }
        },
        Transition { from: "open"; to: ""
            ParallelAnimation {
                NumberAnimation { property: "opacity"; duration: Theme.motionFast }
                NumberAnimation { property: "scale"; duration: Theme.motionFast
                    easing.type: Easing.InCubic }
            }
        }
    ]

    // Catch panel clicks so they don't bubble to the overlay's
    // click-outside-to-dismiss MouseArea.
    MouseArea { anchors.fill: parent; onClicked: (e) => e.accepted = true }

    // Auto-scroll the list when new messages or streaming deltas land.
    function scrollToEnd() {
        if (messageList.count > 0) {
            messageList.positionViewAtEnd();
        }
    }

    onMessagesChanged: Qt.callLater(scrollToEnd)

    Column {
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        // Header.
        Row {
            width: parent.width
            spacing: Theme.spaceMd

            Text {
                text: "Rubber duck"
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeTitle
                font.weight: Theme.typeTitleWeight
                anchors.verticalCenter: parent.verticalCenter
            }
            Text {
                text: "articulate the stuck point"
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.weight: Theme.typeCaptionWeight
                font.italic: true
                anchors.verticalCenter: parent.verticalCenter
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Message list.
        ListView {
            id: messageList
            width: parent.width
            height: parent.height
                - dividerBottom.height - inputRow.implicitHeight
                - 4 * Theme.spaceMd - Theme.typeTitle
            clip: true
            spacing: Theme.spaceSm
            model: root.messages
            boundsBehavior: Flickable.StopAtBounds

            Text {
                anchors.centerIn: parent
                visible: messageList.count === 0
                text: "Tell the duck what you're stuck on."
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeBody
                font.weight: Theme.typeBodyWeight
                font.italic: true
            }

            delegate: Item {
                required property var modelData
                width: messageList.width
                implicitHeight: bubble.implicitHeight + Theme.spaceXs

                Rectangle {
                    id: bubble
                    width: Math.min(implicitWidth, messageList.width * 0.85)
                    implicitWidth: bubbleText.implicitWidth + 2 * Theme.spaceMd
                    implicitHeight: bubbleText.implicitHeight + 2 * Theme.spaceSm
                    anchors {
                        left: modelData.role === "user" ? undefined : parent.left
                        right: modelData.role === "user" ? parent.right : undefined
                    }
                    radius: Theme.panelCornerRadius
                    color: modelData.role === "user"
                        ? Theme.primary
                        : Qt.rgba(Theme.fg.r, Theme.fg.g, Theme.fg.b, 0.08)
                    border.width: modelData.role === "user" ? 0 : 1
                    border.color: Theme.outline

                    Text {
                        id: bubbleText
                        anchors {
                            left: parent.left
                            right: parent.right
                            top: parent.top
                            leftMargin: Theme.spaceMd
                            rightMargin: Theme.spaceMd
                            topMargin: Theme.spaceSm
                        }
                        text: modelData.content
                        color: modelData.role === "user" ? Theme.textOnPrimary : Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                        font.weight: Theme.typeBodyWeight
                        wrapMode: Text.Wrap
                    }
                }
            }
        }

        Rectangle { id: dividerBottom; width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Input row.
        Row {
            id: inputRow
            width: parent.width
            spacing: Theme.spaceMd

            Rectangle {
                id: inputBox
                width: parent.width - sendButton.width - Theme.spaceMd
                height: Math.round(40 * Theme.uiScale)
                radius: Theme.panelCornerRadius
                color: Qt.rgba(Theme.fg.r, Theme.fg.g, Theme.fg.b, 0.06)
                border.width: input.activeFocus ? 1 : 0
                border.color: Theme.primary

                TextInput {
                    id: input
                    anchors.fill: parent
                    anchors.leftMargin: Theme.spaceMd
                    anchors.rightMargin: Theme.spaceMd
                    verticalAlignment: TextInput.AlignVCenter
                    color: Theme.fg
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeBody
                    font.weight: Theme.typeBodyWeight
                    clip: true
                    focus: root.isOpen && !root.streaming
                    enabled: !root.streaming
                    selectionColor: Theme.primary
                    selectedTextColor: Theme.textOnPrimary

                    Keys.onReturnPressed: root.sendAndClear()
                    Keys.onEnterPressed: root.sendAndClear()
                    Keys.onEscapePressed: root.dismissed()
                }

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.left: parent.left
                    anchors.leftMargin: Theme.spaceMd
                    visible: input.text.length === 0
                    text: root.streaming ? "duck is thinking…" : "what's stuck?"
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeBody
                    font.weight: Theme.typeBodyWeight
                    font.italic: true
                }
            }

            Rectangle {
                id: sendButton
                width: sendLabel.implicitWidth + 2 * Theme.spaceMd
                height: Math.round(40 * Theme.uiScale)
                radius: Theme.panelCornerRadius
                color: root.streaming ? Qt.rgba(Theme.primary.r, Theme.primary.g, Theme.primary.b, 0.4)
                                      : Theme.primary
                Text {
                    id: sendLabel
                    anchors.centerIn: parent
                    text: "Send"
                    color: Theme.textOnPrimary
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeLabel
                    font.weight: Theme.typeLabelWeight
                }
                MouseArea {
                    anchors.fill: parent
                    enabled: !root.streaming
                    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: root.sendAndClear()
                }
            }
        }
    }

    function sendAndClear() {
        if (root.streaming) return;
        const text = input.text;
        if (text.trim().length === 0) return;
        root.submit(text);
        input.text = "";
    }

    focus: isOpen
    Keys.onEscapePressed: root.dismissed()
}
