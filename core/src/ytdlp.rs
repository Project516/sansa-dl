//! yt-dlp command lines and output parsing.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PROGRESS_PREFIX: &str = "SDLP ";
pub const POST_PREFIX: &str = "SDLQ ";
pub const FILE_PREFIX: &str = "SDLF ";

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub fraction: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Progress(Progress),
    Processing,
    File(PathBuf),
    Other,
}

#[derive(Deserialize)]
struct RawProgress {
    status: Option<String>,
    downloaded_bytes: Option<f64>,
    total_bytes: Option<f64>,
    total_bytes_estimate: Option<f64>,
    speed: Option<f64>,
}

pub fn parse_line(line: &str) -> Line {
    let line = line.trim_end();
    if let Some(json) = line.strip_prefix(PROGRESS_PREFIX) {
        let Ok(raw) = serde_json::from_str::<RawProgress>(json) else {
            return Line::Other;
        };
        if raw.status.as_deref() == Some("finished") {
            return Line::Progress(Progress {
                fraction: Some(1.0),
                speed: None,
            });
        }
        let total = raw
            .total_bytes
            .or(raw.total_bytes_estimate)
            .filter(|t| *t > 0.0);
        let fraction = match (raw.downloaded_bytes, total) {
            (Some(done), Some(total)) => Some((done / total).clamp(0.0, 1.0)),
            _ => None,
        };
        return Line::Progress(Progress {
            fraction,
            speed: raw.speed.filter(|s| *s > 0.0),
        });
    }
    if line.starts_with(POST_PREFIX) {
        return Line::Processing;
    }
    if let Some(path) = line.strip_prefix(FILE_PREFIX) {
        return Line::File(PathBuf::from(path));
    }
    Line::Other
}

pub fn format_speed(bytes_per_second: f64) -> String {
    let kb = bytes_per_second / 1000.0;
    if kb >= 1000.0 {
        format!("{:.1} MB/s", kb / 1000.0)
    } else {
        format!("{} kB/s", kb.round().max(1.0))
    }
}

/// Text after "ERROR:" on the last error line, cut to a row-sized reason.
pub fn short_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or(lines.last())
        .copied()
        .unwrap_or("yt-dlp failed");
    let line = line.strip_prefix("ERROR:").unwrap_or(line).trim();
    let line = line
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .map_or(line, |(_, rest)| rest.trim());
    // Drop the video id that follows the extractor name, and the cause chain.
    let line = match line.split_once(": ") {
        Some((id, rest)) if !id.contains(' ') => rest,
        _ => line,
    };
    let line = line
        .split_once(" (caused by")
        .map_or(line, |(head, _)| head);
    if line.chars().count() <= 160 {
        line.to_string()
    } else {
        let cut: String = line.chars().take(157).collect();
        format!("{cut}...")
    }
}

/// Plain wording for the errors people meet most. Anything else keeps the
/// cleaned yt-dlp text.
pub fn plain_reason(stderr: &str) -> String {
    let text = short_reason(stderr);
    let lower = format!("{} {}", text, stderr).to_lowercase();
    let has = |needle: &str| lower.contains(needle);
    let http = |code: &str| has(&format!("http error {code}"));
    let reason = if http("404") {
        "Page not found (HTTP 404)"
    } else if http("403") {
        "Access denied (HTTP 403)"
    } else if http("429") {
        "Too many requests, try later (HTTP 429)"
    } else if (500..600).any(|code| http(&code.to_string())) {
        "The site has a problem (HTTP 5xx)"
    } else if has("unsupported url") {
        "This link is not supported"
    } else if has("private video") {
        "Private video"
    } else if has("sign in to confirm")
        || has("confirm you're not a bot")
        || has("confirm you\u{2019}re not a bot")
    {
        "The site wants a sign-in"
    } else if has("members-only") || has("join this channel") {
        "Members-only video"
    } else if has("not available in your country") || has("geo restrict") || has("geo-restrict") {
        "Not available in your country"
    } else if has("video unavailable")
        || has("this video is not available")
        || has("has been removed")
    {
        "Video unavailable"
    } else if has("name resolution")
        || has("network is unreachable")
        || has("timed out")
        || has("connection refused")
        || has("unable to connect")
    {
        "No internet connection"
    } else if has("unable to download webpage") {
        "Could not load the page"
    } else {
        return text;
    };
    reason.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    /// Position in the playlist, when the entry came straight from it.
    pub index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Video { title: String },
    Playlist { entries: Vec<Entry> },
}

