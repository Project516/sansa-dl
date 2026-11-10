use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("dev.project516.sansadl").qml_files(["qml/Main.qml", "qml/JobDelegate.qml"]),
    )
    .files(["src/bridge.rs"])
    .build();
}
