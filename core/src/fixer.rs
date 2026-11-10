//! Makes a downloaded track playable on the Sansa and moves it into the outbox.

use crate::rules::{self, Audio, TrackInfo};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

pub const TEMP_PREFIX: &str = ".sansa-";
pub const TEMP_SUFFIX: &str = ".part";

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

pub fn temp_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{TEMP_PREFIX}{}-{nanos:x}-{n}{TEMP_SUFFIX}", std::process::id())
}

pub fn is_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(TEMP_PREFIX) && n.ends_with(TEMP_SUFFIX))
}

pub fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no details")
        .to_string()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn positive(value: &Value, key: &str) -> Option<u32> {
    let n = match value.get(key)? {
        Value::String(s) => s.parse::<u32>().ok()?,
        Value::Number(n) => u32::try_from(n.as_u64()?).ok()?,
        _ => return None,
    };
    (n > 0).then_some(n)
}

pub fn parse_probe(json: &str) -> Result<TrackInfo, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("ffprobe output: {e}"))?;
    let mut info = TrackInfo::default();
    let mut audio_stream: Option<&Value> = None;
    for stream in root
        .get("streams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match stream.get("codec_type").and_then(Value::as_str) {
            Some("audio") if info.audio.is_none() => {
                info.audio = Some(Audio {
                    codec: text(stream, "codec_name").unwrap_or_default(),
                    rate: positive(stream, "sample_rate").unwrap_or(0),
                    bits: positive(stream, "bits_per_raw_sample")
                        .or_else(|| positive(stream, "bits_per_sample")),
                    channels: positive(stream, "channels").unwrap_or(0),
                });
                audio_stream = Some(stream);
            }
            Some("video") => info.has_art = true,
            _ => {}
        }
    }
    for source in [root.get("format"), audio_stream].into_iter().flatten() {
        if let Some(tags) = source.get("tags").and_then(Value::as_object) {
            for (key, value) in tags {
                if let Some(value) = value.as_str() {
                    info.tags
                        .entry(key.to_lowercase())
                        .or_insert_with(|| value.to_string());
                }
            }
        }
    }
    Ok(info)
}

fn run(program: &Path, args: &[String]) -> Result<std::process::Output, String> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            let name = program.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            if e.kind() == io::ErrorKind::NotFound {
                format!("{name} was not found")
            } else {
                format!("{name}: {e}")
            }
        })
}

pub fn probe(ffprobe: &Path, file: &Path) -> Result<TrackInfo, String> {
    let args: Vec<String> = [
        "-v",
        "error",
        "-show_streams",
        "-show_format",
        "-of",
        "json",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain([file.to_string_lossy().into_owned()])
    .collect();
    let out = run(ffprobe, &args)?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            last_line(&String::from_utf8_lossy(&out.stderr))
        ));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// Returns the fixed file inside `outbox`. The input is left alone.
pub fn fix(tools: &Ffmpeg, input: &Path, outbox: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(outbox).map_err(|e| format!("outbox: {e}"))?;
    let info = probe(&tools.ffprobe, input)?;
    let tag_bytes = rules::id3_size_of(input).map_err(|e| format!("read: {e}"))?;
    let plan = rules::plan(&info, tag_bytes)?;

    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.push(input.to_string_lossy().into_owned());
    args.extend(plan.args.iter().cloned());
    let blank = |key: &str| info.tags.get(key).is_none_or(|v| v.trim().is_empty());
    if blank("album")
        && let Some(artist) = info.tags.get("artist").filter(|a| !a.trim().is_empty())
    {
        args.push("-metadata".into());
        args.push(format!("album={artist}"));
    }
    let temp = outbox.join(temp_name());
    args.extend(["-f".into(), plan.ext.into(), temp.to_string_lossy().into_owned()]);

    let result = (|| {
        let out = run(&tools.ffmpeg, &args)?;
        if !out.status.success() {
            return Err(format!(
                "ffmpeg failed: {}",
                last_line(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        let out_info = probe(&tools.ffprobe, &temp)?;
        let out_tags = rules::id3_size_of(&temp).map_err(|e| format!("read: {e}"))?;
        if let Some(problem) = rules::verify(&out_info, out_tags, plan.ext) {
            return Err(problem);
        }
        let stem = input
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        publish(&temp, outbox, &rules::safe_stem(&stem, plan.ext), plan.ext)
            .map_err(|e| format!("outbox: {e}"))
    })();
    let _ = fs::remove_file(&temp);
    result
}

// A hard link never replaces an existing name, so two fixes cannot clobber each other.
fn publish(temp: &Path, dir: &Path, stem: &str, ext: &str) -> io::Result<PathBuf> {
    for number in 1.. {
        let target = dir.join(rules::numbered_name(stem, ext, number));
        match fs::hard_link(temp, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) if !target.exists() => {
                fs::rename(temp, &target)?;
                return Ok(target);
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_json() {
        let json = r#"{
            "streams": [
                {"codec_type": "audio", "codec_name": "flac", "sample_rate": "48000",
                 "bits_per_raw_sample": "24", "bits_per_sample": 0, "channels": 2,
                 "tags": {"ARTIST": "Someone"}},
                {"codec_type": "video", "codec_name": "mjpeg"}
            ],
            "format": {"tags": {"title": "Name", "artist": "Format Artist"}}
        }"#;
        let info = parse_probe(json).unwrap();
        let audio = info.audio.unwrap();
        assert_eq!(audio.codec, "flac");
        assert_eq!((audio.rate, audio.bits, audio.channels), (48000, Some(24), 2));
        assert!(info.has_art);
        assert_eq!(info.tags["title"], "Name");
        assert_eq!(info.tags["artist"], "Format Artist");
    }

    #[test]
    fn bits_fall_back_to_bits_per_sample() {
        let json = r#"{"streams":[{"codec_type":"audio","codec_name":"pcm_s16le",
            "sample_rate":"44100","bits_per_sample":16,"channels":2}]}"#;
        assert_eq!(parse_probe(json).unwrap().audio.unwrap().bits, Some(16));
    }

    #[test]
    fn temp_names_are_unique_and_detected() {
        let a = temp_name();
        assert_ne!(a, temp_name());
        assert!(is_temp(Path::new(&a)));
        assert!(!is_temp(Path::new("song.mp3")));
        assert!(!is_temp(Path::new(".sansa-x.mp3")));
    }

    #[test]
    fn publish_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..3 {
            let temp = dir.path().join(format!("t{n}"));
            fs::write(&temp, [n as u8]).unwrap();
            publish(&temp, dir.path(), "Song", "mp3").unwrap();
        }
        assert_eq!(fs::read(dir.path().join("Song.mp3")).unwrap(), [0]);
        assert_eq!(fs::read(dir.path().join("Song (2).mp3")).unwrap(), [1]);
        assert_eq!(fs::read(dir.path().join("Song (3).mp3")).unwrap(), [2]);
    }

    #[test]
    fn last_line_skips_blank_lines() {
        assert_eq!(last_line("a\nb\n\n"), "b");
        assert_eq!(last_line(""), "no details");
    }
}
