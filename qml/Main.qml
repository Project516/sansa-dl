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

    function duration(ms) {
        const seconds = Math.floor(ms / 1000);
        return Math.floor(seconds / 60) + ":" + ("0" + seconds % 60).slice(-2);
    }

    Controls.Dialog {
        id: musicDialog
        parent: Controls.Overlay.overlay
        anchors.centerIn: parent
        width: Math.min(root.width - Kirigami.Units.gridUnit * 2, Kirigami.Units.gridUnit * 32)
        height: Math.min(root.height - Kirigami.Units.gridUnit * 2, Kirigami.Units.gridUnit * 26)
        modal: true
        title: "Find music"
        standardButtons: Controls.Dialog.Close
        property bool searched: false

        function search() {
            searched = musicQuery.text.trim() !== "";
            backend.searchMusic(musicQuery.text);
        }

        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            RowLayout {
                Layout.fillWidth: true
                Controls.TextField {
                    id: musicQuery
                    Layout.fillWidth: true
                    placeholderText: "Artist or song"
                    Accessible.name: "Artist or song"
                    onAccepted: musicDialog.search()
                }
                Controls.Button {
                    text: "Search"
                    icon.name: "search"
                    enabled: musicQuery.text.trim() !== ""
                    onClicked: musicDialog.search()
                }
            }
            RowLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Layout.fillWidth: true
                    text: backend.soulseekStatus === "" ? "Not connected to Soulseek" : backend.soulseekStatus
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                }
                Controls.Button {
                    text: backend.soulseekReady ? "Connected" : "Connect"
                    enabled: !backend.soulseekReady
                    onClicked: backend.soulseekConnect()
                }
            }
            Kirigami.InlineMessage {
                Layout.fillWidth: true
                visible: backend.searchError !== ""
                text: backend.searchError
                type: Kirigami.MessageType.Error
            }
            Controls.BusyIndicator {
                Layout.alignment: Qt.AlignHCenter
                running: backend.searching
                visible: running
            }
            ListView {
                id: musicResults
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                currentIndex: -1
                model: backend.searchResults === "" ? [] : JSON.parse(backend.searchResults)
                delegate: Controls.ItemDelegate {
                    required property var modelData
                    required property int index
                    width: ListView.view.width
                    highlighted: ListView.isCurrentItem
                    onClicked: musicResults.currentIndex = index
                    contentItem: ColumnLayout {
                        Controls.Label {
                            Layout.fillWidth: true
                            text: modelData.title
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                        }
                        Controls.Label {
                            Layout.fillWidth: true
                            text: modelData.artist + (modelData.release ? " · " + modelData.release : "") + (modelData.length ? " · " + root.duration(modelData.length) : "")
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
                Kirigami.PlaceholderMessage {
                    anchors.centerIn: parent
                    width: parent.width
                    visible: !backend.searching && backend.searchError === "" && musicResults.count === 0
                    text: musicDialog.searched ? "No recordings found" : "Search MusicBrainz for a song"
                }
            }
            Controls.Button {
                Layout.alignment: Qt.AlignRight
                text: "Find downloads"
                enabled: backend.soulseekReady && musicResults.currentIndex >= 0 && !backend.searching
                onClicked: {
                    const selected = musicResults.model[musicResults.currentIndex];
                    peerDialog.trackTitle = selected.artist + " · " + selected.title;
                    backend.findPeers(musicResults.currentIndex);
                    peerDialog.open();
                }
            }
        }
        Connections {
            target: backend
            function onSearchResultsChanged() { musicResults.currentIndex = -1; }
        }
        onOpened: {
            musicQuery.forceActiveFocus();
            backend.soulseekConnect();
        }
    }

    Controls.Dialog {
        id: peerDialog
        parent: Controls.Overlay.overlay
        anchors.centerIn: parent
        width: Math.min(root.width - Kirigami.Units.gridUnit * 2, Kirigami.Units.gridUnit * 32)
        height: Math.min(root.height - Kirigami.Units.gridUnit * 2, Kirigami.Units.gridUnit * 26)
        modal: true
        title: "Choose a download"
        standardButtons: Controls.Dialog.Close
        property string trackTitle: ""

        contentItem: ColumnLayout {
            Controls.Label {
                Layout.fillWidth: true
                text: peerDialog.trackTitle
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
            Kirigami.InlineMessage {
                Layout.fillWidth: true
                visible: backend.peerError !== ""
                text: backend.peerError
                type: Kirigami.MessageType.Error
            }
            Controls.BusyIndicator {
                Layout.alignment: Qt.AlignHCenter
                running: backend.peerSearching
                visible: running
            }
            ListView {
                id: peerResults
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                currentIndex: -1
                model: backend.peerResults === "" ? [] : JSON.parse(backend.peerResults)
                delegate: Controls.ItemDelegate {
                    required property var modelData
                    required property int index
                    width: ListView.view.width
                    highlighted: ListView.isCurrentItem
                    onClicked: peerResults.currentIndex = index
                    contentItem: ColumnLayout {
                        Controls.Label {
                            Layout.fillWidth: true
                            text: modelData.filename
                            textFormat: Text.PlainText
                            elide: Text.ElideLeft
                        }
                        Controls.Label {
                            Layout.fillWidth: true
                            text: modelData.username + " · " + (modelData.size / 1000000).toFixed(1) + " MB" + (modelData.bitrate ? " · " + modelData.bitrate + " kbps" : "") + (modelData.length ? " · " + root.duration(modelData.length * 1000) : "")
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
                Kirigami.PlaceholderMessage {
                    anchors.centerIn: parent
                    width: parent.width
                    visible: !backend.peerSearching && backend.peerError === "" && peerResults.count === 0
                    text: "No downloads found"
                }
            }
            Controls.Button {
                Layout.alignment: Qt.AlignRight
                text: "Download"
                enabled: !backend.peerSearching && peerResults.currentIndex >= 0
                onClicked: {
                    backend.downloadPeer(peerResults.currentIndex);
                    peerDialog.close();
                    musicDialog.close();
                }
            }
        }
        Connections {
            target: backend
            function onPeerResultsChanged() { peerResults.currentIndex = -1; }
        }
    }

    pageStack.initialPage: Kirigami.ScrollablePage {
        id: page
        title: "Downloads"

        actions: [
            Kirigami.Action {
                text: "Find music"
                icon.name: "search"
                onTriggered: musicDialog.open()
            },
            // Always shown as a switch, so it never becomes an overflow menu item.
            Kirigami.Action {
                displayHint: Kirigami.DisplayHint.KeepVisible
                displayComponent: Controls.Switch {
                    text: "Remove sponsor segments"
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
