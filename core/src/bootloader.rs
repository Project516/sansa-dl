//! Installs the Rockbox bootloader on a Clip+ by writing a patched original
//! firmware (`clppa.bin`) to the player. The player flashes it after unplug.

use crate::{amsboot, net, transfer};
use rustix::fs::{Advice, fadvise};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use zip::ZipArchive;

const BOOTLOADER_URL: &str =
    "https://download.rockbox.org/bootloader/sandisk-sansa/clipplus/bootloader-clipplus.sansa";
const BOOTLOADER_MD5: &str = "dbb6f832c8a911273be0b0455e8378d0";
const OF_FILE: &str = "clppa.bin";
const MAX_OF_SIZE: u64 = 32 << 20;
const FREE_MARGIN: u64 = 1 << 20;

struct FirmwareZip {
    name: &'static str,
    url: &'static str,
    md5: &'static str,
    version: &'static str,
}

const STOCK: FirmwareZip = FirmwareZip {
    name: "clipplus01.02.18.zip",
    url: "https://download.rockbox.org/sansa_fw/clipplus01.02.18.zip",
    md5: "0de048e8d98c20f1c8e1f5728b526f38",
    version: "01.02.18",
};

/// True while `clppa.bin` is still in the root, so the player has not flashed it yet.
pub fn pending(internal_root: &Path) -> bool {
    internal_root.join(OF_FILE).exists()
}

/// Writes `clppa.bin` with the Rockbox bootloader patched in.
pub fn install(
    agent: &ureq::Agent,
    cache: &Path,
    internal_root: &Path,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let of = stock_firmware(agent, cache, &STOCK, progress)?;
    progress("Downloading the Rockbox bootloader");
    let bootloader = cached(
        agent,
        cache,
        "bootloader-clipplus.sansa",
        BOOTLOADER_URL,
        BOOTLOADER_MD5,
    )?;
    progress("Patching the firmware");
    let patched = amsboot::patch(&of, &bootloader)?;
    write_to_player(internal_root, &patched, progress)
}

/// Writes the unmodified original firmware, to go back from Rockbox.
pub fn restore_stock(
    agent: &ureq::Agent,
    cache: &Path,
    internal_root: &Path,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let of = stock_firmware(agent, cache, &STOCK, progress)?;
    write_to_player(internal_root, &of, progress)
}

fn stock_firmware(
    agent: &ureq::Agent,
    cache: &Path,
    zip: &FirmwareZip,
    progress: &dyn Fn(&str),
) -> Result<Vec<u8>, String> {
    progress("Downloading the original firmware");
    let archive = cached(agent, cache, zip.name, zip.url, zip.md5)?;
    let of = extract_of(&archive)?;
    if amsboot::of_version(&of) != Some(zip.version) {
        return Err("the firmware archive holds an unexpected file".into());
    }
    Ok(of)
}

fn extract_of(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut zip = ZipArchive::new(Cursor::new(archive)).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let is_of = entry
            .enclosed_name()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.eq_ignore_ascii_case(OF_FILE))
            })
            .unwrap_or(false);
        if !is_of {
            continue;
        }
        let mut data = Vec::new();
        (&mut entry)
            .take(MAX_OF_SIZE + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() as u64 > MAX_OF_SIZE {
            return Err("the firmware file is too large".into());
        }
        return Ok(data);
    }
    Err(format!("{OF_FILE} is not in the firmware archive"))
}

/// Returns the file's bytes, downloading it when the cached copy is missing or wrong.
fn cached(
    agent: &ureq::Agent,
    cache: &Path,
    name: &str,
    url: &str,
    md5: &str,
) -> Result<Vec<u8>, String> {
    let path = cache.join(name);
    if let Ok(data) = fs::read(&path) {
        if amsboot::md5_hex(&data) == md5 {
            return Ok(data);
        }
        let _ = fs::remove_file(&path);
    }
    fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let part = cache.join(format!("{name}.part"));
    let _ = fs::remove_file(&part);
    net::download(agent, url, &part)?;
    let data = fs::read(&part).map_err(|e| e.to_string())?;
    if amsboot::md5_hex(&data) != md5 {
        let _ = fs::remove_file(&part);
        return Err(format!("{name}: the download does not match its checksum"));
    }
    fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(data)
}

