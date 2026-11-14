//! Self update and tool staging against a local HTTP server.

use clipplus_manager_core::net;
use clipplus_manager_core::selfupdate;
use clipplus_manager_core::tools::{self, Latest, Staged, Tool};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Serves bodies by path until the test process ends. `routes` gets the
/// server's own address, so a body can point back at it.
fn serve(routes: impl FnOnce(&str) -> HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = routes(&base);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap_or(0);
            let text = String::from_utf8_lossy(&request[..n]);
            let path = text.split_whitespace().nth(1).unwrap_or("/");
            let (status, body) = match routes.get(path) {
                Some(body) => ("200 OK", body.as_slice()),
                None => ("404 Not Found", &[][..]),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    base
}

fn sha(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    fs::write(&path, bytes).unwrap();
    net::sha256_file(&path).unwrap()
}

const NEW_APP: &[u8] = b"new appimage bytes";

fn serve_release(tag: &str, checksum: &str) -> String {
    serve(|base| {
        let name = selfupdate::asset_name(tag.trim_start_matches('v'));
        let release = format!(
            r#"{{"tag_name":"{tag}","assets":[
                {{"name":"{name}","browser_download_url":"{base}/app"}},
                {{"name":"{name}.sha256","browser_download_url":"{base}/sum"}}]}}"#
        );
        HashMap::from([
            ("/release".to_string(), release.into_bytes()),
            ("/app".to_string(), NEW_APP.to_vec()),
            (
                "/sum".to_string(),
                format!("{checksum}  {name}\n").into_bytes(),
            ),
        ])
    })
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_newer_release_replaces_the_appimage() {
    let base = serve_release("v0.2.5", &sha(NEW_APP));
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("clipplus-manager.AppImage");
    fs::write(&app, b"old").unwrap();

    let updated = selfupdate::update_from(
        &net::agent(),
        &format!("{base}/release"),
        &app,
        "0.1.9",
        &|_| {},
    )
    .unwrap();

    assert_eq!(updated.as_deref(), Some("0.2.5"));
    assert_eq!(fs::read(&app).unwrap(), NEW_APP);
    assert_eq!(
        fs::metadata(&app).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        files_in(dir.path()),
        ["clipplus-manager.AppImage"],
        "no temp file is left"
    );
}

#[test]
fn the_current_release_changes_nothing() {
    let base = serve_release("v0.1.9", &sha(NEW_APP));
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("clipplus-manager.AppImage");
    fs::write(&app, b"old").unwrap();
    let updated = selfupdate::update_from(
        &net::agent(),
        &format!("{base}/release"),
        &app,
        "0.1.9",
        &|_| {},
    )
    .unwrap();
    assert_eq!(updated, None);
    assert_eq!(fs::read(&app).unwrap(), b"old");
}

#[test]
fn a_bad_checksum_keeps_the_old_appimage() {
    let base = serve_release("v0.2.5", &sha(b"something else"));
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("clipplus-manager.AppImage");
    fs::write(&app, b"old").unwrap();
    let err = selfupdate::update_from(
        &net::agent(),
        &format!("{base}/release"),
        &app,
        "0.1.9",
        &|_| {},
    )
    .unwrap_err();
    assert!(err.contains("checksum"));
    assert_eq!(fs::read(&app).unwrap(), b"old");
    assert_eq!(files_in(dir.path()), ["clipplus-manager.AppImage"]);
}

const FAKE_YTDLP: &[u8] = b"#!/bin/sh\necho 2099.01.01\n";

#[test]
fn a_tool_is_staged_verified_and_installed() {
    let base = serve(|_| HashMap::from([("/yt-dlp".to_string(), FAKE_YTDLP.to_vec())]));
    let tools_dir = tempfile::tempdir().unwrap();
    let latest = Latest {
        version: "2099.01.01".into(),
        url: format!("{base}/yt-dlp"),
        sha256: sha(FAKE_YTDLP),
    };
    let staged = tools::stage(&net::agent(), Tool::Ytdlp, &latest, tools_dir.path()).unwrap();
    assert!(staged.dir.join("yt-dlp").is_file());
    assert!(
        !tools_dir.path().join("yt-dlp").exists(),
        "nothing is swapped yet"
    );

    tools::install(&staged, tools_dir.path()).unwrap();
    let resolved = tools::Resolved::load(tools_dir.path(), None);
    assert_eq!(resolved.version(Tool::Ytdlp), Some("2099.01.01"));
    assert_eq!(resolved.ytdlp().unwrap(), tools_dir.path().join("yt-dlp"));
}

#[test]
fn a_tool_with_a_wrong_checksum_is_rejected() {
    let base = serve(|_| HashMap::from([("/yt-dlp".to_string(), FAKE_YTDLP.to_vec())]));
    let tools_dir = tempfile::tempdir().unwrap();
    let latest = Latest {
        version: "2099.01.01".into(),
        url: format!("{base}/yt-dlp"),
        sha256: sha(b"other"),
    };
    let err: String = tools::stage(&net::agent(), Tool::Ytdlp, &latest, tools_dir.path())
        .map(|s: Staged| s.version)
        .unwrap_err();
    assert!(err.contains("checksum"));
    assert!(
        files_in(tools_dir.path()).is_empty(),
        "the staging folder is removed"
    );
}

#[test]
fn a_tool_that_does_not_run_is_rejected() {
    let broken: &[u8] = b"#!/bin/sh\nexit 1\n";
    let base = serve(|_| HashMap::from([("/deno".to_string(), broken.to_vec())]));
    let tools_dir = tempfile::tempdir().unwrap();
    let latest = Latest {
        version: "1.0.0".into(),
        url: format!("{base}/deno"),
        sha256: sha(broken),
    };
    let err = tools::stage(&net::agent(), Tool::Ytdlp, &latest, tools_dir.path())
        .err()
        .unwrap();
    assert!(err.contains("does not run"), "{err}");
    assert!(files_in(tools_dir.path()).is_empty());
}

#[test]
fn a_non_zip_deno_download_is_rejected() {
    let broken: &[u8] = b"not a zip";
    let base = serve(|_| HashMap::from([("/deno".to_string(), broken.to_vec())]));
    let tools_dir = tempfile::tempdir().unwrap();
    let latest = Latest {
        version: "1.0.0".into(),
        url: format!("{base}/deno"),
        sha256: sha(broken),
    };
    assert!(tools::stage(&net::agent(), Tool::Deno, &latest, tools_dir.path()).is_err());
    assert!(files_in(tools_dir.path()).is_empty());
}
