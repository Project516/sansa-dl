//! Installs immutable slskd releases so running instances keep their files.

use crate::{net, tools};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const RELEASE_URL: &str = "https://api.github.com/repos/slskd/slskd/releases/latest";
const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct Installed {
    pub version: String,
    pub executable: PathBuf,
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_release(json: &Value) -> Result<Release, String> {
    let version = json
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_start_matches('v');
    if !valid_version(version) {
        return Err("slskd: invalid release version".into());
    }
    let name = format!("slskd-{version}-linux-x64.zip");
    let asset = json
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(&name))
        .ok_or_else(|| "slskd: release has no Linux x64 bundle".to_string())?;
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://github.com/slskd/slskd/releases/download/"))
        .ok_or_else(|| "slskd: invalid bundle URL".to_string())?;
    let sha256 = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "slskd: release has no SHA-256 digest".to_string())?;
    Ok(Release {
        version: version.into(),
        url: url.into(),
        sha256: sha256.to_ascii_lowercase(),
    })
}

pub fn fetch_latest(agent: &ureq::Agent) -> Result<Release, String> {
    parse_release(&net::get_json(agent, RELEASE_URL)?)
}

/// Finds the newest installed bundle. Installed directories are never changed.
pub fn find(root: &Path) -> Option<Installed> {
    let mut best: Option<Installed> = None;
    for entry in fs::read_dir(root).ok()?.flatten() {
        let version = entry.file_name().to_string_lossy().into_owned();
        if !valid_version(&version) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let executable = entry.path().join("slskd");
        if !fs::metadata(&executable)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|b| tools::is_newer(&version, &b.version))
        {
            best = Some(Installed {
                version,
                executable,
            });
        }
    }
    best
}

struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stage_dir(root: &Path) -> Result<Stage, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".stage-{}-{n}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(Stage(path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn unpack(archive: &Path, dest: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| "slskd: unsafe archive path".to_string())?;
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("slskd: unsafe archive path".into());
        }
        let mode = entry.unix_mode().unwrap_or(0);
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            return Err("slskd: archive contains a special file".into());
        }
        total = total
            .checked_add(entry.size())
            .filter(|n| *n <= MAX_UNPACKED)
            .ok_or_else(|| "slskd: archive is too large".to_string())?;
        let path = dest.join(path);
        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            io::copy(&mut entry, &mut output).map_err(|e| e.to_string())?;
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Fetches a checked bundle into a new directory. The old bundle is left intact.
pub fn install(agent: &ureq::Agent, release: &Release, root: &Path) -> Result<Installed, String> {
    if !valid_version(&release.version) {
        return Err("slskd: invalid release version".into());
    }
    let stage = stage_dir(root)?;
    let archive = stage.0.join("bundle.zip");
    let got = net::download(agent, &release.url, &archive)?;
    if got != release.sha256 {
        return Err("slskd: checksum mismatch".into());
    }
    let bundle = stage.0.join("bundle");
    fs::create_dir(&bundle).map_err(|e| e.to_string())?;
    unpack(&archive, &bundle)?;
    let executable = bundle.join("slskd");
    if !fs::metadata(&executable).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    {
        return Err("slskd: bundle has no executable".into());
    }
    let output = Command::new(&executable)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("slskd: {e}"))?;
    if !output.status.success() {
        return Err("slskd: executable failed its version check".into());
    }
    let dest = root.join(&release.version);
    match renameat_with(CWD, &bundle, CWD, &dest, RenameFlags::NOREPLACE) {
        Ok(()) => (),
        Err(e) if e == rustix::io::Errno::EXIST => {
            // A concurrent installer won; only use a complete published bundle.
            if !dest.join("slskd").is_file() {
                return Err("slskd: installed bundle is incomplete".into());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(Installed {
        version: release.version.clone(),
        executable: dest.join("slskd"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn release_needs_named_bundle_and_valid_digest() {
        let value = serde_json::json!({"tag_name":"0.26.2","assets":[{"name":"slskd-0.26.2-linux-x64.zip","browser_download_url":"https://github.com/slskd/slskd/releases/download/0.26.2/a.zip","digest":format!("sha256:{}", "a".repeat(64))}]});
        let release = parse_release(&value).unwrap();
        assert_eq!(release.version, "0.26.2");
        assert!(parse_release(&serde_json::json!({"tag_name":"../bad"})).is_err());
        let mut bad = value;
        bad["assets"][0]["digest"] = "sha256:bad".into();
        assert!(parse_release(&bad).is_err());
    }

    #[test]
    fn extracts_complete_bundle_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("test.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file(
            "slskd",
            SimpleFileOptions::default().unix_permissions(0o755),
        )
        .unwrap();
        zip.write_all(b"binary").unwrap();
        zip.start_file("config/example.yml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"config").unwrap();
        zip.finish().unwrap();
        let bundle = dir.path().join("bundle");
        fs::create_dir(&bundle).unwrap();
        unpack(&archive, &bundle).unwrap();
        assert_eq!(
            fs::read(bundle.join("config/example.yml")).unwrap(),
            b"config"
        );
        assert_ne!(
            fs::metadata(bundle.join("slskd"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );

        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file("../escape", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"bad").unwrap();
        zip.finish().unwrap();
        assert!(unpack(&archive, &bundle).is_err());
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn install_does_not_replace_a_running_version() {
        use std::io::Read;
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("fixture.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file(
            "slskd",
            SimpleFileOptions::default().unix_permissions(0o755),
        )
        .unwrap();
        zip.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        zip.start_file("wwwroot/index.html", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"web").unwrap();
        zip.finish().unwrap();
        let bytes = fs::read(&archive).unwrap();
        let hash = net::sha256_file(&archive).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let root = dir.path().join("installed");
        fs::create_dir_all(root.join("1.0")).unwrap();
        fs::write(root.join("1.0/slskd"), b"old").unwrap();
        let release = Release {
            version: "2.0".into(),
            url: format!("http://{address}/bundle.zip"),
            sha256: hash,
        };
        let installed = install(&net::agent(), &release, &root).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(root.join("1.0/slskd")).unwrap(), b"old");
        assert_eq!(
            fs::read(
                installed
                    .executable
                    .parent()
                    .unwrap()
                    .join("wwwroot/index.html")
            )
            .unwrap(),
            b"web"
        );
        assert_eq!(find(&root).unwrap().version, "2.0");
    }

    #[test]
    fn find_keeps_old_bundles_and_selects_newest() {
        let dir = tempfile::tempdir().unwrap();
        for version in ["0.9.0", "0.10.0"] {
            let path = dir.path().join(version);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("slskd"), b"binary").unwrap();
            fs::set_permissions(path.join("slskd"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find(dir.path()).unwrap().version, "0.10.0");
        assert!(dir.path().join("0.9.0/slskd").exists());
    }
}
