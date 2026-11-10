//! What the Sansa firmware needs before it shows and plays a track.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub const ID3_LIMIT: u64 = 1024 * 1024;
pub const MAX_NAME_BYTES: usize = 255;

// Room for " (99)" collision suffixes.
const SUFFIX_RESERVE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub codec: String,
    pub rate: u32,
    pub bits: Option<u32>,
    pub channels: u32,
}

#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub audio: Option<Audio>,
    pub has_art: bool,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub issues: Vec<String>,
    pub ext: &'static str,
    pub args: Vec<String>,
}

/// Size of a leading ID3v2 tag in bytes, 0 when there is none.
pub fn id3_size(header: &[u8]) -> u64 {
    if header.len() < 10 || &header[..3] != b"ID3" {
        return 0;
    }
    let b = |i: usize| u64::from(header[i]);
    10 + ((b(6) << 21) | (b(7) << 14) | (b(8) << 7) | b(9))
}

pub fn id3_size_of(path: &Path) -> io::Result<u64> {
    let mut header = Vec::with_capacity(10);
    File::open(path)?.take(10).read_to_end(&mut header)?;
    Ok(id3_size(&header))
}

fn is_lossless(codec: &str) -> bool {
    codec.starts_with("pcm_")
        || matches!(
            codec,
            "alac" | "ape" | "wavpack" | "tta" | "mlp" | "truehd" | "flac"
        )
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// Decides what is wrong with a track and which ffmpeg arguments fix it.
pub fn plan(info: &TrackInfo, tag_bytes: u64) -> Result<Plan, String> {
    let audio = info.audio.as_ref().ok_or("No audio stream found")?;
    let mut issues = Vec::new();
    if tag_bytes > ID3_LIMIT {
        issues.push(format!("ID3 tag is {}", format_bytes(tag_bytes)));
    } else if info.has_art {
        issues.push("Embedded cover art".to_string());
    }
    if audio.channels > 2 {
        issues.push(format!("{} channels", audio.channels));
    }

    let mut args = strings(&["-map", "0:a:0", "-vn", "-map_metadata", "0"]);
    let downmix: &[&str] = if audio.channels > 2 {
        &["-ac", "2"]
    } else {
        &[]
    };
    let mp3_tags = ["-id3v2_version", "3", "-write_id3v1", "0"];

    // mp3 plays at 44.1 and 48 kHz, so it is never re-encoded for sample rate.
    if audio.codec == "mp3" {
        args.extend(strings(&["-c:a", "copy"]));
        args.extend(strings(&mp3_tags));
        return Ok(Plan {
            issues,
            ext: "mp3",
            args,
        });
    }

    if is_lossless(&audio.codec) {
        let cd_quality = audio.rate == 44100 && audio.bits == Some(16);
        if !cd_quality {
            let bits = audio.bits.map_or("?".to_string(), |b| b.to_string());
            issues.push(format!("{bits}-bit / {} kHz", khz(audio.rate)));
        }
        if audio.codec != "flac" && !audio.codec.starts_with("pcm_") {
            issues.push(format!("{} may not play", audio.codec.to_uppercase()));
        }
        if audio.codec == "flac" && cd_quality && downmix.is_empty() {
            args.extend(strings(&["-c:a", "copy"]));
        } else {
            args.extend(strings(&[
                "-c:a",
                "flac",
                "-sample_fmt",
                "s16",
                "-ar",
                "44100",
            ]));
            args.extend(strings(downmix));
        }
        return Ok(Plan {
            issues,
            ext: "flac",
            args,
        });
    }

    issues.push(format!("{} may not play", audio.codec.to_uppercase()));
    args.extend(strings(&["-c:a", "libmp3lame", "-q:a", "0", "-ar", "44100"]));
    args.extend(strings(downmix));
    args.extend(strings(&mp3_tags));
    Ok(Plan {
        issues,
        ext: "mp3",
        args,
    })
}

/// Rechecks a converted file. Returns the problem, or None when it passes.
pub fn verify(info: &TrackInfo, tag_bytes: u64, ext: &str) -> Option<String> {
    let Some(audio) = &info.audio else {
        return Some("Output has no audio".to_string());
    };
    if info.has_art {
        return Some("Cover art is still embedded".to_string());
    }
    if tag_bytes > ID3_LIMIT {
        return Some(format!("ID3 tag is still {}", format_bytes(tag_bytes)));
    }
    if ext == "flac" && (audio.rate != 44100 || audio.bits != Some(16)) {
        return Some(format!("Output is {}", describe(audio)));
    }
    None
}

/// vfat-safe stem, short enough that stem, `.ext` and a collision suffix fit in 255 bytes.
pub fn safe_stem(name: &str, ext: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let budget = MAX_NAME_BYTES.saturating_sub(ext.len() + 1 + SUFFIX_RESERVE);
    let mut end = cleaned.len().min(budget);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    let stem = cleaned[..end].trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        "_".to_string()
    } else {
        stem.to_string()
    }
}

