//! Jobs: one row per link or playlist item.

use crate::ytdlp::format_speed;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Finding,
    Waiting,
    Downloading,
    Fixing,
    WaitingCard,
    Copying,
    Copied,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    /// What yt-dlp gets. A playlist item keeps the playlist link.
    pub url: String,
    pub title: String,
    pub playlist_index: Option<u32>,
    /// The link is known to be media, so a retry skips the search.
    pub found: bool,
    pub stage: Stage,
    pub reason: String,
    pub downloaded: Option<PathBuf>,
    pub outbox_file: Option<PathBuf>,
    #[serde(default)]
    pub metadata: Option<crate::fixer::Metadata>,
    #[serde(default)]
    pub soulseek: Option<SoulseekJob>,
    #[serde(skip)]
    pub running: bool,
    #[serde(skip)]
    pub generation: u64,
    #[serde(skip)]
    pub fraction: Option<f64>,
    #[serde(skip)]
    pub speed: Option<f64>,
    #[serde(skip)]
    pub processing: bool,
}

impl Job {
    pub fn new(id: u64, url: String) -> Self {
        Self {
            id,
            title: url.clone(),
            url,
            playlist_index: None,
            found: false,
            stage: Stage::Finding,
            reason: String::new(),
            downloaded: None,
            outbox_file: None,
            metadata: None,
            soulseek: None,
            running: false,
            generation: 0,
            fraction: None,
            speed: None,
            processing: false,
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.stage, Stage::Copied | Stage::Failed | Stage::Canceled)
    }

    pub fn can_cancel(&self) -> bool {
        matches!(
            self.stage,
            Stage::Finding | Stage::Waiting | Stage::Downloading | Stage::Fixing
        )
    }

    pub fn can_retry(&self) -> bool {
        !self.running && matches!(self.stage, Stage::Failed | Stage::Canceled)
    }

    /// Where a retry picks up.
    pub fn retry_stage(&self) -> Stage {
        if self.outbox_file.as_ref().is_some_and(|f| f.exists()) {
            Stage::WaitingCard
        } else if self.downloaded.as_ref().is_some_and(|f| f.exists()) {
            Stage::Fixing
        } else if self.found {
            Stage::Waiting
        } else {
            Stage::Finding
        }
    }

    pub fn status(&self) -> String {
        match self.stage {
            Stage::Finding => "Finding".into(),
            Stage::Waiting if self.soulseek.is_some() => "Waiting for Soulseek".into(),
            Stage::Waiting => "Waiting".into(),
            Stage::Downloading if self.processing => "Processing".into(),
            Stage::Downloading => match (self.fraction, self.speed) {
                (Some(f), Some(s)) => {
                    format!(
                        "Downloading {}% at {}",
                        (f * 100.0).floor(),
                        format_speed(s)
                    )
                }
                (Some(f), None) => format!("Downloading {}%", (f * 100.0).floor()),
                _ => "Downloading".into(),
            },
            Stage::Fixing => "Fixing".into(),
            Stage::WaitingCard => "Waiting for the Sansa".into(),
            Stage::Copying => "Copying to the Sansa".into(),
            Stage::Copied => "Copied to the Sansa".into(),
            Stage::Failed => format!("Failed: {}", self.reason),
            Stage::Canceled => "Canceled".into(),
        }
    }

    /// 0 to 1 while a percentage is known, -1 for a busy bar without one.
    pub fn progress(&self) -> f64 {
        match self.stage {
            Stage::Downloading if self.processing => 1.0,
            Stage::Downloading => self.fraction.unwrap_or(-1.0),
            Stage::Copied | Stage::WaitingCard => 1.0,
            Stage::Finding | Stage::Fixing | Stage::Copying => -1.0,
            _ => 0.0,
        }
    }

