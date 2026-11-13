//! Recording search using the MusicBrainz web service.

use serde::Deserialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://musicbrainz.org/ws/2/recording/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub release: Option<String>,
    pub length_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Results {
    recordings: Vec<MusicBrainzRecording>,
}

#[derive(Deserialize)]
struct MusicBrainzRecording {
    id: String,
    title: String,
    length: Option<u64>,
    #[serde(rename = "artist-credit", default)]
    artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct ArtistCredit {
    name: String,
    #[serde(default)]
    joinphrase: String,
}

#[derive(Deserialize)]
struct Release {
    title: String,
}

/// Search recordings. Call from a worker thread: MusicBrainz limits requests to one per second.
pub fn search(query: &str) -> Result<Vec<Recording>, String> {
    static LAST_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut last = LAST_REQUEST
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(wait) = last.and_then(|time| Duration::from_secs(1).checked_sub(time.elapsed())) {
        std::thread::sleep(wait);
    }
    *last = Some(Instant::now());
    drop(last);
    search_at(&crate::net::agent(), ENDPOINT, query)
}

fn search_at(agent: &ureq::Agent, endpoint: &str, query: &str) -> Result<Vec<Recording>, String> {
    let body = agent
        .get(endpoint)
        .header(
            "User-Agent",
            concat!(
                "sansa-dl/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/Project516/sansa-dl)"
            ),
        )
        .query("query", query.trim())
        .query("fmt", "json")
        .query("limit", "25")
        .call()
        .map_err(|e| format!("MusicBrainz search: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("MusicBrainz response: {e}"))?;
    let results: Results =
        serde_json::from_str(&body).map_err(|e| format!("MusicBrainz response: {e}"))?;
    Ok(results
        .recordings
        .into_iter()
        .map(|recording| Recording {
            id: recording.id,
            title: recording.title,
            artist: recording
                .artist_credit
                .into_iter()
                .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
                .collect(),
            release: recording
                .releases
                .into_iter()
                .next()
                .map(|release| release.title),
            length_ms: recording.length,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn searches_and_reads_recording_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/ws/2/recording/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.contains("query=artist%20%26%20song"), "{request}");
            assert!(request.contains("fmt=json"));
            assert!(request.to_lowercase().contains("user-agent: sansa-dl/"));
            let body = r#"{"recordings":[{"id":"recording-id","title":"Song","length":123000,"artist-credit":[{"name":"First","joinphrase":" & "},{"name":"Second"}],"releases":[{"title":"Album"}]},{"id":"other-id","title":"Other"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let results = search_at(&crate::net::agent(), &endpoint, " artist & song ").unwrap();
        server.join().unwrap();
        assert_eq!(
            results[0],
            Recording {
                id: "recording-id".into(),
                title: "Song".into(),
                artist: "First & Second".into(),
                release: Some("Album".into()),
                length_ms: Some(123000),
            }
        );
        assert_eq!(results[1].release, None);
        assert_eq!(results[1].length_ms, None);
    }

    #[test]
    fn empty_query_does_not_search() {
        assert!(search("  ").unwrap().is_empty());
    }
}