/// "Song.mp3", "Song (2).mp3", "Song (3).mp3"
pub fn numbered_name(stem: &str, ext: &str, number: u32) -> String {
    if number <= 1 {
        format!("{stem}.{ext}")
    } else {
        format!("{stem} ({number}).{ext}")
    }
}

pub fn describe(audio: &Audio) -> String {
    let bits = audio.bits.map_or(String::new(), |b| format!("{b}-bit / "));
    format!("{} {bits}{} kHz", audio.codec.to_uppercase(), khz(audio.rate))
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 * 1024 {
        format!("{} KB", ((n as f64 / 1024.0).round() as u64).max(1))
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

fn khz(rate: u32) -> String {
    let value = f64::from(rate) / 1000.0;
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str, rate: u32, bits: Option<u32>, channels: u32, art: bool) -> TrackInfo {
        TrackInfo {
            audio: Some(Audio {
                codec: codec.into(),
                rate,
                bits,
                channels,
            }),
            has_art: art,
            tags: HashMap::new(),
        }
    }

    fn has(plan: &Plan, pair: &[&str]) -> bool {
        plan.args.windows(pair.len()).any(|w| w == pair)
    }

    #[test]
    fn id3_size_reads_synchsafe_header() {
        let mut header = *b"ID3\x03\x00\x00\x00\x00\x02\x01";
        assert_eq!(id3_size(&header), 10 + (2 << 7) + 1);
        header[6] = 1;
        assert_eq!(id3_size(&header), 10 + (1 << 21) + (2 << 7) + 1);
    }

    #[test]
    fn id3_size_is_zero_without_tag() {
        assert_eq!(id3_size(b"\xff\xfb\x90\x00\x00\x00\x00\x00\x00\x00"), 0);
        assert_eq!(id3_size(b"ID3"), 0);
        assert_eq!(id3_size(b""), 0);
    }

    #[test]
    fn mp3_is_copied_even_at_48khz() {
        let plan = plan(&info("mp3", 48000, None, 2, false), 2000).unwrap();
        assert_eq!(plan.ext, "mp3");
        assert!(has(&plan, &["-c:a", "copy"]));
        assert!(has(&plan, &["-id3v2_version", "3"]));
        assert!(has(&plan, &["-write_id3v1", "0"]));
        assert!(!plan.args.contains(&"-ar".to_string()));
        assert!(plan.issues.is_empty());
    }

    #[test]
    fn mp3_with_art_reports_it() {
        let plan = plan(&info("mp3", 44100, None, 2, true), 5000).unwrap();
        assert_eq!(plan.issues, vec!["Embedded cover art"]);
        assert!(has(&plan, &["-map", "0:a:0"]));
        assert!(plan.args.contains(&"-vn".to_string()));
    }

    #[test]
    fn oversized_tag_is_reported_with_size() {
        let plan = plan(&info("mp3", 44100, None, 2, true), 3 * 1024 * 1024).unwrap();
        assert_eq!(plan.issues, vec!["ID3 tag is 3.0 MB"]);
    }

    #[test]
    fn cd_flac_is_copied() {
        let plan = plan(&info("flac", 44100, Some(16), 2, false), 0).unwrap();
        assert_eq!(plan.ext, "flac");
        assert!(has(&plan, &["-c:a", "copy"]));
    }

    #[test]
    fn hires_flac_is_converted_to_16_bit_44khz() {
        let plan = plan(&info("flac", 48000, Some(24), 2, false), 0).unwrap();
        assert_eq!(plan.ext, "flac");
        assert!(has(&plan, &["-c:a", "flac"]));
        assert!(has(&plan, &["-sample_fmt", "s16"]));
        assert!(has(&plan, &["-ar", "44100"]));
        assert_eq!(plan.issues, vec!["24-bit / 48 kHz"]);
    }

    #[test]
    fn opus_becomes_mp3_v0() {
        let plan = plan(&info("opus", 48000, None, 2, false), 0).unwrap();
        assert_eq!(plan.ext, "mp3");
        assert!(has(&plan, &["-c:a", "libmp3lame"]));
        assert!(has(&plan, &["-q:a", "0"]));
        assert!(has(&plan, &["-ar", "44100"]));
        assert_eq!(plan.issues, vec!["OPUS may not play"]);
    }

    #[test]
    fn surround_is_downmixed() {
        let mp3 = plan(&info("mp3", 44100, None, 6, false), 0).unwrap();
        assert_eq!(mp3.issues, vec!["6 channels"]);
        let opus = plan(&info("opus", 48000, None, 6, false), 0).unwrap();
        assert!(has(&opus, &["-ac", "2"]));
        let flac = plan(&info("flac", 44100, Some(16), 6, false), 0).unwrap();
        assert!(has(&flac, &["-c:a", "flac"]));
        assert!(has(&flac, &["-ac", "2"]));
    }

    #[test]
    fn plan_needs_audio() {
        assert!(plan(&TrackInfo::default(), 0).is_err());
    }

    #[test]
    fn verify_accepts_clean_output() {
        assert_eq!(verify(&info("mp3", 48000, None, 2, false), 4000, "mp3"), None);
        assert_eq!(
            verify(&info("flac", 44100, Some(16), 2, false), 0, "flac"),
            None
        );
    }

    #[test]
    fn verify_rejects_bad_output() {
        let ok = info("mp3", 44100, None, 2, false);
        assert!(verify(&TrackInfo::default(), 0, "mp3").unwrap().contains("no audio"));
        assert!(verify(&info("mp3", 44100, None, 2, true), 0, "mp3").unwrap().contains("art"));
        assert!(verify(&ok, ID3_LIMIT + 1, "mp3").unwrap().contains("ID3"));
        assert_eq!(verify(&ok, ID3_LIMIT, "mp3"), None);
        let flac = info("flac", 48000, Some(24), 2, false);
        assert_eq!(verify(&flac, 0, "flac").unwrap(), "Output is FLAC 24-bit / 48 kHz");
    }

    #[test]
    fn safe_stem_replaces_vfat_characters() {
        assert_eq!(safe_stem("a/b\\c:d*e?f\"g<h>i|j", "mp3"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(safe_stem("tab\there\nnew", "mp3"), "tab_here_new");
    }

    #[test]
    fn safe_stem_strips_trailing_dots_and_spaces() {
        assert_eq!(safe_stem("Song. . ", "mp3"), "Song");
        assert_eq!(safe_stem("...", "mp3"), "_");
        assert_eq!(safe_stem("", "mp3"), "_");
    }

    #[test]
    fn safe_stem_keeps_full_name_under_255_bytes() {
        for text in ["a".repeat(400), "é".repeat(400), "曲".repeat(400), "🎵".repeat(400)] {
            let stem = safe_stem(&text, "mp3");
            let longest = numbered_name(&stem, "mp3", 99);
            assert!(longest.len() <= MAX_NAME_BYTES, "{} bytes", longest.len());
            assert!(!stem.is_empty());
        }
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("Song", "mp3", 1), "Song.mp3");
        assert_eq!(numbered_name("Song", "mp3", 2), "Song (2).mp3");
        assert_eq!(numbered_name("Song", "flac", 3), "Song (3).flac");
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(100), "1 KB");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(1536 * 1024), "1.5 MB");
    }
}
