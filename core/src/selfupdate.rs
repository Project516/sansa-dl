//! Updates the AppImage from this project's latest GitHub release.

use crate::{net, tools};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const CURRENT: &str = match option_env!("SANSA_DL_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};

const RELEASE: &str = "https://api.github.com/repos/Project516/sansa-dl/releases/latest";

pub fn asset_name(version: &str) -> String {
    format!("sansa-dl-{version}-x86_64.AppImage")
}

/// Replaces `appimage` with a newer verified release. Returns the new version,
/// or None when this one is current.
pub fn update(
    agent: &ureq::Agent,
    appimage: &Path,
    current: &str,
) -> Result<Option<String>, String> {
    update_from(agent, RELEASE, appimage, current)
}

pub fn update_from(
    agent: &ureq::Agent,
    release_url: &str,
    appimage: &Path,
    current: &str,
) -> Result<Option<String>, String> {
    let release = net::get_json(agent, release_url)?;
    let version = release
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or("release has no tag")?
        .trim_start_matches('v')
        .to_string();
    if !tools::is_newer(&version, current) {
        return Ok(None);
    }
    let name = asset_name(&version);
    let sums_name = format!("{name}.sha256");
    let (_, url, sums_url) = tools::release_assets(&release, &name, &sums_name)
        .ok_or_else(|| format!("release has no {name}"))?;
    let expected = tools::find_sha(&net::get_text(agent, &sums_url)?, None)
        .ok_or("release has no checksum")?;

    // Same folder as the target, so the final rename is atomic.
    let mut random = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .map_err(|e| e.to_string())?;
    let temp = appimage.with_file_name(format!(".sansa-dl-update-{}.part", net::hex(&random)));
    let mut created = false;
    let result = (|| {
        let got = net::download(agent, &url, &temp)?;
        created = true;
        if got != expected {
            return Err("update checksum mismatch".to_string());
        }
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        fs::rename(&temp, appimage).map_err(|e| e.to_string())
    })();
    if created {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| Some(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_follow_the_release_convention() {
        assert_eq!(asset_name("0.1.7"), "sansa-dl-0.1.7-x86_64.AppImage");
    }

    #[test]
    fn current_version_is_set() {
        assert!(!CURRENT.is_empty());
    }
}
