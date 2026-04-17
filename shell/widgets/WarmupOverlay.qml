// WarmupOverlay — spec §2.12.1.
//
// Centered card shown on session start (first activity after a ≥4h gap,
// or on `levshell-ctl warmup open`). Three sections:
//
//   1. Today — calendar events from the unified data store (synced from
//      CalDAV in Phase 2).
//   2. Due — Anki flashcard count pulled from the unified data store.
//   3. Projects — active projects (status != complete), newest-active
//      first. `idle_secs` is session-scoped so freshly-started daemons
//      show "—".
//
// Dismissed by Escape or click-outside. No auto-timeout — the spec
// frames warmup as a deliberate ramp-up, not an ambient notification.

import QtQuick
import QtQuick.Effects
import ".."

Rectangle {
    id: root

    property bool isOpen: false
    property var payload: ({ fired_at: "", events: [], anki_due_count: 0, projects: [] })

    signal dismissed()

    implicitWidth: 520
    implicitHeight: Math.min(
        header.implicitHeight + eventsSection.implicitHeight
        + ankiSection.implicitHeight + projectsSection.implicitHeight
        + dismissRow.implicitHeight + 5 * Theme.spaceLg,
        720)

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

    // Intercept clicks to the panel so they don't reach the overlay's
    // click-outside-to-dismiss MouseArea.
    MouseArea { anchors.fill: parent; onClicked: (e) => e.accepted = true }

    function greeting() {
        const h = new Date().getHours();
        if (h < 4)  return "Welcome back";
        if (h < 12) return "Good morning";
        if (h < 18) return "Good afternoon";
        return "Good evening";
    }

    function statusLabel(s) {
        switch (s) {
        case "active":     return "active";
        case "simmering":  return "simmering";
        case "blocked":    return "blocked";
        case "writing_up": return "writing up";
        default:           return s;
        }
    }

    function statusColor(s) {
        switch (s) {
        case "active":     return Theme.success;
        case "blocked":    return Theme.warning;
        case "writing_up": return Theme.primary;
        default:           return Theme.fgSubtle;
        }
    }

    function formatIdle(secs) {
        if (!secs || secs <= 0) return "—";
        if (secs < 60)   return "just now";
        if (secs < 3600) return Math.round(secs / 60) + "m idle";
        if (secs < 86400) {
            const h = Math.floor(secs / 3600);
            const m = Math.round((secs % 3600) / 60);
            return m > 0 ? h + "h " + m + "m idle" : h + "h idle";
        }
        return Math.floor(secs / 86400) + "d idle";
    }

    function formatTime(rfc3339) {
        if (!rfc3339) return "";
        const d = new Date(rfc3339);
        if (isNaN(d.getTime())) return "";
        const h = String(d.getHours()).padStart(2, "0");
        const m = String(d.getMinutes()).padStart(2, "0");
        return h + ":" + m;
    }

    Column {
        anchors.fill: parent
        anchors.margins: Theme.panelInnerPadding
        spacing: Theme.spaceLg

        Column {
            id: header
            width: parent.width
            spacing: Theme.spaceXs

            Text {
                text: root.greeting()
                color: Theme.fg
                font.family: Theme.fontText
                font.pixelSize: Theme.typeTitle
                font.weight: Theme.typeTitleWeight
            }
            Text {
                text: "Here's where you left off."
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeBody
                font.weight: Theme.typeBodyWeight
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Today's agenda.
        Column {
            id: eventsSection
            width: parent.width
            spacing: Theme.spaceSm

            Text {
                text: "TODAY"
                color: Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.weight: Theme.typeCaptionWeight
                font.letterSpacing: 1.5
            }

            Text {
                visible: root.payload.events.length === 0
                text: "Nothing scheduled."
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeBody
                font.weight: Theme.typeBodyWeight
                font.italic: true
            }

            Repeater {
                model: root.payload.events
                delegate: Row {
                    required property var modelData
                    width: eventsSection.width
                    spacing: Theme.spaceMd

                    Text {
                        width: 60
                        text: root.formatTime(modelData.start_at)
                        color: Theme.fg
                        font.family: Theme.fontMono
                        font.pixelSize: Theme.typeBody
                        font.weight: Theme.typeBodyEmphasisWeight
                        font.features: ({ "tnum": 1 })
                    }
                    Column {
                        width: parent.width - 60 - Theme.spaceMd
                        spacing: 2
                        Text {
                            width: parent.width
                            text: modelData.title
                            color: Theme.fg
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeBody
                            font.weight: Theme.typeBodyEmphasisWeight
                            elide: Text.ElideRight
                        }
                        Text {
                            width: parent.width
                            visible: modelData.location && modelData.location.length > 0
                            text: modelData.location || ""
                            color: Theme.fgSubtle
                            font.family: Theme.fontText
                            font.pixelSize: Theme.typeCaption
                            font.weight: Theme.typeCaptionWeight
                            elide: Text.ElideRight
                        }
                    }
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Anki dues.
        Row {
            id: ankiSection
            width: parent.width
            spacing: Theme.spaceMd

            Column {
                width: 60
                spacing: 0
                Text {
                    text: "DUE"
                    color: Theme.fgSubtle
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeCaption
                    font.weight: Theme.typeCaptionWeight
                    font.letterSpacing: 1.5
                }
            }
            Row {
                spacing: Theme.spaceSm
                anchors.verticalCenter: parent.verticalCenter

                Text {
                    text: root.payload.anki_due_count
                    color: root.payload.anki_due_count > 0 ? Theme.primary : Theme.fgMuted
                    font.family: Theme.fontMono
                    font.pixelSize: Theme.typeTitle
                    font.weight: Theme.typeTitleWeight
                    font.features: ({ "tnum": 1 })
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.payload.anki_due_count === 1
                        ? "flashcard due"
                        : "flashcards due"
                    color: Theme.fgMuted
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeBody
                    font.weight: Theme.typeBodyWeight
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: Theme.outline; opacity: 0.5 }

        // Projects.
        Column {
            id: projectsSection
            width: parent.width
            spacing: Theme.spaceSm

            Text {
                text: "PROJECTS"
                color: Theme.fgSubtle
                font.family: Theme.fontText
                font.pixelSize: Theme.typeCaption
                font.weight: Theme.typeCaptionWeight
                font.letterSpacing: 1.5
            }

            Text {
                visible: root.payload.projects.length === 0
                text: "No active projects."
                color: Theme.fgMuted
                font.family: Theme.fontText
                font.pixelSize: Theme.typeBody
                font.weight: Theme.typeBodyWeight
                font.italic: true
            }

            Repeater {
                model: root.payload.projects
                delegate: Row {
                    required property var modelData
                    width: projectsSection.width
                    spacing: Theme.spaceMd

                    Rectangle {
                        width: 6; height: 6; radius: 3
                        anchors.verticalCenter: parent.verticalCenter
                        color: root.statusColor(modelData.status)
                    }
                    Text {
                        width: Math.min(260, projectsSection.width * 0.5)
                        text: modelData.name
                        color: Theme.fg
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeBody
                        font.weight: Theme.typeBodyEmphasisWeight
                        elide: Text.ElideRight
                    }
                    Text {
                        text: root.statusLabel(modelData.status)
                        color: root.statusColor(modelData.status)
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                        font.weight: Theme.typeCaptionWeight
                    }
                    Item {
                        width: parent.width - 6 - Math.min(260, projectsSection.width * 0.5)
                            - 3 * Theme.spaceMd - idleLabel.implicitWidth
                        height: 1
                    }
                    Text {
                        id: idleLabel
                        text: root.formatIdle(modelData.idle_secs)
                        color: Theme.fgSubtle
                        font.family: Theme.fontText
                        font.pixelSize: Theme.typeCaption
                        font.weight: Theme.typeCaptionWeight
                    }
                }
            }
        }

        Item { width: parent.width; height: 1 } // spacer

        Row {
            id: dismissRow
            anchors.right: parent.right
            spacing: Theme.spaceMd

            Rectangle {
                width: dismissText.implicitWidth + 2 * Theme.spaceMd
                height: 32
                radius: Theme.panelCornerRadius
                color: Theme.primary
                Text {
                    id: dismissText
                    anchors.centerIn: parent
                    text: "Get to work"
                    color: Theme.textOnPrimary
                    font.family: Theme.fontText
                    font.pixelSize: Theme.typeLabel
                    font.weight: Theme.typeLabelWeight
                }
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.dismissed()
                }
            }
        }
    }

    // Escape dismisses.
    focus: isOpen
    Keys.onEscapePressed: root.dismissed()
}