    pub fn stage_name(&self) -> &'static str {
        match self.stage {
            Stage::Finding => "finding",
            Stage::Waiting => "waiting",
            Stage::Downloading => "downloading",
            Stage::Fixing => "fixing",
            Stage::WaitingCard => "card",
            Stage::Copying => "copying",
            Stage::Copied => "done",
            Stage::Failed => "failed",
            Stage::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoulseekJob {
    pub peer: crate::slskd::PeerFile,
    pub transfer: Option<crate::slskd::Transfer>,
}

/// http(s) links in pasted text, in order, without repeats.
pub fn parse_links(text: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let token = token.trim_matches(|c| matches!(c, '<' | '>' | '"' | '\'' | ',' | ';'));
        let rest = token
            .strip_prefix("https://")
            .or_else(|| token.strip_prefix("http://"));
        if rest.is_some_and(|r| {
            r.chars()
                .next()
                .is_some_and(|c| c != '/' && c != '?' && c != '#')
        }) && !links.iter().any(|l| l == token)
        {
            links.push(token.to_string());
        }
    }
    links
}

/// Jobs to keep across a restart, with work that was in flight set back to its start.
pub fn restore(mut jobs: Vec<Job>) -> Vec<Job> {
    jobs.retain(|j| !matches!(j.stage, Stage::Copied | Stage::Canceled));
    for job in &mut jobs {
        let kept_download = job.downloaded.as_ref().is_some_and(|f| f.exists());
        job.stage = match job.stage {
            Stage::Downloading | Stage::Fixing if kept_download => Stage::Fixing,
            Stage::Downloading | Stage::Fixing => Stage::Waiting,
            Stage::Copying => Stage::WaitingCard,
            other => other,
        };
    }
    jobs.retain(|j| match (&j.stage, &j.outbox_file) {
        (Stage::WaitingCard, Some(file)) => file.exists(),
        (Stage::WaitingCard, None) => false,
        _ => true,
    });
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_any_whitespace_and_keeps_links() {
        let text = "https://a.example/x  not-a-link\nhttp://b.example/y\u{2028}ftp://c.example\r\n<https://d.example/z>,";
        assert_eq!(
            parse_links(text),
            [
                "https://a.example/x",
                "http://b.example/y",
                "https://d.example/z"
            ]
        );
    }

    #[test]
    fn drops_repeats_and_empty_hosts() {
        assert_eq!(
            parse_links("https://a.example https://a.example https:// https:///x"),
            ["https://a.example"]
        );
        assert!(parse_links("").is_empty());
    }

    fn job(stage: Stage) -> Job {
        let mut job = Job::new(1, "https://a.example".into());
        job.stage = stage;
        job
    }

    #[test]
    fn status_text() {
        let mut j = job(Stage::Downloading);
        assert_eq!(j.status(), "Downloading");
        j.fraction = Some(0.437);
        assert_eq!(j.status(), "Downloading 43%");
        j.speed = Some(2_100_000.0);
        assert_eq!(j.status(), "Downloading 43% at 2.1 MB/s");
        j.processing = true;
        assert_eq!(j.status(), "Processing");
        assert_eq!(job(Stage::Finding).status(), "Finding");
        assert_eq!(job(Stage::WaitingCard).status(), "Waiting for the Sansa");
        assert_eq!(job(Stage::Copied).status(), "Copied to the Sansa");
        let mut failed = job(Stage::Failed);
        failed.reason = "Video unavailable".into();
        assert_eq!(failed.status(), "Failed: Video unavailable");
    }

    #[test]
    fn buttons_follow_the_stage() {
        assert!(job(Stage::Waiting).can_cancel());
        assert!(!job(Stage::WaitingCard).can_cancel());
        assert!(job(Stage::Failed).can_retry());
        assert!(job(Stage::Canceled).can_retry());
        assert!(!job(Stage::Copied).can_retry());
        assert!(job(Stage::Copied).is_finished());
    }

    #[test]
    fn restore_resets_work_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp3");
        std::fs::write(&file, b"x").unwrap();
        let mut copying = job(Stage::Copying);
        copying.outbox_file = Some(file.clone());
        let mut gone = job(Stage::WaitingCard);
        gone.outbox_file = Some(dir.path().join("missing.mp3"));
        let restored = restore(vec![
            job(Stage::Downloading),
            job(Stage::Fixing),
            job(Stage::Finding),
            copying,
            gone,
            job(Stage::Copied),
            job(Stage::Canceled),
            job(Stage::Failed),
        ]);
        let stages: Vec<_> = restored.iter().map(|j| j.stage).collect();
        assert_eq!(
            stages,
            [
                Stage::Waiting,
                Stage::Waiting,
                Stage::Finding,
                Stage::WaitingCard,
                Stage::Failed
            ]
        );
    }

    #[test]
    fn restore_keeps_soulseek_transfer_for_file_recovery() {
        let mut pending = job(Stage::Downloading);
        pending.soulseek = Some(SoulseekJob {
            peer: crate::slskd::PeerFile {
                username: "peer".into(),
                filename: "track.mp3".into(),
                size: 5,
                length: None,
                bitrate: None,
            },
            transfer: Some(crate::slskd::Transfer {
                id: "a".into(),
                username: "peer".into(),
                destination: "b".repeat(48),
                size: 5,
            }),
        });
        let restored = restore(vec![pending]);
        assert_eq!(restored[0].stage, Stage::Waiting);
        assert!(restored[0].soulseek.as_ref().unwrap().transfer.is_some());
    }

    #[test]
    fn jobs_round_trip_through_json() {
        let mut j = job(Stage::Waiting);
        j.playlist_index = Some(3);
        j.fraction = Some(0.5);
        let back: Job = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
        assert_eq!(back.playlist_index, Some(3));
        assert_eq!(back.fraction, None);
    }
}
