mod bridge;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QQuickStyle, QString, QUrl};
use cxx_qt_lib_extras::QApplication;
use sansa_dl_core::instance::{self, Command, Instance, Role};
use sansa_dl_core::paths::APP_ID;
use sansa_dl_core::{launcher, queue, selfupdate};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, OnceLock};

pub static INITIAL_LINKS: OnceLock<Vec<String>> = OnceLock::new();
pub static INSTANCE: Mutex<Option<(Instance, Receiver<Command>)>> = Mutex::new(None);

const HELP: &str = "Usage: sansa-dl [LINK]...\n       sansa-dl --eject\n\nDownload audio from links and copy it to a Sansa Clip+.\nLinks go to the running window when there is one.\n--eject asks the running window to eject the card.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("sansa-dl {}", selfupdate::CURRENT);
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "--eject") {
        let sent = instance::eject_running();
        if let Err(e) = &sent {
            eprintln!("sansa-dl: no running window: {e}");
        }
        std::process::exit(i32::from(sent.is_err()));
    }
    let links = queue::parse_links(&args.join(" "));

    match instance::claim(&links) {
        Role::Forwarded => return,
        Role::Primary(held) => *INSTANCE.lock().expect("fresh lock") = held,
    }
    let _ = INITIAL_LINKS.set(links);

    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        std::thread::spawn(move || {
            if let Err(e) = launcher::install(appimage.as_ref(), &launcher::data_home()) {
                eprintln!("sansa-dl: launcher entry: {e}");
            }
        });
    }

    let mut app = QApplication::new();
    QGuiApplication::set_desktop_file_name(&QString::from(APP_ID));
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        QQuickStyle::set_style(&QString::from("org.kde.desktop"));
    }

    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/dev/project516/sansadl/qml/Main.qml",
        ));
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
