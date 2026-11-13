//! The Qt side: one QObject that is both the job list model and the app state.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, waiting)]
        #[qproperty(QString, card_kind)]
        #[qproperty(QString, card_text)]
        #[qproperty(bool, card_can_eject)]
        #[qproperty(bool, sponsor_block)]
        #[qproperty(QString, tools_note)]
        #[qproperty(QString, updated_version)]
        #[qproperty(QString, restart_error)]
        #[qproperty(bool, has_finished)]
        #[qproperty(QString, search_results)]
        #[qproperty(QString, search_error)]
        #[qproperty(bool, searching)]
        #[qproperty(bool, soulseek_ready)]
        #[qproperty(QString, soulseek_status)]
        #[qproperty(QString, peer_results)]
        #[qproperty(QString, peer_error)]
        #[qproperty(bool, peer_searching)]
        type Backend = super::BackendRust;

        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &Backend, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(self: &Backend, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &Backend) -> QHash_i32_QByteArray;

        #[inherit]
        #[qsignal]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut Backend>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        #[inherit]
        fn index(self: &Backend, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        /// # Safety
        /// Every call must be followed by `end_insert_rows`.
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(
            self: Pin<&mut Backend>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );

        /// # Safety
        /// Must follow `begin_insert_rows`.
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut Backend>);

        /// # Safety
        /// Every call must be followed by `end_reset_model`.
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut Backend>);

        /// # Safety
        /// Must follow `begin_reset_model`.
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut Backend>);

        #[qsignal]
        fn raise_requested(self: Pin<&mut Backend>);

        #[qinvokable]
        fn add_links(self: &Backend, text: &QString);

        #[qinvokable]
        fn search_music(self: Pin<&mut Backend>, query: &QString);

        #[qinvokable]
        fn soulseek_login(self: &Backend, username: &QString, password: &QString);

        #[qinvokable]
        fn find_peers(self: Pin<&mut Backend>, index: i32);

        #[qinvokable]
        fn download_peer(self: &Backend, index: i32);

        #[qinvokable]
        fn cancel(self: &Backend, id: i64);

        #[qinvokable]
        fn retry(self: &Backend, id: i64);

        #[qinvokable]
        fn clear_finished(self: &Backend);

        #[qinvokable]
        fn eject(self: &Backend);

        #[qinvokable]
        fn set_sponsor_block_enabled(self: Pin<&mut Backend>, on: bool);

        #[qinvokable]
        fn restart(self: Pin<&mut Backend>) -> bool;

        #[qinvokable]
        fn shutdown(self: &Backend);
    }

    impl cxx_qt::Threading for Backend {}
    impl cxx_qt::Initialize for Backend {}
}

use crate::{INITIAL_LINKS, INSTANCE};
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant, QVector,
};
use sansa_dl_core::engine::{self, Emit, Engine, JobView, UiEvent};
use sansa_dl_core::instance::{Command, Instance};
use sansa_dl_core::paths::Dirs;
use sansa_dl_core::queue::parse_links;
use std::sync::Arc;

const ROLE_BASE: i32 = 0x100;
const ROLES: [&str; 7] = [
    "jobId",
    "title",
    "status",
    "stage",
    "progress",
    "canCancel",
    "canRetry",
];

#[derive(Default)]
pub struct BackendRust {
    waiting: i32,
    card_kind: QString,
    card_text: QString,
    card_can_eject: bool,
    sponsor_block: bool,
    tools_note: QString,
    updated_version: QString,
    restart_error: QString,
    has_finished: bool,
    search_results: QString,
    search_error: QString,
    searching: bool,
    search_generation: u64,
    music_rows: Vec<sansa_dl_core::musicbrainz::Recording>,
    soulseek_ready: bool,
    soulseek_status: QString,
    peer_results: QString,
    peer_error: QString,
    peer_searching: bool,
    rows: Vec<JobView>,
    engine: Option<Engine>,
    instance: Option<Instance>,
}

