//! Runs the fixer against small files made with ffmpeg. Skips without ffmpeg.

use sansa_dl_core::fixer::{self, Ffmpeg};
use sansa_dl_core::rules::{self, ID3_LIMIT};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tools() -> Option<Ffmpeg> {
    let find = |name: &str| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    };
    let tools = Ffmpeg {
        ffmpeg: find("ffmpeg")?,
        ffprobe: find("ffprobe")?,
    };
    // The fixtures are mp3 and flac, so a build without LAME cannot make them.
    let encoders = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&encoders.stdout)
        .contains("libmp3lame")
        .then_some(tools)
}

macro_rules! need_ffmpeg {
    () => {
        match tools() {
            Some(t) => t,
            None => {
                eprintln!("skipped: ffmpeg with libmp3lame not found");
                return;
            }
        }
    };
}

fn run(tools: &Ffmpeg, args: &[&str]) {
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg {args:?}");
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

// A noisy jpeg bigger than the ID3 limit, to attach as cover art.
fn big_cover(tools: &Ffmpeg, dir: &Path) -> PathBuf {
    let cover = dir.join("cover.jpg");
    run(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=1600x1600",
            "-vf",
            "noise=alls=100:allf=t+u",
            "-frames:v",
            "1",
            "-q:v",
            "1",
            s(&cover),
        ],
    );
    assert!(fs::metadata(&cover).unwrap().len() > ID3_LIMIT);
    cover
}

fn mp3_with_cover(tools: &Ffmpeg, dir: &Path) -> PathBuf {
    let cover = big_cover(tools, dir);
    let out = dir.join("Cover Song.mp3");
    run(
        tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1:sample_rate=48000",
            "-i",
            s(&cover),
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "96k",
            "-c:v",
            "copy",
            "-id3v2_version",
            "3",
            "-metadata",
            "title=Cover Song",
            "-metadata",
            "artist=Some Artist",
            "-metadata:s:v",
            "title=Album cover",
            "-disposition:v",
            "attached_pic",
            s(&out),
        ],
    );
    out
}

#[test]
fn applies_selected_recording_metadata() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mp3");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
            "-c:a",
            "libmp3lame",
            s(&input),
        ],
    );
    let metadata = fixer::Metadata {
        title: "Selected title".into(),
        artist: "Selected artist".into(),
        album: Some("Selected album".into()),
    };
    let output =
        fixer::fix_with_metadata(&tools, &input, &dir.path().join("outbox"), Some(&metadata))
            .unwrap();
    let info = fixer::probe(&tools.ffprobe, &output).unwrap();
    assert_eq!(
        info.tags.get("title").map(String::as_str),
        Some("Selected title")
    );
    assert_eq!(
        info.tags.get("artist").map(String::as_str),
        Some("Selected artist")
    );
    assert_eq!(
        info.tags.get("album").map(String::as_str),
        Some("Selected album")
    );
}

#[test]
fn strips_a_huge_cover_from_an_mp3_and_keeps_tags() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = mp3_with_cover(&tools, dir.path());
    assert!(rules::id3_size_of(&input).unwrap() > ID3_LIMIT);

    let outbox = dir.path().join("outbox");
    let fixed = fixer::fix(&tools, &input, &outbox).unwrap();

    assert_eq!(fixed.parent().unwrap(), outbox);
    assert_eq!(fixed.file_name().unwrap(), "Cover Song.mp3");
    let info = fixer::probe(&tools.ffprobe, &fixed).unwrap();
    assert!(!info.has_art);
    assert_eq!(info.audio.unwrap().rate, 48000, "mp3 must not be resampled");
    assert!(rules::id3_size_of(&fixed).unwrap() < ID3_LIMIT);
    assert_eq!(info.tags["title"], "Cover Song");
    assert_eq!(info.tags["artist"], "Some Artist");
    assert_eq!(
        info.tags["album"], "Some Artist",
        "empty album takes the artist"
    );
    assert!(input.exists(), "the input stays");
    let leftovers = fs::read_dir(&outbox).unwrap().count();
    assert_eq!(leftovers, 1, "no temp files");
}

#[test]
fn keeps_an_existing_album() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Song.mp3");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
            "-c:a",
            "libmp3lame",
            "-metadata",
            "artist=A",
            "-metadata",
            "album=B",
            s(&input),
        ],
    );
    let fixed = fixer::fix(&tools, &input, &dir.path().join("outbox")).unwrap();
    assert_eq!(
        fixer::probe(&tools.ffprobe, &fixed).unwrap().tags["album"],
        "B"
    );
}

#[test]
fn converts_24_bit_48khz_flac_to_cd_quality() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Hires.flac");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1:sample_rate=48000",
            "-sample_fmt",
            "s32",
            "-c:a",
            "flac",
            "-bits_per_raw_sample",
            "24",
            s(&input),
        ],
    );
    let before = fixer::probe(&tools.ffprobe, &input).unwrap().audio.unwrap();
    assert_eq!((before.rate, before.bits), (48000, Some(24)));

    let fixed = fixer::fix(&tools, &input, &dir.path().join("outbox")).unwrap();
    assert_eq!(fixed.extension().unwrap(), "flac");
    let after = fixer::probe(&tools.ffprobe, &fixed).unwrap().audio.unwrap();
    assert_eq!((after.rate, after.bits), (44100, Some(16)));
}

#[test]
fn converts_opus_to_mp3_at_44khz() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Opus Song.opus");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
            "-c:a",
            "libopus",
            s(&input),
        ],
    );
    let fixed = fixer::fix(&tools, &input, &dir.path().join("outbox")).unwrap();
    assert_eq!(fixed.extension().unwrap(), "mp3");
    let audio = fixer::probe(&tools.ffprobe, &fixed).unwrap().audio.unwrap();
    assert_eq!((audio.codec.as_str(), audio.rate), ("mp3", 44100));
}

#[test]
fn downmixes_surround_flac() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Surround.flac");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1:sample_rate=44100",
            "-af",
            "pan=5.1|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0",
            "-sample_fmt",
            "s16",
            "-c:a",
            "flac",
            s(&input),
        ],
    );
    let fixed = fixer::fix(&tools, &input, &dir.path().join("outbox")).unwrap();
    assert_eq!(
        fixer::probe(&tools.ffprobe, &fixed)
            .unwrap()
            .audio
            .unwrap()
            .channels,
        2
    );
}

#[test]
fn same_title_twice_gets_two_files() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Song.mp3");
    run(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
            "-c:a",
            "libmp3lame",
            s(&input),
        ],
    );
    let outbox = dir.path().join("outbox");
    let first = fixer::fix(&tools, &input, &outbox).unwrap();
    let second = fixer::fix(&tools, &input, &outbox).unwrap();
    assert_eq!(first.file_name().unwrap(), "Song.mp3");
    assert_eq!(second.file_name().unwrap(), "Song (2).mp3");
}

#[test]
fn a_file_without_audio_fails_and_the_input_stays() {
    let tools = need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Broken.mp3");
    fs::write(&input, b"this is not audio").unwrap();
    let err = fixer::fix(&tools, &input, &dir.path().join("outbox")).unwrap_err();
    assert!(!err.is_empty());
    assert!(input.exists());
    let outbox = dir.path().join("outbox");
    assert_eq!(fs::read_dir(outbox).map_or(0, Iterator::count), 0);
}
