import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Controls.ItemDelegate {
    id: delegate

    required property int index
    required property var jobId
    required property string title
    required property string status
    required property string stage
    required property real progress
    required property bool canCancel
    required property bool canRetry

    signal cancelRequested(var id)
    signal retryRequested(var id)

    hoverEnabled: false
    down: false
    highlighted: false
    Accessible.name: title + ", " + status

    contentItem: RowLayout {
        spacing: Kirigami.Units.largeSpacing

        ColumnLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            Controls.Label {
                Layout.fillWidth: true
                text: delegate.title
                elide: Text.ElideRight
                maximumLineCount: 1
            }

            Controls.Label {
                Layout.fillWidth: true
                text: delegate.status
                elide: Text.ElideRight
                maximumLineCount: 1
                font: Kirigami.Theme.smallFont
                color: delegate.stage === "failed" ? Kirigami.Theme.negativeTextColor : delegate.stage === "done" ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.disabledTextColor
            }

            Controls.ProgressBar {
                Layout.fillWidth: true
                visible: delegate.stage === "finding" || delegate.stage === "downloading" || delegate.stage === "fixing" || delegate.stage === "copying"
                from: 0
                to: 1
                indeterminate: delegate.progress < 0
                value: Math.max(0, delegate.progress)
            }
        }

        Controls.ToolButton {
            visible: delegate.canCancel
            icon.name: "dialog-cancel"
            text: "Cancel"
            display: Controls.AbstractButton.IconOnly
            Controls.ToolTip.text: text
            Controls.ToolTip.visible: hovered
            Controls.ToolTip.delay: Kirigami.Units.toolTipDelay
            onClicked: delegate.cancelRequested(delegate.jobId)
        }

        Controls.ToolButton {
            visible: delegate.canRetry
            icon.name: "view-refresh"
            text: "Retry"
            display: Controls.AbstractButton.IconOnly
            Controls.ToolTip.text: text
            Controls.ToolTip.visible: hovered
            Controls.ToolTip.delay: Kirigami.Units.toolTipDelay
            onClicked: delegate.retryRequested(delegate.jobId)
        }
    }
}