impl cxx_qt::Initialize for qobject::Backend {
    fn initialize(mut self: Pin<&mut Self>) {
        let qt = self.qt_thread();
        let emit: Emit = {
            let qt = self.qt_thread();
            Arc::new(move |event| {
                let _ = qt.queue(move |backend| backend.apply(event));
            })
        };
        let appimage = std::env::var_os("APPIMAGE").map(Into::into);
        let engine = engine::start(Dirs::from_env(), appimage, emit);

        let links = INITIAL_LINKS.get().cloned().unwrap_or_default();
        if !links.is_empty() {
            engine.add_links(links);
        }
        if let Some((instance, opened)) = INSTANCE.lock().ok().and_then(|mut slot| slot.take()) {
            self.as_mut().rust_mut().instance = Some(instance);
            let engine = engine.clone();
            std::thread::spawn(move || {
                for command in opened {
                    match command {
                        Command::Open(urls) => {
                            if !urls.is_empty() {
                                engine.add_links(parse_links(&urls.join(" ")));
                            }
                            let _ = qt.queue(|backend| backend.raise_requested());
                        }
                        Command::Eject => engine.eject(),
                    }
                }
            });
        }
        self.as_mut().rust_mut().engine = Some(engine);
    }
}

impl qobject::Backend {
    fn apply(mut self: Pin<&mut Self>, event: UiEvent) {
        match event {
            UiEvent::Jobs(rows) => self.set_rows(rows),
            UiEvent::JobUpdate(row) => self.update_row(row),
            UiEvent::Waiting(n) => self.set_waiting(i32::try_from(n).unwrap_or(i32::MAX)),
            UiEvent::Card(card) => {
                self.as_mut().set_card_kind(QString::from(card.kind));
                self.as_mut().set_card_text(QString::from(&card.text));
                self.set_card_can_eject(card.can_eject);
            }
            UiEvent::SponsorBlock(on) => self.set_sponsor_block(on),
            UiEvent::Tools(text) => self.set_tools_note(QString::from(&text)),
            UiEvent::Updated(version) => {
                self.set_updated_version(QString::from(&format!("v{version}")));
            }
            UiEvent::SoulseekStatus { ready, text } => {
                self.as_mut().set_soulseek_ready(ready);
                self.set_soulseek_status(QString::from(&text));
            }
            UiEvent::PeerResults(rows) => {
                let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into());
                self.set_peer_results(QString::from(&json));
            }
            UiEvent::PeerError(error) => self.set_peer_error(QString::from(&error)),
            UiEvent::PeerSearching(searching) => self.set_peer_searching(searching),
        }
    }

    fn set_rows(mut self: Pin<&mut Self>, rows: Vec<JobView>) {
        let has_finished = rows
            .iter()
            .any(|r| matches!(r.stage, "done" | "failed" | "canceled"));
        let old_len = self.rust().rows.len();
        let appended = rows.len() >= old_len
            && self
                .rust()
                .rows
                .iter()
                .zip(&rows)
                .all(|(a, b)| a.id == b.id);
        let none = QModelIndex::default();
        if appended {
            let mut rows = rows;
            let tail = rows.split_off(old_len);
            let changed: Vec<usize> = (0..old_len)
                .filter(|i| self.rust().rows[*i] != rows[*i])
                .collect();
            self.as_mut().rust_mut().rows = rows;
            for i in changed {
                self.as_mut().row_changed(i);
            }
            if !tail.is_empty() {
                let last = (old_len + tail.len() - 1) as i32;
                // SAFETY: every begin is closed by the matching end below.
                unsafe {
                    self.as_mut().begin_insert_rows(&none, old_len as i32, last);
                    self.as_mut().rust_mut().rows.extend(tail);
                    self.as_mut().end_insert_rows();
                }
            }
        } else {
            unsafe {
                self.as_mut().begin_reset_model();
                self.as_mut().rust_mut().rows = rows;
                self.as_mut().end_reset_model();
            }
        }
        self.set_has_finished(has_finished);
    }

    fn update_row(mut self: Pin<&mut Self>, row: JobView) {
        let Some(i) = self.rust().rows.iter().position(|r| r.id == row.id) else {
            return;
        };
        if self.rust().rows[i] != row {
            self.as_mut().rust_mut().rows[i] = row;
            self.row_changed(i);
        }
    }

    fn row_changed(mut self: Pin<&mut Self>, i: usize) {
        let index = self.index(i as i32, 0, &QModelIndex::default());
        self.as_mut()
            .data_changed(&index, &index, &QVector::default());
    }

    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        i32::try_from(self.rust().rows.len()).unwrap_or(i32::MAX)
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = usize::try_from(index.row())
            .ok()
            .and_then(|i| self.rust().rows.get(i))
        else {
            return QVariant::default();
        };
        match role - ROLE_BASE {
            0 => QVariant::from(&(row.id as i64)),
            1 => QVariant::from(&QString::from(&row.title)),
            2 => QVariant::from(&QString::from(&row.status)),
            3 => QVariant::from(&QString::from(row.stage)),
            4 => QVariant::from(&row.progress),
            5 => QVariant::from(&row.can_cancel),
            6 => QVariant::from(&row.can_retry),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut names = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, name) in ROLES.iter().enumerate() {
            names.insert(ROLE_BASE + i as i32, QByteArray::from(*name));
        }
        names
    }

    fn engine(&self) -> Option<&Engine> {
        self.rust().engine.as_ref()
    }

    fn add_links(&self, text: &QString) {
        let links = parse_links(&text.to_string());
        if let (false, Some(engine)) = (links.is_empty(), self.engine()) {
            engine.add_links(links);
        }
    }

    fn search_music(mut self: Pin<&mut Self>, query: &QString) {
        let query = query.to_string();
        let generation = self.rust().search_generation.wrapping_add(1);
        self.as_mut().rust_mut().search_generation = generation;
        self.as_mut().set_search_error(QString::default());
        self.as_mut().set_search_results(QString::from("[]"));
        self.as_mut().rust_mut().music_rows.clear();
        if query.trim().is_empty() {
            self.set_searching(false);
            return;
        }
        self.as_mut().set_searching(true);
        let qt = self.qt_thread();
        std::thread::spawn(move || {
            let result = sansa_dl_core::musicbrainz::search(&query);
            let _ = qt.queue(move |mut backend| {
                if backend.rust().search_generation != generation {
                    return;
                }
                backend.as_mut().set_searching(false);
                match result {
                    Ok(rows) => {
                        let values: Vec<_> = rows
                            .iter()
                            .map(|r| {
                                serde_json::json!({
                                    "id": r.id, "title": r.title, "artist": r.artist,
                                    "release": r.release, "length": r.length_ms
                                })
                            })
                            .collect();
                        backend.as_mut().rust_mut().music_rows = rows;
                        backend.set_search_results(QString::from(
                            &serde_json::Value::Array(values).to_string(),
                        ));
                    }
                    Err(error) => backend.set_search_error(QString::from(&error)),
                }
            });
        });
    }

    fn soulseek_login(&self, username: &QString, password: &QString) {
        if let Some(engine) = self.engine() {
            engine.soulseek_login(username.to_string(), password.to_string());
        }
    }

    fn find_peers(mut self: Pin<&mut Self>, index: i32) {
        let recording = usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().music_rows.get(i))
            .cloned();
        if let (Some(recording), Some(engine)) = (recording, self.engine().cloned()) {
            self.as_mut().set_peer_error(QString::default());
            self.as_mut().set_peer_results(QString::from("[]"));
            self.as_mut().set_peer_searching(true);
            engine.search_peers(recording);
        }
    }

    fn download_peer(&self, index: i32) {
        if let (Some(engine), Ok(index)) = (self.engine(), usize::try_from(index)) {
            engine.download_peer(index);
        }
    }

    fn cancel(&self, id: i64) {
        if let (Some(engine), Ok(id)) = (self.engine(), u64::try_from(id)) {
            engine.cancel(id);
        }
    }

    fn retry(&self, id: i64) {
        if let (Some(engine), Ok(id)) = (self.engine(), u64::try_from(id)) {
            engine.retry(id);
        }
    }

    fn clear_finished(&self) {
        if let Some(engine) = self.engine() {
            engine.clear_finished();
        }
    }

    fn eject(&self) {
        if let Some(engine) = self.engine() {
            engine.eject();
        }
    }

    fn set_sponsor_block_enabled(mut self: Pin<&mut Self>, on: bool) {
        // Show the new value now; the engine confirms it with the same value.
        self.as_mut().set_sponsor_block(on);
        if let Some(engine) = self.engine() {
            engine.set_sponsorblock(on);
        }
    }

    /// Starts a fresh copy of the app. The caller quits this one.
    fn restart(mut self: Pin<&mut Self>) -> bool {
        let result = std::env::var_os("APPIMAGE")
            .ok_or_else(|| std::io::Error::other("AppImage path is unavailable"))
            .and_then(|appimage| {
                std::process::Command::new(appimage)
                    .arg("--restart")
                    .spawn()
            });
        match result {
            Ok(_) => {
                if let Some(instance) = &self.rust().instance {
                    instance.release();
                }
                true
            }
            Err(error) => {
                let message = format!("Could not restart: {error}");
                eprintln!("sansa-dl: {message}");
                self.as_mut().set_restart_error(QString::from(&message));
                false
            }
        }
    }

    fn shutdown(&self) {
        if let Some(engine) = self.engine() {
            engine.shutdown();
        }
    }
}