fn write_to_player(root: &Path, data: &[u8], progress: &dyn Fn(&str)) -> Result<(), String> {
    let need = data.len() as u64 + FREE_MARGIN;
    if transfer::free_space(root).is_some_and(|free| free < need) {
        return Err("There is not enough free space on the player".into());
    }
    progress("Writing the firmware to the player");
    let temp = root.join(format!(".{OF_FILE}.part"));
    let target = root.join(OF_FILE);
    let result = write_and_check(root, &temp, &target, data);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_and_check(root: &Path, temp: &Path, target: &Path, data: &[u8]) -> Result<(), String> {
    let _ = fs::remove_file(temp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(temp, target).map_err(|e| e.to_string())?;
    File::open(root)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;

    let mut written = File::open(target).map_err(|e| e.to_string())?;
    let _ = fadvise(&written, 0, None, Advice::DontNeed);
    let mut back = Vec::with_capacity(data.len());
    written.read_to_end(&mut back).map_err(|e| e.to_string())?;
    if amsboot::md5_hex(&back) != amsboot::md5_hex(data) {
        let _ = fs::remove_file(target);
        return Err("The file on the player does not match, it was removed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn writes_and_detects_pending() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!pending(dir.path()));
        write_to_player(dir.path(), b"firmware", &|_| {}).unwrap();
        assert!(pending(dir.path()));
        assert_eq!(fs::read(dir.path().join(OF_FILE)).unwrap(), b"firmware");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(OF_FILE), b"old").unwrap();
        write_to_player(dir.path(), b"new", &|_| {}).unwrap();
        assert_eq!(fs::read(dir.path().join(OF_FILE)).unwrap(), b"new");
    }

    #[test]
    fn extracts_the_firmware_by_name() {
        let zip = archive(&[("readme.txt", b"hi"), ("Dir/CLPPA.BIN", b"data")]);
        assert_eq!(extract_of(&zip).unwrap(), b"data");
        let none = archive(&[("readme.txt", b"hi")]);
        assert!(extract_of(&none).unwrap_err().contains("not in"));
        assert!(extract_of(b"not a zip").is_err());
    }

    #[test]
    fn uses_a_cached_file_with_the_right_checksum() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f"), b"abc").unwrap();
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        let got = cached(&net::agent(), dir.path(), "f", "http://127.0.0.1:1/", md5).unwrap();
        assert_eq!(got, b"abc");
    }

    #[test]
    fn drops_a_cached_file_with_the_wrong_checksum() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f"), b"abd").unwrap();
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        assert!(cached(&net::agent(), dir.path(), "f", "http://127.0.0.1:1/", md5).is_err());
        assert!(!dir.path().join("f").exists());
    }

    #[test]
    #[ignore = "downloads about 30 MB"]
    fn golden_patch_matches_mkamsboot() {
        let dir = tempfile::tempdir().unwrap();
        let agent = net::agent();
        let bootloader = cached(
            &agent,
            dir.path(),
            "bootloader-clipplus.sansa",
            BOOTLOADER_URL,
            BOOTLOADER_MD5,
        )
        .unwrap();
        let old = FirmwareZip {
            name: "clipplus01.02.16.zip",
            url: "https://download.rockbox.org/sansa_fw/clipplus01.02.16.zip",
            md5: "f7f1f92ea02de36de5848cf22ab7f93f",
            version: "01.02.16",
        };
        for (zip, expected) in [
            (&old, "cce6bec0a11c75ece397e6d02e5ffd16"),
            (&STOCK, "0234d17611f42c6e64e57eae8d751b89"),
        ] {
            let of = stock_firmware(&agent, dir.path(), zip, &|_| {}).unwrap();
            let patched = amsboot::patch(&of, &bootloader).unwrap();
            assert_eq!(amsboot::md5_hex(&patched), expected, "{}", zip.version);
        }
    }
}
