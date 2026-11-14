//! Blocking HTTP helpers.

use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("clipplus-manager/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
        // A stalled connection otherwise holds the update check for the full body timeout.
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
}

pub fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent
        .get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))
}

pub fn get_json(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value, String> {
    let text = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))
}

/// Writes the body to `dest` and returns its SHA-256 as lowercase hex.
pub fn download(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<String, String> {
    fetch(agent, url, dest, false)
}

/// Like `download`, and fails when the body is shorter than its Content-Length.
pub fn download_checked(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<String, String> {
    fetch(agent, url, dest, true)
}

fn fetch(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    check_length: bool,
) -> Result<String, String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let mut response = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        let expected = response.body().content_length();
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut written = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        if check_length && expected.is_some_and(|n| n != written) {
            return Err(format!("{url}: the download was cut short"));
        }
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(hex(&hasher.finalize()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_does_not_follow_existing_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, b"safe").unwrap();
        symlink(&target, &link).unwrap();
        assert!(download(&agent(), "http://127.0.0.1:1/", &link).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"safe");
        assert!(link.is_symlink());
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
