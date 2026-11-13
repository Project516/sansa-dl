//! XDG locations and the small JSON files kept in them.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "dev.project516.sansa-dl";

#[derive(Debug, Clone)]
pub struct Dirs {
    pub data: PathBuf,
    pub downloads: PathBuf,
    pub outbox: PathBuf,
    pub tools: PathBuf,
    pub config_file: PathBuf,
    pub soulseek_file: PathBuf,
    pub jobs_file: PathBuf,
    pub state_file: PathBuf,
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

impl Dirs {
    pub fn from_env() -> Self {
        Self::under(
            &xdg("XDG_DATA_HOME", ".local/share").join("sansa-dl"),
            &xdg("XDG_CONFIG_HOME", ".config").join("sansa-dl"),
        )
    }

    pub fn under(data: &Path, config: &Path) -> Self {
        Self {
            data: data.to_path_buf(),
            downloads: data.join("downloads"),
            outbox: data.join("outbox"),
            tools: data.join("tools"),
            config_file: config.join("config.json"),
            soulseek_file: config.join("soulseek.json"),
            jobs_file: data.join("jobs.json"),
            state_file: data.join("state.json"),
        }
    }

    pub fn create(&self) -> io::Result<()> {
        for dir in [&self.downloads, &self.outbox, &self.tools] {
            fs::create_dir_all(dir)?;
        }
        if let Some(parent) = self.config_file.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

pub fn read_json_checked<T: DeserializeOwned + Default>(path: &Path) -> (T, bool) {
    let result =
        fs::read(path).and_then(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other));
    match result {
        Ok(value) => (value, true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (T::default(), true),
        Err(e) => {
            let bad = free_backup_name(path);
            eprintln!("sansa-dl: could not read {}: {e}", path.display());
            if let Err(rename_error) = fs::rename(path, &bad) {
                eprintln!(
                    "sansa-dl: could not preserve {}: {rename_error}",
                    path.display()
                );
            }
            (T::default(), false)
        }
    }
}

/// `x.json.bad`, or `x.json.bad.2`, `.3`, ... so earlier backups are kept.
fn free_backup_name(path: &Path) -> PathBuf {
    let first = path.with_extension("json.bad");
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| path.with_extension(format!("json.bad.{n}")))
        .find(|p| !p.exists())
        .expect("a free backup name")
}

pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    read_json_checked(path).0
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temp, path)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sponsorblock: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { sponsorblock: true }
    }
}

/// Last update check per tool and for the app itself, in seconds since the epoch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub checked: BTreeMap<String, u64>,
}

const DAY: u64 = 24 * 60 * 60;

impl State {
    pub fn due(&self, key: &str, now: u64) -> bool {
        self.checked
            .get(key)
            .is_none_or(|last| now.saturating_sub(*last) >= DAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_the_given_roots() {
        let dirs = Dirs::under(Path::new("/d/sansa-dl"), Path::new("/c/sansa-dl"));
        assert_eq!(dirs.downloads, Path::new("/d/sansa-dl/downloads"));
        assert_eq!(dirs.outbox, Path::new("/d/sansa-dl/outbox"));
        assert_eq!(dirs.tools, Path::new("/d/sansa-dl/tools"));
        assert_eq!(dirs.config_file, Path::new("/c/sansa-dl/config.json"));
    }

    #[test]
    fn config_defaults_on_and_survives_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(read_json::<Config>(&path).sponsorblock);
        fs::write(&path, "not json").unwrap();
        assert!(read_json::<Config>(&path).sponsorblock);
        assert_eq!(
            fs::read_to_string(path.with_extension("json.bad")).unwrap(),
            "not json"
        );
        write_json(
            &path,
            &Config {
                sponsorblock: false,
            },
        )
        .unwrap();
        assert!(!read_json::<Config>(&path).sponsorblock);
    }

    #[test]
    fn bad_files_never_overwrite_an_earlier_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        for (n, text) in ["first", "second", "third"].iter().enumerate() {
            fs::write(&path, text).unwrap();
            let (jobs, ok) = read_json_checked::<Vec<u64>>(&path);
            assert!(!ok && jobs.is_empty(), "round {n}");
        }
        assert_eq!(fs::read(path.with_extension("json.bad")).unwrap(), b"first");
        assert_eq!(
            fs::read(path.with_extension("json.bad.2")).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(path.with_extension("json.bad.3")).unwrap(),
            b"third"
        );
    }

    #[test]
    fn checks_are_due_once_a_day() {
        let mut state = State::default();
        assert!(state.due("yt-dlp", 1000));
        state.checked.insert("yt-dlp".into(), 1000);
        assert!(!state.due("yt-dlp", 1000 + DAY - 1));
        assert!(state.due("yt-dlp", 1000 + DAY));
        assert!(state.due("deno", 1000));
    }
}
