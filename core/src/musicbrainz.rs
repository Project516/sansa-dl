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
    status: Option<String>,
    date: Option<String>,
    #[serde(rename = "release-group")]
    group: Option<ReleaseGroup>,
}

#[derive(Deserialize)]
struct ReleaseGroup {
    #[serde(rename = "primary-type")]
    primary: Option<String>,
    #[serde(rename = "secondary-types", default)]
    secondary: Vec<String>,
}

impl Release {
    /// Lower is better: official studio albums first, then the earliest.
    fn rank(&self) -> (u8, &str) {
        let official = self.status.as_deref() == Some("Official");
        let group = self.group.as_ref();
        let album = group.is_some_and(|g| g.primary.as_deref() == Some("Album"));
        // Film albums like "Help!" are tagged Soundtrack but hold the original songs.
        let studio = album && group.is_some_and(|g| g.secondary.iter().all(|t| t == "Soundtrack"));
        let kind = match (official, studio, album) {
            (true, true, _) => 0,
            (true, false, true) => 1,
            (true, false, false) => 2,
            _ => 3,
        };
        (
            kind,
            self.date
                .as_deref()
                .filter(|d| !d.is_empty())
                .unwrap_or("9999"),
        )
    }
}

/// Every word must be in the artist or the title, so "artist song" finds the song.
fn lucene(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| {
            let word: String = word
                .chars()
                .flat_map(|c| {
                    let special = "+-&|!(){}[]^\"~*?:\\/".contains(c);
                    special.then_some('\\').into_iter().chain([c])
                })
                .collect();
            format!("(artist:{word} OR recording:{word})")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Search recordings. Call from a worker thread: MusicBrainz limits requests to one per second.
pub fn search(query: &str) -> Result<Vec<Recording>, String> {
    static LAST_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    if lucene(query).is_empty() {
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
    let agent = crate::net::agent();
    match search_at(&agent, ENDPOINT, query) {
        // MusicBrainz answers 503 when it is rate limiting.
        Err(SearchError::Busy) => {
            std::thread::sleep(Duration::from_secs(2));
            search_at(&agent, ENDPOINT, query)
        }
        other => other,
    }
    .map_err(|e| e.to_string())
}

#[derive(Debug)]
enum SearchError {
    Busy,
    Other(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SearchError::Busy => f.write_str("MusicBrainz is busy, try again in a moment"),
            SearchError::Other(e) => f.write_str(e),
        }
    }
}

fn search_at(
    agent: &ureq::Agent,
    endpoint: &str,
    query: &str,
) -> Result<Vec<Recording>, SearchError> {
    let body = agent
        .get(endpoint)
        .header(
            "User-Agent",
            concat!(
                "clipplus-manager/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/Project516/clipplus-manager)"
            ),
        )
        .query("query", lucene(query))
        .query("fmt", "json")
        // Each song repeats once per release, so ask for more than we show.
        .query("limit", "100")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(503) => SearchError::Busy,
            e => SearchError::Other(format!("MusicBrainz search: {e}")),
        })?
        .body_mut()
        .read_to_string()
        .map_err(|e| SearchError::Other(format!("MusicBrainz response: {e}")))?;
    let results: Results = serde_json::from_str(&body)
        .map_err(|e| SearchError::Other(format!("MusicBrainz response: {e}")))?;
    // One row per song: keep the copy on the best release.
    let mut songs: Vec<(Recording, (u8, String))> = Vec::new();
    for recording in results.recordings {
        let artist: String = recording
            .artist_credit
            .into_iter()
            .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
            .collect();
        let best = recording
            .releases
            .iter()
            .min_by(|a, b| a.rank().cmp(&b.rank()));
        let rank = best.map_or((4, String::new()), |r| {
            let (kind, date) = r.rank();
            (kind, date.to_string())
        });
        let song = Recording {
            id: recording.id,
            title: recording.title,
            artist,
            release: best.map(|release| release.title.clone()),
            length_ms: recording.length,
        };
        let same = |(kept, _): &&mut (Recording, (u8, String))| {
            kept.title.to_lowercase() == song.title.to_lowercase()
                && kept.artist.to_lowercase() == song.artist.to_lowercase()
        };
        match songs.iter_mut().find(same) {
            Some(kept) if rank < kept.1 => *kept = (song, rank),
            Some(_) => {}
            None => songs.push((song, rank)),
        }
    }
    // Stable, so MusicBrainz's order holds within each kind.
    songs.sort_by_key(|(_, (kind, _))| *kind);
    Ok(songs.into_iter().map(|(song, _)| song).collect())
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
            assert!(
                request.contains("query=(artist%3Aartist%20OR%20recording%3Aartist)%20AND%20(artist%3Asong%20OR%20recording%3Asong)"),
                "{request}"
            );
            assert!(request.contains("fmt=json"));
            assert!(
                request
                    .to_lowercase()
                    .contains("user-agent: clipplus-manager/")
            );
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
        assert!(search(" & - ").unwrap().is_empty());
    }

    #[test]
    fn each_word_matches_artist_or_title() {
        assert_eq!(
            lucene(" toto  africa "),
            "(artist:toto OR recording:toto) AND (artist:africa OR recording:africa)"
        );
        assert_eq!(
            lucene("AC/DC & t.n.t"),
            "(artist:AC\\/DC OR recording:AC\\/DC) AND (artist:t.n.t OR recording:t.n.t)"
        );
    }

    #[test]
    fn keeps_one_row_per_song_from_the_studio_album() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            let album = |title: &str, status: &str, date: &str, secondary: &str| {
                format!(
                    r#"{{"title":"{title}","status":"{status}","date":"{date}","release-group":{{"primary-type":"Album","secondary-types":[{secondary}]}}}}"#
                )
            };
            let body = format!(
                r#"{{"recordings":[
                    {{"id":"d","title":"Song (live)","artist-credit":[{{"name":"Band"}}],"releases":[{}]}},
                    {{"id":"a","title":"Song","artist-credit":[{{"name":"Band"}}],"releases":[{}]}},
                    {{"id":"b","title":"Other","artist-credit":[{{"name":"Band"}}],"releases":[]}},
                    {{"id":"c","title":"song","artist-credit":[{{"name":"Band"}}],"releases":[{},{},{}]}}
                ]}}"#,
                album("Live", "Bootleg", "1990", r#""Live""#),
                album("Hits", "Official", "1999", r#""Compilation""#),
                album("Bootleg", "Bootleg", "1980", ""),
                album("Reissue", "Official", "2003", ""),
                album("Debut", "Official", "1982-04", r#""Soundtrack""#),
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let results = search_at(&crate::net::agent(), &endpoint, "band song").unwrap();
        server.join().unwrap();
        let rows: Vec<_> = results
            .iter()
            .map(|r| (r.id.as_str(), r.release.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [("c", Some("Debut")), ("d", Some("Live")), ("b", None)]
        );
    }
}
