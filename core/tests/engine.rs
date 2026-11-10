//! Runs the coordinator with fake yt-dlp and deno scripts and real ffmpeg.
//! Card detection and updates are off, so no card or network is touched.
//! Skips without an ffmpeg that has LAME.

use sansa_dl_core::engine::{self, Emit, Engine, JobView, Options, UiEvent};
use sansa_dl_core::paths::Dirs;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn find(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake yt-dlp. `-J` answers discovery from the link. Anything else
/// "downloads" a tagged tone into the `-P` folder and reports it.
const FAKE_YTDLP: &str = r#"
case "$*" in
  *--version*) echo 2099.01.01; exit 0;;
esac
last=""; dir=""; prev=""; items=""
for a in "$@"; do
  [ "$prev" = "-P" ] && dir="$a"
  [ "$prev" = "--playlist-items" ] && items="$a"
  prev="$a"; last="$a"
done
case "$*" in
  *" -J "*)
    case "$last" in
      *bad*) sleep 0.3; echo "ERROR: [generic] bad: Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404>)" >&2; exit 1;;
      *list*) echo '{"_type":"playlist","entries":[{"url":"https://example.test/a","title":"First"},{"url":"https://example.test/b","title":"Second"}]}';;
      *) echo '{"id":"x","title":"Single Song"}';;
    esac
    exit 0;;
esac
title="Single Song"
[ "$items" = "1" ] && title="First"
[ "$items" = "2" ] && title="Second"
case "$last" in *slow*) sleep 30;; esac
mkdir -p "$dir"
"$FFMPEG" -v error -f lavfi -i sine=duration=1 -c:a libmp3lame -metadata title="$title" "$dir/$title.mp3"
echo 'SDLP {"status":"downloading","downloaded_bytes":50,"total_bytes":100,"speed":1000000.0}'
echo "SDLP {\"status\":\"finished\",\"downloaded_bytes\":100,\"total_bytes\":100}"
echo "SDLF $dir/$title.mp3"
"#;

struct Rig {
    engine: Engine,
    jobs: Arc<Mutex<Vec<JobView>>>,
    dirs: Dirs,
    _root: tempfile::TempDir,
}