fn entry_url(entry: &Value) -> Option<String> {
    ["url", "webpage_url"]
        .iter()
        .filter_map(|k| entry.get(k).and_then(Value::as_str))
        .find(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(str::to_string)
}

fn collect(entries: &[Value], nested: bool, out: &mut Vec<Entry>) {
    for (i, entry) in entries.iter().enumerate() {
        if entry.is_null() {
            continue;
        }
        if let Some(inner) = entry.get("entries").and_then(Value::as_array) {
            collect(inner, true, out);
            continue;
        }
        let Some(url) = entry_url(entry) else {
            continue;
        };
        let title = entry
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(&url)
            .to_string();
        let index = if nested {
            None
        } else {
            u32::try_from(i + 1).ok()
        };
        out.push(Entry { url, title, index });
    }
}

/// Reads the output of `yt-dlp -J --flat-playlist`.
pub fn parse_discovery(json: &str) -> Result<Found, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|_| "Unexpected answer from yt-dlp".to_string())?;
    match root.get("entries").and_then(Value::as_array) {
        Some(entries) => {
            let mut out = Vec::new();
            collect(entries, false, &mut out);
            if out.is_empty() {
                Err("No media found".to_string())
            } else {
                Ok(Found::Playlist { entries: out })
            }
        }
        None => {
            let title = root
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
                .to_string();
            Ok(Found::Video { title })
        }
    }
}

pub fn discovery_args(url: &str, deno: Option<&Path>) -> Vec<String> {
    let mut args = base_args(deno);
    args.extend(["-J", "--flat-playlist", "--", url].map(String::from));
    args
}

fn base_args(deno: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = ["--ignore-config", "--no-colors", "--no-warnings"]
        .map(String::from)
        .into();
    if let Some(deno) = deno {
        args.push("--js-runtimes".into());
        args.push(format!("deno:{}", deno.display()));
    }
    args
}

pub struct DownloadSpec<'a> {
    pub url: &'a str,
    pub playlist_index: Option<u32>,
    pub dir: &'a Path,
    pub ffmpeg_dir: &'a Path,
    pub deno: Option<&'a Path>,
    pub sponsorblock: bool,
}

