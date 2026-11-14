//! Launcher entry and icon for an AppImage, so Plasma lists the app.

use crate::paths::APP_ID;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ICON: &str = include_str!("../../data/icons/dev.project516.clipplus-manager.svg");

/// Quotes a path for a desktop entry Exec line.
fn quote(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            // Exec escapes a backslash once, and the entry file escapes it again.
            '\\' => out.push_str("\\\\\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn desktop_entry(appimage: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=ClipPlus Manager\n\
         GenericName=Music Downloader\n\
         Comment=Download music and copy it to a Sansa Clip+\n\
         Exec={} %U\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;Network;Qt;KDE;\n\
         StartupWMClass={APP_ID}\n\
         MimeType=\n",
        quote(&appimage.to_string_lossy())
    )
}

fn write_if_changed(path: &Path, content: &str) -> io::Result<bool> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, content)?;
    fs::rename(&temp, path)?;
    Ok(true)
}

/// Writes the launcher entry and icon under `data_home` when they differ.
pub fn install(appimage: &Path, data_home: &Path) -> io::Result<()> {
    if appimage.to_string_lossy().chars().any(char::is_control) {
        eprintln!("clipplus-manager: launcher path contains a control character");
        return Ok(());
    }
    let apps = data_home.join("applications");
    let entry = write_if_changed(
        &apps.join(format!("{APP_ID}.desktop")),
        &desktop_entry(appimage),
    )?;
    let icon = write_if_changed(
        &data_home
            .join("icons/hicolor/scalable/apps")
            .join(format!("{APP_ID}.svg")),
        ICON,
    )?;
    if entry || icon {
        // Plasma watches the folder; the database only matters for other desktops.
        let _ = Command::new("update-desktop-database")
            .arg(&apps)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

pub fn data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_has_the_required_keys() {
        let entry = desktop_entry(Path::new("/opt/ClipPlus Manager.AppImage"));
        assert!(entry.contains("Exec=\"/opt/ClipPlus Manager.AppImage\" %U\n"));
        assert!(entry.contains("Icon=dev.project516.clipplus-manager\n"));
        assert!(entry.contains("StartupWMClass=dev.project516.clipplus-manager\n"));
        assert!(entry.contains("Categories=AudioVideo;Audio;Network;Qt;KDE;\n"));
        assert!(entry.contains("MimeType=\n"));
        assert!(entry.contains("GenericName=Music Downloader\n"));
    }

    #[test]
    fn exec_quoting_escapes_special_characters() {
        assert_eq!(quote("/a b/c"), "\"/a b/c\"");
        assert_eq!(quote("/a\"b$c`d"), "\"/a\\\"b\\$c\\`d\"");
        assert_eq!(quote("/a%b"), "\"/a%%b\"");
        assert_eq!(quote("/a\\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn install_skips_control_characters() {
        let dir = tempfile::tempdir().unwrap();
        install(Path::new("/bad\npath"), dir.path()).unwrap();
        assert!(!dir.path().join("applications").exists());
    }

    #[test]
    fn install_writes_once() {
        let dir = tempfile::tempdir().unwrap();
        let app = Path::new("/opt/clipplus-manager.AppImage");
        install(app, dir.path()).unwrap();
        let entry = dir
            .path()
            .join("applications/dev.project516.clipplus-manager.desktop");
        let icon = dir
            .path()
            .join("icons/hicolor/scalable/apps/dev.project516.clipplus-manager.svg");
        assert!(entry.exists() && icon.exists());
        let first = fs::metadata(&entry).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        install(app, dir.path()).unwrap();
        assert_eq!(fs::metadata(&entry).unwrap().modified().unwrap(), first);
        install(Path::new("/elsewhere.AppImage"), dir.path()).unwrap();
        assert!(
            fs::read_to_string(&entry)
                .unwrap()
                .contains("/elsewhere.AppImage")
        );
    }
}