fn rig() -> Option<Rig> {
    let ffmpeg = find("ffmpeg")?;
    let ffprobe = find("ffprobe")?;
    let encoders = Command::new(&ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    if !String::from_utf8_lossy(&encoders.stdout).contains("libmp3lame") {
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let bundled = root.path().join("bundled");
    fs::create_dir(&bundled).unwrap();
    script(
        &bundled.join("yt-dlp"),
        &format!("FFMPEG={}\n{}", ffmpeg.display(), FAKE_YTDLP),
    );
    script(&bundled.join("deno"), "echo 2.0.0");
    std::os::unix::fs::symlink(&ffmpeg, bundled.join("ffmpeg")).unwrap();
    std::os::unix::fs::symlink(&ffprobe, bundled.join("ffprobe")).unwrap();
    fs::write(
        bundled.join("versions.json"),
        r#"{"yt-dlp":"2099.01.01","deno":"2.0.0","ffmpeg":"9.0"}"#,
    )
    .unwrap();

    let dirs = Dirs::under(&root.path().join("data"), &root.path().join("config"));
    let jobs: Arc<Mutex<Vec<JobView>>> = Arc::default();
    let sink = jobs.clone();
    let emit: Emit = Arc::new(move |event| match event {
        UiEvent::Jobs(all) => *sink.lock().unwrap() = all,
        UiEvent::JobUpdate(one) => {
            let mut all = sink.lock().unwrap();
            if let Some(slot) = all.iter_mut().find(|j| j.id == one.id) {
                *slot = one;
            }
        }
        _ => {}
    });
    let options = Options {
        card: false,
        updates: false,
        bundled_tools: Some(bundled),
    };
    let engine = engine::start_with(dirs.clone(), None, emit, options);
    Some(Rig {
        engine,
        jobs,
        dirs,
        _root: root,
    })
}

impl Rig {
    fn wait(&self, what: &str, done: impl Fn(&[JobView]) -> bool) {
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            if done(&self.jobs.lock().unwrap()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {what}: {:?}",
            self.jobs.lock().unwrap()
        );
    }

    fn outbox(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dirs.outbox)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

macro_rules! rig {
    () => {
        match rig() {
            Some(rig) => rig,
            None => {
                eprintln!("skipped: ffmpeg with libmp3lame not found");
                return;
            }
        }
    };
}

#[test]
fn a_link_goes_from_finding_to_waiting_for_the_card() {
    let rig = rig!();
    rig.engine
        .add_links(vec!["https://example.test/one".into()]);
    rig.wait("the song to reach the outbox", |jobs| {
        jobs.len() == 1 && jobs[0].stage == "card"
    });
    let jobs = rig.jobs.lock().unwrap().clone();
    assert_eq!(jobs[0].title, "Single Song");
    assert_eq!(jobs[0].status, "Waiting for the Sansa");
    drop(jobs);
    assert_eq!(rig.outbox(), ["Single Song.mp3"]);
    let leftovers: Vec<_> = fs::read_dir(&rig.dirs.downloads).unwrap().collect();
    assert!(leftovers.is_empty(), "the download folder is cleaned up");
    let saved = fs::read_to_string(&rig.dirs.jobs_file).unwrap();
    assert!(saved.contains("Single Song"));
}

#[test]
fn a_playlist_becomes_one_row_per_song() {
    let rig = rig!();
    rig.engine
        .add_links(vec!["https://example.test/list".into()]);
    rig.wait("both songs to reach the outbox", |jobs| {
        jobs.len() == 2 && jobs.iter().all(|j| j.stage == "card")
    });
    let titles: Vec<String> = rig
        .jobs
        .lock()
        .unwrap()
        .iter()
        .map(|j| j.title.clone())
        .collect();
    assert_eq!(titles, ["First", "Second"]);
    assert_eq!(rig.outbox(), ["First.mp3", "Second.mp3"]);
}

#[test]
fn a_bad_link_fails_on_its_own_row_and_retry_runs_it_again() {
    let rig = rig!();
    rig.engine.add_links(vec![
        "https://example.test/bad".into(),
        "https://example.test/ok".into(),
    ]);
    rig.wait("one failure and one song", |jobs| {
        jobs.len() == 2 && jobs[0].stage == "failed" && jobs[1].stage == "card"
    });
    let failed = rig.jobs.lock().unwrap()[0].clone();
    assert_eq!(failed.status, "Failed: Page not found (HTTP 404)");
    assert!(failed.can_retry && !failed.can_cancel);

    rig.engine.retry(failed.id);
    rig.wait("the retry to start", |jobs| jobs[0].stage == "finding");
    rig.wait("the retried row to fail again", |jobs| {
        jobs[0].stage == "failed"
    });
}

#[test]
fn cancel_stops_a_running_download_and_clear_removes_finished_rows() {
    let rig = rig!();
    rig.engine
        .add_links(vec!["https://example.test/slow".into()]);
    rig.wait("the download to start", |jobs| {
        jobs.len() == 1 && jobs[0].stage == "downloading"
    });
    let id = rig.jobs.lock().unwrap()[0].id;
    let begin = Instant::now();
    rig.engine.cancel(id);
    rig.wait("the cancel", |jobs| jobs[0].stage == "canceled");
    assert!(
        begin.elapsed() < Duration::from_secs(10),
        "the 30 s sleep was cut short"
    );
    assert!(rig.jobs.lock().unwrap()[0].can_retry);

    rig.engine.clear_finished();
    rig.wait("the list to empty", |jobs| jobs.is_empty());
    assert!(rig.outbox().is_empty());
}

#[test]
fn the_queue_survives_a_restart() {
    let rig = rig!();
    rig.engine
        .add_links(vec!["https://example.test/one".into()]);
    rig.wait("the song to reach the outbox", |jobs| {
        jobs.len() == 1 && jobs[0].stage == "card"
    });
    rig.engine.shutdown();

    let jobs: Arc<Mutex<Vec<JobView>>> = Arc::default();
    let sink = jobs.clone();
    let emit: Emit = Arc::new(move |event| {
        if let UiEvent::Jobs(all) = event {
            *sink.lock().unwrap() = all;
        }
    });
    let bundled = rig._root.path().join("bundled");
    let options = Options {
        card: false,
        updates: false,
        bundled_tools: Some(bundled),
    };
    let _second = engine::start_with(rig.dirs.clone(), None, emit, options);
    let end = Instant::now() + Duration::from_secs(10);
    while jobs.lock().unwrap().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let restored = jobs.lock().unwrap().clone();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].stage, "card");
    assert_eq!(restored[0].title, "Single Song");
}

#[test]
fn shutdown_keeps_a_running_download_resumable() {
    let rig = rig!();
    rig.engine
        .add_links(vec!["https://example.test/slow".into()]);
    rig.wait("the download to start", |jobs| {
        jobs.len() == 1 && jobs[0].stage == "downloading"
    });
    rig.engine.shutdown();
    std::thread::sleep(Duration::from_millis(500));
    let saved = fs::read_to_string(&rig.dirs.jobs_file).unwrap();
    let jobs: Vec<sansa_dl_core::queue::Job> = serde_json::from_str(&saved).unwrap();
    assert_ne!(jobs[0].stage, sansa_dl_core::queue::Stage::Failed);
    assert_eq!(
        sansa_dl_core::queue::restore(jobs)[0].stage,
        sansa_dl_core::queue::Stage::Waiting
    );
}

#[test]
fn bad_jobs_file_keeps_downloads() {
    let root = tempfile::tempdir().unwrap();
    let dirs = Dirs::under(&root.path().join("data"), &root.path().join("config"));
    dirs.create().unwrap();
    fs::create_dir(dirs.downloads.join("keep")).unwrap();
    fs::write(dirs.jobs_file.with_extension("json.bad"), "older").unwrap();
    fs::write(&dirs.jobs_file, "broken").unwrap();
    let engine = engine::start_with(
        dirs.clone(),
        None,
        Arc::new(|_| {}),
        Options {
            card: false,
            updates: false,
            bundled_tools: None,
        },
    );
    engine.shutdown();
    assert!(dirs.downloads.join("keep").exists());
    let second = engine::start_with(
        dirs.clone(),
        None,
        Arc::new(|_| {}),
        Options {
            card: false,
            updates: false,
            bundled_tools: None,
        },
    );
    second.shutdown();
    assert!(dirs.downloads.join("keep").exists());
    assert_eq!(
        fs::read_to_string(dirs.jobs_file.with_extension("json.bad")).unwrap(),
        "older"
    );
    assert_eq!(
        fs::read_to_string(dirs.jobs_file.with_extension("json.bad.2")).unwrap(),
        "broken"
    );
}
