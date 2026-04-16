// NotificationCenter — dropdown panel for desktop notifications (§12.3).
//
// Anchored top-right below the bar by main.qml's notification overlay
// PanelWindow. Groups notifications by application, supports dismiss,
// action invocation, and inline reply. DnD toggle at the top.
//
// Visual spec (§12.3):
//   Width 380px, corner radius 8px, surface bg at barOpacity,
//   1px outline border, drop shadow 0 4px 16px rgba(0,0,0,0.3).
//   Notification entries: 28px app icon, title (typeBodyEmphasis),
//   body (typeBody), timestamp (typeCaption, fgMuted), action buttons.
//   Unread notifications have a small primary dot.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    // =================================================================
    // INPUTS
    // =================================================================
    property var notifModel: null
    property var arrivalTimes: ({})
    property bool doNotDisturb: false
    property bool isOpen: false

    signal dndToggled()
    signal closeRequested()

    // =================================================================
    // NOTIFICATION GROUPING
    //
    // trackedNotifications is a flat model ordered by arrival. We
    // re-sort into app-contiguous buckets for ListView.section, same
    // pattern as CommandPalette's groupedPayload.
    // =================================================================
    readonly property var groupedPayload: {
        if (!notifModel) return { items: [], appCount: 0 };
        const byApp = {};
        const order = [];
        for (let i = 0; i < notifModel.count; i++) {
            const n = notifModel.values[i];
            if (!n) continue;
            const app = n.appName || "Unknown";
            if (!byApp[app]) {
                byApp[app] = [];
                order.push(app);
            }
            byApp[app].push({
                notification: n,
                appName: app,
                summary: n.summary || "",
                body: n.body || "",
                appIcon: n.appIcon || "",
                nId: n.id,
                urgency: n.urgency,
                actions: n.actions || [],
                hasInlineReply: n.hasInlineReply || false,
                inlineReplyPlaceholder: n.inlineReplyPlaceholder || "Reply...",
            });
        }
        const out = [];
        for (let i = 0; i < order.length; i++) {
            const bucket = byApp[order[i]];
            for (let j = 0; j < bucket.length; j++) {
                out.push(bucket[j]);
            }
        }
        return { items: out, appCount: order.length };
    }
    readonly property var displayedNotifs: groupedPayload.items
    readonly property int sectionCount: groupedPayload.appCount

    // =================================================================
    // TIMESTAMP FORMATTING
    // =================================================================
    function relativeTime(notifId) {
        const ts = root.arrivalTimes[notifId];
        if (!ts) return "";
        const delta = Math.floor((Date.now() - ts) / 1000);
        if (delta < 60) return "now";
        if (delta < 3600) return Math.floor(delta / 60) + "m ago";
        if (delta < 86400) return Math.floor(delta / 3600) + "h ago";
        return Math.floor(delta / 86400) + "d ago";
    }

    // Refresh timestamps every 30 seconds.
    Timer {
        running: root.isOpen
        interval: 30000
        repeat: true
        onTriggered: {
            // Force re-evaluation by toggling a dummy property.
            root.arrivalTimes = root.arrivalTimes;
        }
    }

    // =================================================================
    // LAYOUT CONSTANTS
    // =================================================================
    readonly property int headerHeight: 40
    readonly property int entryMinHeight: 64
    readonly property int sectionHeaderHeight: 22

    // =================================================================
    // CARD DIMENSIONS & CHROME (§12.3)
    // =================================================================
    readonly property int maxCardHeight: Math.min(
        520,
        Math.floor(Screen.height * 0.5)
    )

    implicitWidth: 380
    implicitHeight: {
        const content = headerHeight + Theme.spaceMd
                      + displayedNotifs.length * entryMinHeight
                      + sectionCount * sectionHeaderHeight
                      + 2 * Theme.panelInnerPadding;
        return Math.min(maxCardHeight,
                        Math.max(headerHeight + 2 * Theme.panelInnerPadding + 60,
                                 content));
    }

    color: Qt.rgba(Theme.surface.r, Theme.surface.g, Theme.surface.b,
                   Theme.onBattery ? Theme.barOpacityBattery
                                   : Theme.barOpacity)
    Behavior on color {
        ColorAnimation { duration: Theme.motionNormal }
    }
    radius: Theme.panelCornerRadius
    border.width: Theme.panelBorderWidth
    border.color: Theme.outline
    antialiasing: true

    layer.enabled: true
    layer.effect: MultiEffect {
        shadowEnabled:        true
        shadowColor:          "#000000"
        blurMax:              Theme.panelShadowBlur
        shadowBlur:           1.0
        shadowVerticalOffset: Theme.panelShadowOffsetY
        shadowOpacity:        Theme.panelShadowOpacity
        autoPaddingEnabled:   true
    }

    // =================================================================
    // OPEN/CLOSE ANIMATION
    // =================================================================
    opacity: 0.0
    scale: 0.96
    transformOrigin: Item.TopRight

    states: [
        State {
            name: "open"
            when: root.isOpen
            PropertyChanges { target: root; opacity: 1.0; scale: 1.0 }
        }
    ]

    transitions: [
        Transition {
            from: ""; to: "open"
            ParallelAnimation {
                NumberAnimation {
                    property: "opacity"
                    duration: Theme.motionFast
                }
                SpringAnimation {
                    property: "scale"
                    spring:  Theme.springDefault
                    damping: Theme.springDefaultDamping
                    mass:    Theme.springMass
                    epsilon: 0.005
                }
            }
        },
        Transition {
            from: "open"; to: ""
            ParallelAnimation {
                NumberAnimation {
                    property: "opacity"
                    duration: Theme.motionFast
                }
                SpringAnimation {
                    property: "scale"
                    spring:  Theme.springSnappy
                    damping: Theme.springSnappyDamping
                    mass:    Theme.springMass
                    epsilon: 0.005
                }
            }
        }
    ]

    // Prevent clicks from falling through to the dismiss MouseArea.
    MouseArea {
        anchors.fill: parent
        onClicked: (event) => event.accepted = true
    }

    // =================================================================
    // CONTENT
    // =================================================================
    Column {
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceMd

        // -------------------------------------------------------------
        // Header: title + DnD toggle
        // -------------------------------------------------------------
        Row {
            width: parent.width
            height: root.headerHeight

            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "Notifications"
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeTitle
                font.weight: Theme.typeTitleWeight
            }

            Item { width: 1; Layout.fillWidth: true; height: 1 }

            // DnD toggle — right-aligned pill button.
            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                anchors.right: parent.right
                width: dndLabel.implicitWidth + 2 * Theme.spaceMd + dndIcon.implicitWidth + Theme.spaceSm
                height: 28
                radius: 14
                color: root.doNotDisturb ? Theme.primary : Theme.surfaceRaised
                border.width: root.doNotDisturb ? 0 : 1
                border.color: Theme.outline

                Behavior on color {
                    ColorAnimation { duration: Theme.motionFast }
                }

                Row {
                    anchors.centerIn: parent
                    spacing: Theme.spaceSm

                    Text {
                        id: dndIcon
                        anchors.verticalCenter: parent.verticalCenter
                        text: Theme.iconBellSlash
                        color: root.doNotDisturb ? Theme.textOnPrimary : Theme.fgSubtle
                        font.family: Theme.fontIcon
                        font.pixelSize: 14
                    }

                    Text {
                        id: dndLabel
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.doNotDisturb ? "On" : "Off"
                        color: root.doNotDisturb ? Theme.textOnPrimary : Theme.fgSubtle
                        font.family: Theme.fontMono
                        font.pixelSize: Theme.typeCaption
                        font.weight: Theme.typeCaptionWeight
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.dndToggled()
                }
            }
        }

        // Thin divider below header.
        Rectangle {
            width: parent.width
            height: 1
            color: Theme.outline
            opacity: 0.5
        }

        // -------------------------------------------------------------
        // Notification list
        // -------------------------------------------------------------
        ListView {
            id: notifList
            width: parent.width
            height: parent.height - root.headerHeight - Theme.spaceMd - 1
            clip: true
            model: root.displayedNotifs
            spacing: Theme.spaceXs
            boundsBehavior: Flickable.StopAtBounds

            section.property: "appName"
            section.criteria: ViewSection.FullString
            section.delegate: Item {
                required property string section
                width: ListView.view ? ListView.view.width : 0
                height: root.sectionHeaderHeight

                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: Theme.spaceSm
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: Theme.spaceXs
                    text: section
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeLabel
                    font.weight: Theme.typeLabelWeight
                    font.capitalization: Font.AllUppercase
                    font.letterSpacing: 0.8
                }
            }

            delegate: Rectangle {
                id: entry
                required property var modelData
                required property int index
                width: notifList.width
                height: Math.max(root.entryMinHeight, entryContent.implicitHeight + 2 * Theme.spaceSm)
                radius: Theme.panelCornerRadius
                color: "transparent"

                readonly property bool isCritical:
                    modelData.urgency === 2

                // Hover highlight.
                Rectangle {
                    anchors.fill: parent
                    radius: parent.radius
                    color: Theme.surfaceRaised
                    visible: entryHover.containsMouse
                }

                MouseArea {
                    id: entryHover
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.NoButton
                }

                Row {
                    id: entryContent
                    anchors.fill: parent
                    anchors.margins: Theme.spaceSm
                    spacing: Theme.spaceMd

                    // App icon or Phosphor fallback.
                    Item {
                        width: 28
                        height: 28
                        anchors.top: parent.top

                        Image {
                            anchors.fill: parent
                            visible: status === Image.Ready
                            source: {
                                const icon = entry.modelData.appIcon;
                                if (!icon || icon.length === 0) return "";
                                if (icon.startsWith("/")) return "file://" + icon;
                                return "";
                            }
                            sourceSize.width:  28
                            sourceSize.height: 28
                            fillMode: Image.PreserveAspectFit
                            smooth: true
                            asynchronous: true
                        }

                        Text {
                            anchors.centerIn: parent
                            visible: parent.children[0].status !== Image.Ready
                            text: Theme.iconAppWindow
                            color: Theme.primary
                            font.family: Theme.fontIcon
                            font.pixelSize: 20
                        }
                    }

                    // Text column: summary, body, timestamp, actions.
                    Column {
                        width: parent.width - 28 - Theme.spaceMd - dismissBtn.width - Theme.spaceSm
                        spacing: 2

                        // Summary + timestamp row.
                        Row {
                            width: parent.width
                            spacing: Theme.spaceSm

                            Text {
                                width: parent.width - tsText.implicitWidth - Theme.spaceSm
                                text: entry.modelData.summary
                                color: entry.isCritical ? Theme.error : Theme.fg
                                font.family: Theme.fontText
                                font.pixelSize: Theme.typeBody
                                font.weight: Theme.typeBodyEmphasisWeight
                                elide: Text.ElideRight
                            }

                            Text {
                                id: tsText
                                anchors.baseline: parent.children[0].baseline
                                text: root.relativeTime(entry.modelData.nId)
                                color: Theme.fgMuted
                                font.family: Theme.fontMono
                                font.pixelSize: Theme.typeCaption
                                font.weight: Theme.typeCaptionWeight
                            }
                        }

                        // Body.
                        Text {
                            width: parent.width
                            text: entry.modelData.body
                            color: Theme.fgSubtle
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeBody
                            wrapMode: Text.WordWrap
                            maximumLineCount: 3
                            elide: Text.ElideRight
                            visible: text.length > 0
                        }

                        // Action buttons.
                        Row {
                            spacing: Theme.spaceSm
                            visible: entry.modelData.actions.length > 0

                            Repeater {
                                model: entry.modelData.actions
                                delegate: Rectangle {
                                    required property var modelData
                                    width: actionLabel.implicitWidth + 2 * Theme.spaceMd
                                    height: 24
                                    radius: 4
                                    color: Theme.surfaceRaised
                                    border.width: 1
                                    border.color: Theme.outline

                                    Text {
                                        id: actionLabel
                                        anchors.centerIn: parent
                                        text: modelData.text || ""
                                        color: Theme.fg
                                        font.family: Theme.fontText
                                        font.pixelSize: Theme.typeCaption
                                        font.weight: Theme.typeCaptionWeight
                                    }

                                    MouseArea {
                                        anchors.fill: parent
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: modelData.invoke()
                                    }
                                }
                            }
                        }

                        // Inline reply.
                        Row {
                            width: parent.width
                            spacing: Theme.spaceSm
                            visible: entry.modelData.hasInlineReply

                            Rectangle {
                                width: parent.width - sendBtn.width - Theme.spaceSm
                                height: 28
                                radius: 4
                                color: Theme.surfaceRaised
                                border.width: 1
                                border.color: replyInput.activeFocus ? Theme.primary : Theme.outline

                                TextInput {
                                    id: replyInput
                                    anchors.fill: parent
                                    anchors.margins: Theme.spaceSm
                                    color: Theme.fg
                                    font.family: Theme.fontText
                                    font.pixelSize: Theme.typeCaption
                                    clip: true

                                    Text {
                                        anchors.verticalCenter: parent.verticalCenter
                                        visible: !replyInput.text && !replyInput.activeFocus
                                        text: entry.modelData.inlineReplyPlaceholder
                                        color: Theme.fgMuted
                                        font: replyInput.font
                                    }
                                }
                            }

                            Rectangle {
                                id: sendBtn
                                width: 28
                                height: 28
                                radius: 4
                                color: Theme.primary

                                Text {
                                    anchors.centerIn: parent
                                    text: "\u2192"
                                    color: Theme.textOnPrimary
                                    font.pixelSize: 14
                                }

                                MouseArea {
                                    anchors.fill: parent
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        if (replyInput.text.length > 0) {
                                            entry.modelData.notification.sendInlineReply(replyInput.text);
                                            replyInput.text = "";
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Dismiss button (top-right of entry).
                    Text {
                        id: dismissBtn
                        anchors.top: parent.top
                        text: Theme.iconX
                        color: entryHover.containsMouse ? Theme.fg : "transparent"
                        font.family: Theme.fontIcon
                        font.pixelSize: 14

                        Behavior on color {
                            ColorAnimation { duration: Theme.motionFast }
                        }

                        MouseArea {
                            anchors.fill: parent
                            anchors.margins: -Theme.spaceXs
                            cursorShape: Qt.PointingHandCursor
                            onClicked: entry.modelData.notification.dismiss()
                        }
                    }
                }
            }

            // Empty state.
            Text {
                anchors.centerIn: parent
                visible: notifList.count === 0
                text: "no notifications"
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.italic: true
            }
        }
    }
}