pub fn download_args(spec: &DownloadSpec) -> Vec<String> {
    let mut args = base_args(spec.deno);
    let mut add = |items: &[&str]| args.extend(items.iter().map(|s| (*s).to_string()));
    add(&[
        "-f",
        "bestaudio/best",
        "-x",
        "--audio-format",
        "mp3",
        "--audio-quality",
        "0",
    ]);
    if spec.sponsorblock {
        add(&["--sponsorblock-remove", "default"]);
    }
    add(&["--embed-metadata", "--no-embed-thumbnail"]);
    add(&["--parse-metadata", "%(artist,uploader|)s:%(meta_artist)s"]);
    add(&[
        "--parse-metadata",
        "%(album,playlist_title,uploader|)s:%(meta_album)s",
    ]);
    if spec.playlist_index.is_some() {
        add(&["--parse-metadata", "%(playlist_index|)s:%(meta_track)s"]);
    }
    add(&["--newline", "--progress"]);
    add(&["--progress-template", "download:SDLP %(progress)j"]);
    add(&[
        "--progress-template",
        "postprocess:SDLQ %(progress.postprocessor)s",
    ]);
    add(&["--print", "after_move:SDLF %(filepath)s"]);
    add(&["-o", "%(title)s.%(ext)s"]);
    let (dir, ffmpeg) = (
        spec.dir.to_string_lossy().into_owned(),
        spec.ffmpeg_dir.to_string_lossy().into_owned(),
    );
    add(&["-P", &dir, "--ffmpeg-location", &ffmpeg]);
    match spec.playlist_index {
        Some(n) => add(&["--playlist-items", &n.to_string()]),
        None => add(&["--no-playlist"]),
    }
    add(&["--", spec.url]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_download_progress() {
        let line = r#"SDLP {"status": "downloading", "downloaded_bytes": 430, "total_bytes": 1000, "speed": 2100000.5, "eta": 3}"#;
        assert_eq!(
            parse_line(line),
            Line::Progress(Progress {
                fraction: Some(0.43),
                speed: Some(2100000.5)
            })
        );
    }

    #[test]
    fn progress_uses_the_estimate_and_survives_nulls() {
        let line = r#"SDLP {"status":"downloading","downloaded_bytes":50,"total_bytes":null,"total_bytes_estimate":200.0,"speed":null}"#;
        assert_eq!(
            parse_line(line),
            Line::Progress(Progress {
                fraction: Some(0.25),
                speed: None
            })
        );
        let unknown = r#"SDLP {"status":"downloading","downloaded_bytes":50,"total_bytes":null}"#;
        assert_eq!(
            parse_line(unknown),
            Line::Progress(Progress {
                fraction: None,
                speed: None
            })
        );
    }

    #[test]
    fn finished_means_full() {
        let line = r#"SDLP {"status":"finished","downloaded_bytes":1000,"total_bytes":1000}"#;
        assert_eq!(
            parse_line(line),
            Line::Progress(Progress {
                fraction: Some(1.0),
                speed: None
            })
        );
    }

    #[test]
    fn parses_file_and_processing_lines() {
        assert_eq!(
            parse_line("SDLF /tmp/a b/Song.mp3\n"),
            Line::File(PathBuf::from("/tmp/a b/Song.mp3"))
        );
        assert_eq!(parse_line("SDLQ ExtractAudio"), Line::Processing);
        assert_eq!(parse_line("[youtube] abc: Downloading"), Line::Other);
        assert_eq!(parse_line("SDLP not json"), Line::Other);
    }

    #[test]
    fn common_errors_get_plain_text() {
        let cases = [
            (
                "ERROR: [generic] x: Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404>)",
                "Page not found (HTTP 404)",
            ),
            (
                "ERROR: [youtube] a: HTTP Error 403: Forbidden",
                "Access denied (HTTP 403)",
            ),
            (
                "ERROR: HTTP Error 429: Too Many Requests",
                "Too many requests, try later (HTTP 429)",
            ),
            (
                "ERROR: HTTP Error 503: Service Unavailable",
                "The site has a problem (HTTP 5xx)",
            ),
            (
                "ERROR: Unsupported URL: https://example.test/x",
                "This link is not supported",
            ),
            (
                "ERROR: [youtube] a: Private video. Sign in if you've been granted access",
                "Private video",
            ),
            (
                "ERROR: [youtube] a: Sign in to confirm you're not a bot",
                "The site wants a sign-in",
            ),
            ("ERROR: [youtube] a: Video unavailable", "Video unavailable"),
            (
                "ERROR: [youtube] a: The uploader has not made this video available in your country (not available in your country)",
                "Not available in your country",
            ),
            (
                "ERROR: [generic] x: Unable to download webpage: <urlopen error [Errno -3] Temporary failure in name resolution>",
                "No internet connection",
            ),
            (
                "ERROR: [generic] x: Unable to download webpage: something odd",
                "Could not load the page",
            ),
        ];
        for (raw, plain) in cases {
            assert_eq!(plain_reason(raw), plain, "{raw}");
        }
        assert_eq!(
            plain_reason("ERROR: Something new broke"),
            "Something new broke"
        );
        assert_eq!(plain_reason(""), "yt-dlp failed");
    }

    #[test]
    fn speed_text() {
        assert_eq!(format_speed(2_100_000.0), "2.1 MB/s");
        assert_eq!(format_speed(512_000.0), "512 kB/s");
        assert_eq!(format_speed(10.0), "1 kB/s");
    }

    #[test]
    fn reasons_are_short_and_plain() {
        let stderr = "WARNING: x\nERROR: [youtube] abc: Video unavailable\n";
        assert_eq!(short_reason(stderr), "Video unavailable");
        let not_found = "ERROR: [generic] x: Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404: Not Found>)";
        assert_eq!(
            short_reason(not_found),
            "Unable to download webpage: HTTP Error 404: Not Found"
        );
        assert_eq!(short_reason(""), "yt-dlp failed");
        assert_eq!(short_reason("plain failure"), "plain failure");
        let long = format!("ERROR: {}", "x".repeat(300));
        assert_eq!(short_reason(&long).chars().count(), 160);
    }

    #[test]
    fn discovery_of_a_video() {
        let found = parse_discovery(r#"{"id":"a","title":"One song","formats":[]}"#).unwrap();
        assert_eq!(
            found,
            Found::Video {
                title: "One song".into()
            }
        );
    }

    #[test]
    fn discovery_of_a_playlist_keeps_positions() {
        let json = r#"{"_type":"playlist","title":"Mix","entries":[
            {"url":"https://example.com/v1","title":"First"},
            null,
            {"id":"x"},
            {"webpage_url":"https://example.com/v4","title":""}
        ]}"#;
        let Found::Playlist { entries } = parse_discovery(json).unwrap() else {
            panic!("expected a playlist");
        };
        assert_eq!(
            entries,
            vec![
                Entry {
                    url: "https://example.com/v1".into(),
                    title: "First".into(),
                    index: Some(1)
                },
                Entry {
                    url: "https://example.com/v4".into(),
                    title: "https://example.com/v4".into(),
                    index: Some(4)
                },
            ]
        );
    }

    #[test]
    fn nested_playlists_are_flattened_without_positions() {
        let json = r#"{"entries":[{"entries":[{"url":"https://example.com/a","title":"A"}]}]}"#;
        let Found::Playlist { entries } = parse_discovery(json).unwrap() else {
            panic!("expected a playlist");
        };
        assert_eq!(entries[0].index, None);
    }

    #[test]
    fn empty_playlist_and_garbage_are_errors() {
        assert_eq!(
            parse_discovery(r#"{"entries":[]}"#),
            Err("No media found".into())
        );
        assert!(parse_discovery("oops").is_err());
    }

    fn spec(index: Option<u32>, sponsorblock: bool) -> Vec<String> {
        download_args(&DownloadSpec {
            url: "https://example.com/watch",
            playlist_index: index,
            dir: Path::new("/dl/1"),
            ffmpeg_dir: Path::new("/tools"),
            deno: Some(Path::new("/tools/deno")),
            sponsorblock,
        })
    }

    fn has(args: &[String], pair: &[&str]) -> bool {
        args.windows(pair.len()).any(|w| w == pair)
    }

    #[test]
    fn single_video_arguments() {
        let args = spec(None, true);
        assert_eq!(args[0], "--ignore-config");
        assert!(has(
            &args,
            &["-x", "--audio-format", "mp3", "--audio-quality", "0"]
        ));
        assert!(has(&args, &["--sponsorblock-remove", "default"]));
        assert!(has(&args, &["--js-runtimes", "deno:/tools/deno"]));
        assert!(has(&args, &["--ffmpeg-location", "/tools"]));
        assert!(has(&args, &["-o", "%(title)s.%(ext)s"]));
        assert!(args.contains(&"--no-playlist".to_string()));
        assert!(args.contains(&"--no-embed-thumbnail".to_string()));
        assert!(!args.iter().any(|a| a.contains("meta_track")));
        assert_eq!(&args[args.len() - 2..], ["--", "https://example.com/watch"]);
    }

    #[test]
    fn playlist_item_arguments() {
        let args = spec(Some(3), false);
        assert!(has(&args, &["--playlist-items", "3"]));
        assert!(args.iter().any(|a| a.ends_with(":%(meta_track)s")));
        assert!(!args.contains(&"--sponsorblock-remove".to_string()));
        assert!(!args.contains(&"--no-playlist".to_string()));
    }
}
