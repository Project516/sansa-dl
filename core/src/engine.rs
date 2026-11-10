//! The coordinator. One thread owns all job state and reacts to messages from
//! the UI and from worker threads. Nothing polls.

use crate::card::{CardInfo, Udisks};
use crate::paths::{Config, Dirs, State, now_secs, read_json, read_json_checked, write_json};
use crate::proc::{self, Cancel};
use crate::queue::{self, Job, Stage};
use crate::tools::{self, Resolved, Staged, Tool};
use crate::ytdlp::{self, Found, Line};
use crate::{fixer, net, notify, selfupdate, transfer};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const MAX_FINDING: usize = 2;
const MAX_DOWNLOADS: usize = 3;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq)]
pub struct JobView {
    pub id: u64,
    pub title: String,
    pub status: String,
    pub stage: &'static str,
    pub progress: f64,
    pub can_cancel: bool,
    pub can_retry: bool,
}

impl From<&Job> for JobView {
    fn from(job: &Job) -> Self {
        Self {
            id: job.id,
            title: job.title.clone(),
            status: job.status(),
            stage: job.stage_name(),
            progress: job.progress(),
            can_cancel: job.can_cancel(),
            can_retry: job.can_retry(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    pub kind: &'static str,
    pub text: String,
    pub can_eject: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Jobs(Vec<JobView>),
    JobUpdate(JobView),
    Waiting(usize),
    Card(CardView),
    SponsorBlock(bool),
    Tools(String),
    Updated(String),
}

pub type Emit = Arc<dyn Fn(UiEvent) + Send + Sync>;

enum Msg {
    AddLinks(Vec<String>),
    Cancel(u64),
    Retry(u64),
    ClearFinished,
    Eject,
    SetSponsorBlock(bool),
    Discovered {
        id: u64,
        generation: u64,
        result: Result<Found, String>,
    },
    Progress {
        id: u64,
        generation: u64,
        fraction: Option<f64>,
        speed: Option<f64>,
        processing: bool,
    },
    Downloaded {
        id: u64,
        generation: u64,
        result: Result<PathBuf, String>,
    },
    Fixed {
        id: u64,
        generation: u64,
        result: Result<PathBuf, String>,
    },
    Udisks(Udisks),
    Card(Result<Option<CardInfo>, String>),
    Mounted(Result<PathBuf, String>),
    Free(Option<u64>),
    CopyStarted(u64),
    Copied(Option<u64>),
    CopyFailed {
        id: Option<u64>,
        reason: String,
    },
    TransferDone,
    Ejected(Result<(), String>),
    ToolStaged(Staged),
    AppUpdated(String),
    Shutdown(Sender<()>),
}

#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
}

impl Engine {
    pub fn add_links(&self, links: Vec<String>) {
        let _ = self.tx.send(Msg::AddLinks(links));
    }
    pub fn cancel(&self, id: u64) {
        let _ = self.tx.send(Msg::Cancel(id));
    }
    pub fn retry(&self, id: u64) {
        let _ = self.tx.send(Msg::Retry(id));
    }
    pub fn clear_finished(&self) {
        let _ = self.tx.send(Msg::ClearFinished);
    }
    pub fn eject(&self) {
        let _ = self.tx.send(Msg::Eject);
    }
    pub fn set_sponsorblock(&self, on: bool) {
        let _ = self.tx.send(Msg::SetSponsorBlock(on));
    }
    /// Stops running downloads and saves the queue. Waits briefly for that.
    pub fn shutdown(&self) {
        let (ack, done) = channel();
        if self.tx.send(Msg::Shutdown(ack)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CardNote {
    Safe,
    EjectError(String),
    MountError(String),
}

struct Core {
    dirs: Dirs,
    cfg: Config,
    emit: Emit,
    tx: Sender<Msg>,
    jobs: Vec<Job>,
    next_id: u64,
    dirty: bool,
    jobs_writable: bool,
    config_writable: bool,
    bundled: Option<PathBuf>,
    tools: Resolved,
    staged: Vec<Staged>,
    cancels: HashMap<u64, Arc<Cancel>>,
    udisks: Option<Udisks>,
    card: Option<CardInfo>,
    free: Option<u64>,
    note: Option<CardNote>,
    mounting: bool,
    ejecting: bool,
    ejected_block: Option<String>,
    eject_after_copy: bool,
    transferring: bool,
    transfer_failed: bool,
    stopping: bool,
    card_cleanup_pending: bool,
    stop_transfer: Arc<AtomicBool>,
    batch_copied: usize,
    last_waiting: Option<usize>,
    last_card: Option<CardView>,
    last_tools: String,
}

/// What the coordinator listens to besides the UI.
#[derive(Debug, Clone)]
pub struct Options {
    pub card: bool,
    pub updates: bool,
    pub bundled_tools: Option<PathBuf>,
}

/// Starts the coordinator and everything it listens to.
pub fn start(dirs: Dirs, appimage: Option<PathBuf>, emit: Emit) -> Engine {
    let options = Options {
        card: true,
        updates: true,
        bundled_tools: tools::bundled_dir(),
    };
    start_with(dirs, appimage, emit, options)
}

pub fn start_with(dirs: Dirs, appimage: Option<PathBuf>, emit: Emit, options: Options) -> Engine {
    let (tx, rx) = channel();
    let engine = Engine { tx: tx.clone() };
    let _ = dirs.create();
    tools::clean_stages(&dirs.tools);

    let bundled = options.bundled_tools.clone();
    let (saved_jobs, jobs_ok) = read_json_checked::<Vec<Job>>(&dirs.jobs_file);
    let mut jobs = queue::restore(saved_jobs);
    let next_id = jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    for job in &mut jobs {
        job.running = false;
    }
    if jobs_ok && !dirs.jobs_file.with_extension("json.bad").exists() {
        remove_stale_downloads(&dirs.downloads, &jobs);
    }

    let (cfg, config_ok) = read_json_checked(&dirs.config_file);
    let jobs_writable = jobs_ok || !dirs.jobs_file.exists();
    let config_writable = config_ok || !dirs.config_file.exists();
    let resolved = Resolved::load(&dirs.tools, bundled.as_deref());
    let core = Core {
        cfg,
        jobs_writable,
        config_writable,
        emit,
        tx: tx.clone(),
        jobs,
        next_id,
        dirty: true,
        bundled,
        tools: resolved.clone(),
        staged: Vec::new(),
        cancels: HashMap::new(),
        udisks: None,
        card: None,
        free: None,
        note: None,
        mounting: false,
        ejecting: false,
        ejected_block: None,
        eject_after_copy: false,
        transferring: false,
        transfer_failed: false,
        stopping: false,
        card_cleanup_pending: false,
        stop_transfer: Arc::new(AtomicBool::new(false)),
        batch_copied: 0,
        last_waiting: None,
        last_card: None,
        last_tools: String::new(),
        dirs: dirs.clone(),
    };

    if options.card {
        let (connected, changed) = (tx.clone(), tx.clone());
        Udisks::watch(
            move |udisks| {
                let _ = connected.send(Msg::Udisks(udisks));
            },
            move |card| {
                let _ = changed.send(Msg::Card(card));
            },
        );
    }
    if options.updates {
        spawn_updater(tx, dirs, resolved, appimage);
    }

    std::thread::Builder::new()
        .name("coordinator".into())
        .spawn(move || core.run(rx))
        .expect("spawn coordinator");
    engine
}

fn remove_stale_downloads(downloads: &Path, jobs: &[Job]) {
    let Ok(entries) = fs::read_dir(downloads) else {
        return;
    };
    for entry in entries.flatten() {
        let keep = jobs.iter().any(|j| {
            j.downloaded.as_ref().and_then(|p| p.parent()) == Some(entry.path().as_path())
        });
        if !keep {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn current_attempt(job: &Job, id: u64, generation: u64, stage: Stage) -> bool {
    job.id == id && job.generation == generation && job.stage == stage
}

fn clean_card_temps(tracks: &Path) {
    if let Ok(entries) = fs::read_dir(tracks) {
        for entry in entries.flatten() {
            if fixer::is_temp(&entry.path()) && entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn spawn<F: FnOnce() + Send + 'static>(name: &str, work: F) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(work);
}

pub fn format_free(bytes: u64) -> String {
    let (value, unit) = if bytes >= 1_000_000_000_000 {
        (bytes as f64 / 1e12, "TB")
    } else if bytes >= 1_000_000_000 {
        (bytes as f64 / 1e9, "GB")
    } else {
        (bytes as f64 / 1e6, "MB")
    };
    if value >= 100.0 || unit == "MB" {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn outbox_files(outbox: &Path) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(outbox)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && !fixer::is_temp(&e.path()))
        .map(|e| {
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, e.path())
        })
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

impl Core {
    fn run(mut self, rx: Receiver<Msg>) {
        self.refresh_all();
        self.schedule();
        for msg in rx {
            self.handle(msg);
            self.schedule();
            self.flush();
        }
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    fn show(&self, id: u64) {
        if let Some(job) = self.jobs.iter().find(|j| j.id == id) {
            (self.emit)(UiEvent::JobUpdate(job.into()));
        }
    }

    fn show_all(&mut self) {
        (self.emit)(UiEvent::Jobs(self.jobs.iter().map(JobView::from).collect()));
        self.dirty = true;
    }

    fn refresh_all(&mut self) {
        (self.emit)(UiEvent::SponsorBlock(self.cfg.sponsorblock));
        self.show_all();
        self.refresh_waiting();
        self.refresh_card();
        self.refresh_tools();
    }

    fn flush(&mut self) {
        if self.dirty && self.jobs_writable {
            self.dirty = false;
            let _ = write_json(&self.dirs.jobs_file, &self.jobs);
        }
    }

    fn refresh_waiting(&mut self) {
        let count = outbox_files(&self.dirs.outbox).len();
        if self.last_waiting != Some(count) {
            self.last_waiting = Some(count);
            (self.emit)(UiEvent::Waiting(count));
        }
        self.refresh_card();
    }

    fn refresh_tools(&mut self) {
        let missing = self.tools.missing();
        let text = if missing.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = missing.iter().map(|t| t.key()).collect();
            format!("Getting {}", names.join(", "))
        };
        if text != self.last_tools {
            self.last_tools = text.clone();
            (self.emit)(UiEvent::Tools(text));
        }
    }

    fn card_view(&self) -> CardView {
        let view = |kind, text: String, can_eject| CardView {
            kind,
            text,
            can_eject,
        };
        if self.ejecting {
            return view("busy", "Ejecting the Sansa".into(), false);
        }
        if let Some(note) = &self.note {
            match note {
                CardNote::Safe => return view("safe", "Safe to unplug the Sansa".into(), false),
                CardNote::EjectError(e) => {
                    return view(
                        "error",
                        format!("Could not eject: {e}"),
                        self.card.is_some(),
                    );
                }
                CardNote::MountError(e) => {
                    return view("error", format!("Could not mount the Sansa: {e}"), false);
                }
            }
        }
        let waiting = self.last_waiting.unwrap_or(0);
        match &self.card {
            Some(card) if self.ejected_block.as_ref() == Some(&card.block) => {
                view("safe", "Safe to unplug the Sansa".into(), false)
            }
            Some(_) if self.transferring => {
                view("copying", "Copying songs to the Sansa".into(), true)
            }
            Some(_) => {
                let text = match self.free {
                    Some(free) => format!("Sansa connected, {} free", format_free(free)),
                    None => "Sansa connected".into(),
                };
                view("connected", text, true)
            }
            None if waiting > 0 => view("plug", "Plug in your Sansa".into(), false),
            None => view("none", String::new(), false),
        }
    }

    fn refresh_card(&mut self) {
        let view = self.card_view();
        if self.last_card.as_ref() != Some(&view) {
            self.last_card = Some(view.clone());
            (self.emit)(UiEvent::Card(view));
        }
    }

    // Messages -----------------------------------------------------------

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::AddLinks(links) => {
                for url in links {
                    let job = Job::new(self.next_id, url);
                    self.next_id += 1;
                    self.jobs.push(job);
                }
                self.show_all();
            }
            Msg::Cancel(id) => self.cancel(id),
            Msg::Retry(id) => {
                if let Some(job) = self.job_mut(id).filter(|j| j.can_retry()) {
                    job.generation += 1;
                    job.stage = job.retry_stage();
                    job.reason.clear();
                    job.fraction = None;
                    job.speed = None;
                    job.processing = false;
                    self.transfer_failed = false;
                    self.show_all();
                    self.refresh_waiting();
                }
            }
            Msg::ClearFinished => {
                self.jobs.retain(|j| !j.is_finished() || j.running);
                self.show_all();
            }
            Msg::SetSponsorBlock(on) => {
                self.cfg.sponsorblock = on;
                if self.config_writable {
                    let _ = write_json(&self.dirs.config_file, &self.cfg);
                }
                (self.emit)(UiEvent::SponsorBlock(on));
            }
            Msg::Discovered {
                id,
                generation,
                result,
            } => {
                if self
                    .jobs
                    .iter()
                    .any(|j| j.id == id && j.generation == generation)
                {
                    self.discovered(id, result);
                }
            }
            Msg::Progress {
                id,
                generation,
                fraction,
                speed,
                processing,
            } => {
                if let Some(job) = self
                    .job_mut(id)
                    .filter(|j| j.stage == Stage::Downloading && j.generation == generation)
                {
                    job.fraction = fraction.or(job.fraction);
                    job.speed = speed;
                    job.processing = processing;
                    self.show(id);
                }
            }
            Msg::Downloaded {
                id,
                generation,
                result,
            } => {
                if self
                    .jobs
                    .iter()
                    .any(|j| current_attempt(j, id, generation, Stage::Downloading))
                {
                    self.downloaded(id, result);
                } else if let Ok(file) = result
                    && let Some(dir) = file.parent()
                {
                    let _ = fs::remove_dir_all(dir);
                }
            }
            Msg::Fixed {
                id,
                generation,
                result,
            } => {
                if self
                    .jobs
                    .iter()
                    .any(|j| current_attempt(j, id, generation, Stage::Fixing))
                {
                    self.fixed(id, result);
                } else {
                    if let Ok(file) = result {
                        let _ = fs::remove_file(file);
                    }
                    if let Some(job) = self.jobs.iter_mut().find(|j| {
                        j.id == id && j.generation == generation && j.stage == Stage::Canceled
                    }) {
                        job.running = false;
                        if let Some(file) = job.downloaded.take()
                            && let Some(dir) = file.parent()
                        {
                            let _ = fs::remove_dir_all(dir);
                        }
                    }
                    self.show_all();
                }
            }
            Msg::Udisks(udisks) => self.udisks = Some(udisks),
            Msg::Card(result) => self.on_card(result),
            Msg::Mounted(result) => {
                self.mounting = false;
                if let Err(e) = result {
                    self.note = Some(CardNote::MountError(e));
                }
                self.refresh_card();
            }
            Msg::Free(free) => {
                self.free = free;
                self.refresh_card();
            }
            Msg::CopyStarted(id) => {
                if let Some(job) = self.job_mut(id) {
                    job.stage = Stage::Copying;
                    self.show(id);
                }
            }
            Msg::Copied(id) => {
                self.batch_copied += 1;
                if let Some(job) = id.and_then(|id| self.job_mut(id)) {
                    job.stage = Stage::Copied;
                    job.outbox_file = None;
                }
                self.show_all();
                self.refresh_waiting();
            }
            Msg::CopyFailed { id, reason } => {
                self.transfer_failed = true;
                if let Some(job) = id.and_then(|id| self.job_mut(id)) {
                    job.stage = Stage::Failed;
                    job.reason = reason;
                }
                self.show_all();
            }
            Msg::TransferDone => self.transfer_done(),
            Msg::Ejected(result) => {
                self.ejecting = false;
                match result {
                    Ok(()) => {
                        self.note = Some(CardNote::Safe);
                        self.ejected_block = self.card.as_ref().map(|c| c.block.clone());
                    }
                    Err(e) => self.note = Some(CardNote::EjectError(e)),
                }
                self.refresh_card();
            }
            Msg::Eject => self.eject(),
            Msg::ToolStaged(staged) => self.staged.push(staged),
            Msg::AppUpdated(version) => (self.emit)(UiEvent::Updated(version)),
            Msg::Shutdown(ack) => {
                self.stopping = true;
                self.stop_transfer.store(true, Ordering::SeqCst);
                for cancel in self.cancels.values() {
                    cancel.cancel();
                }
                self.dirty = true;
                self.flush();
                let _ = ack.send(());
            }
        }
    }

    fn cancel(&mut self, id: u64) {
        let Some(job) = self
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .filter(|j| j.can_cancel())
        else {
            return;
        };
        let fixing = job.stage == Stage::Fixing && job.running;
        job.stage = Stage::Canceled;
        job.running = fixing;
        if !fixing {
            job.downloaded = None;
        }
        if let Some(cancel) = self.cancels.remove(&id) {
            cancel.cancel();
        }
        self.show_all();
    }

    fn discovered(&mut self, id: u64, result: Result<Found, String>) {
        self.cancels.remove(&id);
        let Some(pos) = self
            .jobs
            .iter()
            .position(|j| j.id == id && j.stage == Stage::Finding)
        else {
            return;
        };
        self.jobs[pos].running = false;
        match result {
            Err(reason) if self.stopping && reason == "Canceled" => {}
            Err(reason) => {
                self.jobs[pos].stage = Stage::Failed;
                self.jobs[pos].reason = reason;
            }
            Ok(Found::Video { title }) => {
                let job = &mut self.jobs[pos];
                job.title = title;
                job.found = true;
                job.stage = Stage::Waiting;
            }
            Ok(Found::Playlist { entries }) => {
                let playlist_url = self.jobs.remove(pos).url;
                for (offset, entry) in entries.into_iter().enumerate() {
                    let mut job = Job::new(self.next_id, entry.url.clone());
                    self.next_id += 1;
                    job.title = entry.title;
                    job.found = true;
                    job.stage = Stage::Waiting;
                    if entry.index.is_some() {
                        job.url = playlist_url.clone();
                        job.playlist_index = entry.index;
                    }
                    self.jobs.insert(pos + offset, job);
                }
            }
        }
        self.show_all();
    }

    fn downloaded(&mut self, id: u64, result: Result<PathBuf, String>) {
        self.cancels.remove(&id);
        let stopping = self.stopping;
        let Some(job) = self.job_mut(id).filter(|j| j.stage == Stage::Downloading) else {
            return;
        };
        job.running = false;
        job.processing = false;
        match result {
            Err(reason) if stopping && reason == "Canceled" => {}
            Ok(file) => {
                job.downloaded = Some(file);
                job.stage = Stage::Fixing;
            }
            Err(reason) => {
                job.stage = Stage::Failed;
                job.reason = reason;
            }
        }
        self.show_all();
    }

    fn fixed(&mut self, id: u64, result: Result<PathBuf, String>) {
        self.cancels.remove(&id);
        let Some(job) = self.job_mut(id).filter(|j| j.stage == Stage::Fixing) else {
            // Canceled while fixing: the result must not reach the card.
            if let Ok(file) = result {
                let _ = fs::remove_file(file);
            }
            return;
        };
        job.running = false;
        match result {
            Ok(outbox_file) => {
                job.stage = Stage::WaitingCard;
                job.outbox_file = Some(outbox_file);
                if let Some(file) = job.downloaded.take()
                    && let Some(dir) = file.parent()
                {
                    let _ = fs::remove_dir_all(dir);
                }
                self.note = None;
                self.transfer_failed = false;
            }
            Err(reason) => {
                job.stage = Stage::Failed;
                job.reason = reason;
            }
        }
        self.show_all();
        self.refresh_waiting();
    }

    // Card ---------------------------------------------------------------

    fn on_card(&mut self, result: Result<Option<CardInfo>, String>) {
        match result {
            Err(e) => {
                self.card = None;
                self.note = Some(CardNote::MountError(e));
            }
            Ok(None) => {
                if self.card.is_some() {
                    self.free = None;
                    self.mounting = false;
                    self.ejected_block = None;
                    if !matches!(self.note, Some(CardNote::Safe)) {
                        self.note = None;
                    }
                }
                self.card = None;
                self.ejected_block = None;
            }
            Ok(Some(info)) => {
                let appeared = self.card.as_ref().is_none_or(|c| c.block != info.block);
                let newly_mounted = info.mount.is_some()
                    && (appeared || self.card.as_ref().is_some_and(|c| c.mount.is_none()));
                if appeared && self.ejected_block.as_ref() != Some(&info.block) {
                    self.note = None;
                    self.free = None;
                    self.ejected_block = None;
                    self.transfer_failed = false;
                }
                if self.ejected_block.as_ref() == Some(&info.block) {
                    self.note = Some(CardNote::Safe);
                }
                if newly_mounted {
                    self.transfer_failed = false;
                    self.card_cleanup_pending = true;
                }
                self.card = Some(info.clone());
                let wanted = appeared || self.last_waiting.unwrap_or(0) > 0;
                let skip = self.mounting
                    || self.ejecting
                    || self.ejected_block.as_ref() == Some(&info.block)
                    || matches!(self.note, Some(CardNote::MountError(_)));
                if info.mount.is_none() && wanted && !skip {
                    self.start_mount(info);
                }
            }
        }
        self.refresh_card();
    }

    fn read_free_space(&self, mount: PathBuf) {
        let tx = self.tx.clone();
        spawn("free-space", move || {
            let _ = tx.send(Msg::Free(transfer::free_space(&mount)));
        });
    }

    fn start_mount(&mut self, card: CardInfo) {
        let Some(udisks) = self.udisks.clone() else {
            return;
        };
        self.mounting = true;
        let tx = self.tx.clone();
        spawn("mount", move || {
            let _ = tx.send(Msg::Mounted(udisks.mount(&card)));
        });
    }

    fn eject(&mut self) {
        if self.card.is_none() || self.ejecting {
            return;
        }
        self.note = None;
        if self.transferring {
            self.stop_transfer.store(true, Ordering::SeqCst);
            self.eject_after_copy = true;
        } else {
            self.run_eject();
        }
        self.refresh_card();
    }

    fn run_eject(&mut self) {
        let (Some(udisks), Some(card)) = (self.udisks.clone(), self.card.clone()) else {
            return;
        };
        self.ejecting = true;
        let tx = self.tx.clone();
        spawn("eject", move || {
            rustix::fs::sync();
            let _ = tx.send(Msg::Ejected(udisks.eject(&card)));
        });
    }

    fn transfer_done(&mut self) {
        self.transferring = false;
        self.stop_transfer.store(false, Ordering::SeqCst);
        let stopped = self.eject_after_copy;
        let card_present = self.card.as_ref().is_some_and(|c| c.mount.is_some());
        let more = !outbox_files(&self.dirs.outbox).is_empty();
        let batch_over = stopped || !card_present || !more || self.transfer_failed;
        if batch_over && self.batch_copied > 0 {
            let count = std::mem::take(&mut self.batch_copied);
            if self.ejected_block.is_none() && !stopped {
                let tx = self.tx.clone();
                notify::songs_copied(count, move || {
                    let _ = tx.send(Msg::Eject);
                });
            }
        }
        // A transfer that failed for a reason on the card goes back to waiting.
        for job in &mut self.jobs {
            if job.stage == Stage::Copying {
                job.stage = Stage::WaitingCard;
            }
        }
        self.show_all();
        if let Some(card) = &self.card
            && let Some(mount) = &card.mount
        {
            self.read_free_space(mount.clone());
        }
        if self.eject_after_copy {
            self.eject_after_copy = false;
            self.run_eject();
        }
        self.refresh_waiting();
    }

    fn start_transfer(&mut self, mount: &Path) {
        let files = outbox_files(&self.dirs.outbox);
        let work: Vec<(Option<u64>, PathBuf)> = files
            .into_iter()
            .map(|file| {
                let id = self
                    .jobs
                    .iter()
                    .find(|j| j.outbox_file.as_ref() == Some(&file))
                    .map(|j| j.id);
                (id, file)
            })
            .collect();
        self.transferring = true;
        self.card_cleanup_pending = false;
        let tx = self.tx.clone();
        let stop = self.stop_transfer.clone();
        let tracks = mount.join("Tracks");
        spawn("transfer", move || {
            let device = transfer::mount_device(tracks.parent().unwrap());
            if device.is_ok() {
                clean_card_temps(&tracks);
            }
            let _ = tx.send(Msg::Free(transfer::free_space(tracks.parent().unwrap())));
            for (id, file) in work {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(id) = id {
                    let _ = tx.send(Msg::CopyStarted(id));
                }
                match device
                    .as_ref()
                    .map_err(|e| std::io::Error::other(e.to_string()))
                    .and_then(|device| transfer::copy_to_card_device(&file, &tracks, *device))
                {
                    Ok(_) => {
                        let _ = tx.send(Msg::Copied(id));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::CopyFailed {
                            id,
                            reason: e.to_string(),
                        });
                        break;
                    }
                }
            }
            let _ = tx.send(Msg::TransferDone);
        });
    }

    // Scheduling -----------------------------------------------------------

    fn busy(&self) -> bool {
        self.jobs.iter().any(|j| j.running)
    }

    fn swap_tools(&mut self) {
        if self.staged.is_empty() || self.busy() {
            return;
        }
        for staged in self.staged.drain(..) {
            if let Err(e) = tools::install(&staged, &self.dirs.tools) {
                eprintln!("sansa-dl: could not install {}: {e}", staged.tool.key());
            }
        }
        self.tools = Resolved::load(&self.dirs.tools, self.bundled.as_deref());
        self.refresh_tools();
    }

    fn schedule(&mut self) {
        if self.stopping {
            return;
        }
        self.swap_tools();
        if self.staged.is_empty() {
            self.start_jobs();
        }
        let waiting = self.last_waiting.unwrap_or(0) > 0;
        let idle = !self.ejecting && !self.eject_after_copy && self.ejected_block.is_none();
        if (waiting || self.card_cleanup_pending)
            && idle
            && !self.transferring
            && !self
                .jobs
                .iter()
                .any(|j| j.running && matches!(j.stage, Stage::Fixing | Stage::Canceled))
            && !self.transfer_failed
            && let Some(mount) = self.card.as_ref().and_then(|c| c.mount.clone())
        {
            self.start_transfer(&mount);
        }
        if waiting
            && idle
            && !self.mounting
            && !matches!(self.note, Some(CardNote::MountError(_)))
            && let Some(card) = self.card.clone()
            && card.mount.is_none()
        {
            self.start_mount(card);
        }
        self.refresh_card();
    }

    fn start_jobs(&mut self) {
        let (Some(ytdlp), Some(deno), Some(ffmpeg)) =
            (self.tools.ytdlp(), self.tools.deno(), self.tools.ffmpeg())
        else {
            return;
        };
        let mut finding = self
            .jobs
            .iter()
            .filter(|j| j.running && j.stage == Stage::Finding)
            .count();
        let mut downloading = self
            .jobs
            .iter()
            .filter(|j| j.running && j.stage == Stage::Downloading)
            .count();
        let mut changed = false;
        for i in 0..self.jobs.len() {
            if self.jobs[i].running {
                continue;
            }
            let (id, stage, generation) =
                (self.jobs[i].id, self.jobs[i].stage, self.jobs[i].generation);
            let cancel = Arc::new(Cancel::default());
            let tx = self.tx.clone();
            match stage {
                Stage::Finding if finding < MAX_FINDING => {
                    finding += 1;
                    let (url, ytdlp, deno, cancel) = (
                        self.jobs[i].url.clone(),
                        ytdlp.clone(),
                        deno.clone(),
                        cancel.clone(),
                    );
                    spawn("find", move || {
                        let result = discover(&ytdlp, &deno, &url, &cancel);
                        let _ = tx.send(Msg::Discovered {
                            id,
                            generation,
                            result,
                        });
                    });
                }
                Stage::Waiting if downloading < MAX_DOWNLOADS => {
                    downloading += 1;
                    self.jobs[i].stage = Stage::Downloading;
                    self.jobs[i].fraction = None;
                    self.jobs[i].speed = None;
                    let spec = DownloadJob {
                        id,
                        url: self.jobs[i].url.clone(),
                        playlist_index: self.jobs[i].playlist_index,
                        dir: self.dirs.downloads.join(format!("{id}-{generation}")),
                        generation,
                        ytdlp: ytdlp.clone(),
                        deno: deno.clone(),
                        ffmpeg_dir: ffmpeg
                            .ffmpeg
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_default(),
                        sponsorblock: self.cfg.sponsorblock,
                    };
                    let cancel = cancel.clone();
                    spawn("download", move || {
                        let result = download(&spec, &cancel, &tx);
                        let _ = tx.send(Msg::Downloaded {
                            id,
                            generation,
                            result,
                        });
                    });
                }
                Stage::Fixing => {
                    let Some(file) = self.jobs[i].downloaded.clone() else {
                        self.jobs[i].stage = Stage::Waiting;
                        changed = true;
                        continue;
                    };
                    let (ffmpeg, outbox) = (ffmpeg.clone(), self.dirs.outbox.clone());
                    spawn("fix", move || {
                        let result = fixer::fix(&ffmpeg, &file, &outbox);
                        let _ = tx.send(Msg::Fixed {
                            id,
                            generation,
                            result,
                        });
                    });
                }
                _ => continue,
            }
            self.jobs[i].running = true;
            self.cancels.insert(id, cancel);
            changed = true;
        }
        if changed {
            self.show_all();
        }
    }
}

// Workers ------------------------------------------------------------------

fn discover(ytdlp: &Path, deno: &Path, url: &str, cancel: &Cancel) -> Result<Found, String> {
    let args = ytdlp::discovery_args(url, Some(deno));
    let mut json = String::new();
    let out = proc::run(ytdlp, &args, cancel, |line| json.push_str(line))
        .map_err(|e| format!("yt-dlp: {e}"))?;
    if cancel.is_cancelled() {
        return Err("Canceled".into());
    }
    if !out.success {
        return Err(failure(url, &out.stderr));
    }
    ytdlp::parse_discovery(&json)
}

/// The row gets plain text; the full yt-dlp message goes to the log.
fn failure(url: &str, stderr: &str) -> String {
    eprintln!("sansa-dl: yt-dlp failed for {url}: {}", stderr.trim());
    ytdlp::plain_reason(stderr)
}

struct DownloadJob {
    generation: u64,
    id: u64,
    url: String,
    playlist_index: Option<u32>,
    dir: PathBuf,
    ytdlp: PathBuf,
    deno: PathBuf,
    ffmpeg_dir: PathBuf,
    sponsorblock: bool,
}

fn download(job: &DownloadJob, cancel: &Cancel, tx: &Sender<Msg>) -> Result<PathBuf, String> {
    fs::create_dir_all(&job.dir).map_err(|e| e.to_string())?;
    let args = ytdlp::download_args(&ytdlp::DownloadSpec {
        url: &job.url,
        playlist_index: job.playlist_index,
        dir: &job.dir,
        ffmpeg_dir: &job.ffmpeg_dir,
        deno: Some(&job.deno),
        sponsorblock: job.sponsorblock,
    });
    let mut file: Option<PathBuf> = None;
    let mut last = Instant::now() - PROGRESS_EVERY;
    let mut processing = false;
    let result = proc::run(&job.ytdlp, &args, cancel, |line| {
        match ytdlp::parse_line(line) {
            Line::File(path) => file = Some(path),
            Line::Progress(p) => {
                if last.elapsed() >= PROGRESS_EVERY || p.fraction == Some(1.0) {
                    last = Instant::now();
                    let _ = tx.send(Msg::Progress {
                        id: job.id,
                        generation: job.generation,
                        fraction: p.fraction,
                        speed: p.speed,
                        processing,
                    });
                }
            }
            Line::Processing => {
                processing = true;
                let _ = tx.send(Msg::Progress {
                    id: job.id,
                    generation: job.generation,
                    fraction: Some(1.0),
                    speed: None,
                    processing,
                });
            }
            Line::Other => {}
        }
    });
    let fail = |reason: String| {
        let _ = fs::remove_dir_all(&job.dir);
        Err(reason)
    };
    let out = match result {
        Ok(out) => out,
        Err(e) => return fail(format!("yt-dlp: {e}")),
    };
    if cancel.is_cancelled() {
        return fail("Canceled".into());
    }
    if !out.success {
        return fail(failure(&job.url, &out.stderr));
    }
    let found = file.filter(|f| f.exists()).or_else(|| {
        fs::read_dir(&job.dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp3"))
    });
    match found {
        Some(path) => Ok(path),
        None => fail("yt-dlp produced no file".into()),
    }
}

// Updates ------------------------------------------------------------------

fn record_tool_check(state: &mut State, tool: Tool, now: u64) -> bool {
    if !state.due(tool.key(), now) {
        return false;
    }
    state.checked.insert(tool.key().into(), now);
    true
}

fn spawn_updater(tx: Sender<Msg>, dirs: Dirs, resolved: Resolved, appimage: Option<PathBuf>) {
    spawn("updates", move || {
        let agent = net::agent();
        let mut state: State = read_json(&dirs.state_file);
        for tool in Tool::ALL {
            let have = resolved.version(tool).map(str::to_string);
            let now = now_secs();
            if !record_tool_check(&mut state, tool, now) {
                continue;
            }
            let _ = write_json(&dirs.state_file, &state);
            let staged = tools::fetch_latest(&agent, tool).and_then(|latest| {
                if have
                    .as_deref()
                    .is_none_or(|h| tools::is_newer(&latest.version, h))
                {
                    tools::stage(&agent, tool, &latest, &dirs.tools).map(Some)
                } else {
                    Ok(None)
                }
            });
            match staged {
                Ok(Some(staged)) => {
                    let _ = tx.send(Msg::ToolStaged(staged));
                }
                Ok(None) => {}
                Err(e) => eprintln!("sansa-dl: {} update: {e}", tool.key()),
            }
        }
        if let Some(appimage) = appimage {
            let now = now_secs();
            if state.due("app", now) {
                state.checked.insert("app".into(), now);
                let _ = write_json(&dirs.state_file, &state);
                match selfupdate::update(&agent, &appimage, selfupdate::CURRENT) {
                    Ok(Some(version)) => {
                        let _ = tx.send(Msg::AppUpdated(version));
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("sansa-dl: app update: {e}"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_attempt_does_not_match_current_job() {
        let mut job = Job::new(1, "https://example.test".into());
        job.stage = Stage::Downloading;
        job.generation = 2;
        assert!(!current_attempt(&job, 1, 1, Stage::Downloading));
        assert!(current_attempt(&job, 1, 2, Stage::Downloading));
    }

    #[test]
    fn missing_tool_check_is_recorded_before_download() {
        let mut state = State::default();
        assert!(record_tool_check(&mut state, Tool::Ytdlp, 100));
        assert!(!record_tool_check(&mut state, Tool::Ytdlp, 101));
        assert!(record_tool_check(&mut state, Tool::Ytdlp, 100 + 86400));
    }

    #[test]
    fn cleans_only_matching_card_temps() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(fixer::temp_name());
        let other = dir.path().join(".sansa-other.part");
        fs::write(&temp, b"x").unwrap();
        fs::write(&other, b"x").unwrap();
        clean_card_temps(dir.path());
        assert!(!temp.exists());
        assert!(other.exists());
    }

    #[test]
    fn free_space_text() {
        assert_eq!(format_free(114_300_000_000), "114 GB");
        assert_eq!(format_free(9_540_000_000), "9.5 GB");
        assert_eq!(format_free(2_100_000_000_000), "2.1 TB");
        assert_eq!(format_free(512_000_000), "512 MB");
    }

    #[test]
    fn outbox_listing_skips_temp_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.mp3"), b"a").unwrap();
        fs::write(dir.path().join(fixer::temp_name()), b"t").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let files = outbox_files(dir.path());
        assert_eq!(files, vec![dir.path().join("a.mp3")]);
    }

    #[test]
    fn stale_download_folders_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["1", "2", "junk"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        let mut kept = Job::new(1, "https://a.example".into());
        kept.downloaded = Some(dir.path().join("1/x.mp3"));
        let other = Job::new(2, "https://b.example".into());
        remove_stale_downloads(dir.path(), &[kept, other]);
        assert!(dir.path().join("1").exists());
        assert!(!dir.path().join("2").exists());
        assert!(!dir.path().join("junk").exists());
    }
}
