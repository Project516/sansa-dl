import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import dev.project516.sansadl

Kirigami.ApplicationWindow {
    id: root

    title: "Sansa DL"
    width: Kirigami.Units.gridUnit * 36
    height: Kirigami.Units.gridUnit * 30
    minimumWidth: Kirigami.Units.gridUnit * 20
    minimumHeight: Kirigami.Units.gridUnit * 18

    Backend {
        id: backend
        onRaiseRequested: {
            root.show();
            root.raise();
            root.requestActivate();
        }
    }

    Connections {
        target: Qt.application
        function onAboutToQuit() {
            backend.shutdown();
        }
    }

    function addFromField() {
        backend.addLinks(field.text);
        field.clear();
        field.forceActiveFocus();
    }

    pageStack.initialPage: Kirigami.ScrollablePage {
        id: page
        title: "Downloads"

        actions: [
            Kirigami.Action {
                id: sponsorAction
                text: "Remove sponsor segments"
                checkable: true
                checked: backend.sponsorBlock
                onToggled: backend.setSponsorBlockEnabled(checked)
                // A switch shows on and off plainly; the overflow menu still uses the check item.
                displayComponent: Controls.Switch {
                    text: sponsorAction.text
                    checked: backend.sponsorBlock
                    onToggled: backend.setSponsorBlockEnabled(checked)
                }
            },
            Kirigami.Action {
                text: "Clear finished"
                icon.name: "edit-clear-all"
                enabled: backend.hasFinished
                onTriggered: backend.clearFinished()
            }
        ]

        header: Controls.Control {
            padding: Kirigami.Units.largeSpacing
            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing

                    Controls.TextField {
                        id: field
                        Layout.fillWidth: true
                        placeholderText: "Paste links"
                        inputMethodHints: Qt.ImhUrlCharactersOnly
                        Accessible.name: "Paste links"
                        onAccepted: root.addFromField()
                        Component.onCompleted: forceActiveFocus()
                    }

                    Controls.Button {
                        text: "Add"
                        icon.name: "list-add"
                        onClicked: root.addFromField()
                    }
                }

                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: backend.cardKind !== "none"
                    showCloseButton: false
                    text: backend.cardText
                    type: {
                        switch (backend.cardKind) {
                        case "error":
                            return Kirigami.MessageType.Error;
                        case "connected":
                        case "safe":
                            return Kirigami.MessageType.Positive;
                        default:
                            return Kirigami.MessageType.Information;
                        }
                    }
                    actions: [
                        Kirigami.Action {
                            text: "Eject"
                            icon.name: "media-eject"
                            visible: backend.cardCanEject
                            onTriggered: backend.eject()
                        }
                    ]
                }

                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: backend.updatedVersion !== ""
                    showCloseButton: true
                    type: Kirigami.MessageType.Positive
                    text: "Updated to " + backend.updatedVersion + ". Restart to use it"
                    actions: [
                        Kirigami.Action {
                            text: "Restart"
                            icon.name: "view-refresh"
                            onTriggered: {
                                if (backend.restart())
                                    Qt.quit();
                            }
                        }
                    ]
                }

                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: backend.restartError !== ""
                    type: Kirigami.MessageType.Error
                    text: backend.restartError
                }

                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: backend.toolsNote !== ""
                    showCloseButton: false
                    type: Kirigami.MessageType.Information
                    text: backend.toolsNote
                }

                Controls.Label {
                    Layout.fillWidth: true
                    visible: backend.waiting > 0
                    text: backend.waiting === 1 ? "1 song waiting for the Sansa" : backend.waiting + " songs waiting for the Sansa"
                    color: Kirigami.Theme.disabledTextColor
                }
            }
        }

        ListView {
            id: list
            model: backend
            spacing: 0
            currentIndex: -1
            reuseItems: true
            delegate: JobDelegate {
                width: ListView.view.width
                onCancelRequested: id => backend.cancel(id)
                onRetryRequested: id => backend.retry(id)
            }

            Kirigami.PlaceholderMessage {
                anchors.centerIn: parent
                width: parent.width - Kirigami.Units.gridUnit * 4
                visible: list.count === 0
                icon.name: "folder-music"
                text: "Paste a link to get started"
                explanation: "Songs are saved as mp3 and copied to your Sansa when it is plugged in."
            }

            DropArea {
                parent: list.parent
                anchors.fill: list
                onDropped: drop => {
                    if (drop.hasText)
                        backend.addLinks(drop.text);
                    else if (drop.hasUrls)
                        backend.addLinks(drop.urls.join("\n"));
                }
            }
        }
    }
}
