//! Rockbox on the Clip+ internal drive: finds builds, installs them the way
//! Rockbox Utility does, and removes them again. The install log
//! (`.rockbox/rbutil.log`) stays readable by Rockbox Utility.

use crate::{net, transfer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

pub const TARGET: &str = "sansaclipplus";
const MODEL_NUMBER: u32 = 66;
const BUILD_INFO_URL: &str = "https://download.rockbox.org/build-info";
const THEMES_HOST: &str = "https://themes.rockbox.org";
const RBUTIL_VERSION: &str = "1.5.1";
const BASE_SECTION: &str = "Rockbox (Base)";
const BOOTLOADER_SECTION: &str = "Bootloader";
const BINARY: &str = ".rockbox/rockbox.sansa";
const INFO_FILE: &str = ".rockbox/rockbox-info.txt";
const LOG_FILE: &str = ".rockbox/rbutil.log";
const CONFIG_FILE: &str = ".rockbox/config.cfg";
const SPACE_MARGIN: u64 = 1_000_000;
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
const PART: &str = "download.zip.part";
const CURRENT: &str = "current.zip";
const PREVIOUS: &str = "previous.zip";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Dev,
    Daily,
    Release,
}

impl Channel {
    fn section(self) -> &'static str {
        match self {
            Self::Dev => "development",
            Self::Daily => "daily",
            Self::Release => "release",
        }
    }
}

// build-info -----------------------------------------------------------------

type Ini = BTreeMap<String, BTreeMap<String, String>>;

fn unquote(value: &str) -> String {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => value.to_string(),
    }
}

/// Qt INI: `[section]`, `key=value`, optional quotes, `;` comments.
fn parse_ini(text: &str) -> Ini {
    let mut ini = Ini::new();
    let mut section = String::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            ini.entry(section.clone()).or_default();
        } else if let Some((key, value)) = line.split_once('=') {
            ini.entry(section.clone())
                .or_default()
                .insert(key.trim().to_string(), unquote(value.trim()));
        }
    }
    ini
}

fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .fold(template.to_string(), |text, (key, value)| {
            text.replace(&format!("%{key}%"), value)
        })
}

/// A build value is a version, optionally followed by a comma and its own URL.
fn split_version(value: &str) -> (&str, Option<&str>) {
    match value.split_once(',') {
        Some((version, url)) => (version.trim(), Some(url.trim()).filter(|u| !u.is_empty())),
        None => (value.trim(), None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub channel: Channel,
    /// What the user sees: a git hash, a date, or a release number.
    pub version: String,
    /// What is compared with the installed build: a hash or a release number.
    pub compare: String,
    pub url: String,
    pub fonts_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct BuildInfo {
    ini: Ini,
}

impl BuildInfo {
    pub fn parse(text: &str) -> Self {
        Self {
            ini: parse_ini(text),
        }
    }

    fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.ini.get(section)?.get(key).map(String::as_str)
    }

    fn target_value(&self, channel: Channel) -> Result<(&str, Option<&str>), String> {
        let value = self.get(channel.section(), TARGET).ok_or_else(|| {
            format!(
                "build-info has no {} build for the Clip+",
                channel.section()
            )
        })?;
        Ok(split_version(value))
    }

    fn template(&self, channel: Channel, key: &str) -> Result<&str, String> {
        self.get(channel.section(), key)
            .ok_or_else(|| format!("build-info has no {key} for {}", channel.section()))
    }

    pub fn build(&self, channel: Channel) -> Result<Build, String> {
        let (version, own_url) = self.target_value(channel)?;
        let vars = [("TARGET", TARGET), ("VERSION", version)];
        let url = match own_url {
            Some(url) => url.to_string(),
            None => fill(self.template(channel, "build_url")?, &vars),
        };
        let compare = match channel {
            Channel::Daily => self
                .get("dailies", "rev")
                .ok_or("build-info has no daily revision")?,
            _ => version,
        };
        // Dev builds have no fonts of their own, so they use the daily ones.
        let (font_channel, font_version) = match channel {
            Channel::Dev => (Channel::Dev, self.target_value(Channel::Daily)?.0),
            _ => (channel, version),
        };
        let fonts_url = fill(
            self.template(font_channel, "font_url")?,
            &[("TARGET", TARGET), ("VERSION", font_version)],
        );
        Ok(Build {
            channel,
            version: version.into(),
            compare: compare.into(),
            url,
            fonts_url,
        })
    }

    pub fn voice_url(&self, channel: Channel, language: &str) -> Result<String, String> {
        if channel == Channel::Dev {
            return Err("Dev builds have no voice file. Use the daily or release channel.".into());
        }
        let (version, _) = self.target_value(channel)?;
        Ok(fill(
            self.template(channel, "voice_url")?,
            &[
                ("TARGET", TARGET),
                ("VERSION", version),
                ("LANGUAGE", language),
            ],
        ))
    }
}

/// Downloads `build-info`, revalidating the cached copy by ETag.
pub fn fetch_build_info(agent: &ureq::Agent, cache: &Path) -> Result<BuildInfo, String> {
    let body_file = cache.join("build-info");
    let etag_file = cache.join("build-info.etag");
    let cached = fs::read_to_string(&body_file).ok();
    let etag = fs::read_to_string(&etag_file)
        .ok()
        .filter(|_| cached.is_some());
    let mut request = agent.get(BUILD_INFO_URL);
    if let Some(etag) = &etag {
        request = request.header("If-None-Match", etag.trim());
    }
    let mut response = request
        .call()
        .map_err(|e| format!("{BUILD_INFO_URL}: {e}"))?;
    if response.status().as_u16() == 304
        && let Some(text) = cached
    {
        return Ok(BuildInfo::parse(&text));
    }
    let new_etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{BUILD_INFO_URL}: {e}"))?;
    let _ = fs::create_dir_all(cache);
    let _ = fs::write(&body_file, &text);
    match new_etag {
        Some(tag) => {
            let _ = fs::write(&etag_file, tag);
        }
        None => {
            let _ = fs::remove_file(&etag_file);
        }
    }
    Ok(BuildInfo::parse(&text))
}

// Installed build --------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub target: String,
    pub version: String,
    pub binary_size: Option<u64>,
}

/// Reads `.rockbox/rockbox-info.txt`: `Key: value` lines.
pub fn parse_info(text: &str) -> Info {
    let mut info = Info::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Target" => info.target = value.into(),
            "Version" => info.version = value.into(),
            "Binary size" => info.binary_size = value.parse().ok(),
            _ => {}
        }
    }
    info
}

/// A release is its version string. Anything else is a revision: the text
/// before the first dash, without a leading `r`.
pub fn installed_id(version: &str) -> String {
    let version = version.trim();
    if version.contains('.') {
        return version.into();
    }
    let revision = version.split('-').next().unwrap_or_default();
    revision.strip_prefix('r').unwrap_or(revision).into()
}

/// Hashes have no order, so any difference counts.
pub fn update_available(installed_version: &str, latest: &Build) -> bool {
    let (have, want) = (installed_id(installed_version), latest.compare.as_str());
    let same_hash =
        have.len() >= 7 && want.len() >= 7 && (have.starts_with(want) || want.starts_with(&have));
    !(have == want || same_hash)
}

/// The boot file check the bootloader makes: `cli+` and a checksum.
pub fn check_binary(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 8 || &bytes[4..8] != b"cli+" {
        return Err("rockbox.sansa is not a Clip+ build".into());
    }
    let sum = bytes[8..]
        .iter()
        .fold(MODEL_NUMBER, |sum, b| sum.wrapping_add(u32::from(*b)));
    if u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != sum {
        return Err("rockbox.sansa has a bad checksum".into());
    }
    Ok(())
}

// Install log ------------------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Qt `QSettings` key escaping: `/` becomes a backslash, odd characters
/// become `%XX` or `%UXXXX`.
fn encode_key(text: &str) -> String {
    let mut out = String::new();
    for unit in text.encode_utf16() {
        match u8::try_from(unit).map(char::from) {
            Ok('/') => out.push('\\'),
            Ok(c) if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') => out.push(c),
            Ok(c) => {
                let code = c as usize;
                out.push('%');
                out.push(HEX[code / 16] as char);
                out.push(HEX[code % 16] as char);
            }
            Err(_) => out.push_str(&format!("%U{unit:04X}")),
        }
    }
    out
}

fn decode_key(text: &str) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            units.push(u16::from(b'/'));
        } else if c == '%' {
            let wide = chars.peek() == Some(&'U');
            if wide {
                chars.next();
            }
            let digits: String = (0..if wide { 4 } else { 2 })
                .map_while(|_| chars.next_if(char::is_ascii_hexdigit))
                .collect();
            match u16::from_str_radix(&digits, 16) {
                Ok(unit) if digits.len() == if wide { 4 } else { 2 } => units.push(unit),
                _ => {
                    units.extend(format!("%{}{digits}", if wide { "U" } else { "" }).encode_utf16())
                }
            }
        } else {
            let mut buf = [0u16; 2];
            units.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    String::from_utf16_lossy(&units)
}

fn encode_value(value: &str) -> String {
    let plain = !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
        && value.chars().all(|c| {
            c.is_ascii() && !c.is_ascii_control() && !matches!(c, ',' | ';' | '=' | '"' | '\\')
        });
    if plain {
        return value.into();
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\x{unit:x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

#[derive(Debug, Clone, Default)]
struct LogSection {
    name: String,
    raw_name: String,
    /// Raw `key=value` text, so sections nobody touches are written back as read.
    entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
struct RbLog {
    sections: Vec<LogSection>,
}

impl RbLog {
    fn parse(text: &str) -> Self {
        let mut log = Self::default();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some(raw) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                log.sections.push(LogSection {
                    name: decode_key(raw),
                    raw_name: raw.into(),
                    entries: Vec::new(),
                });
            } else if let Some((key, value)) = line.split_once('=') {
                if log.sections.is_empty() {
                    log.sections.push(LogSection::default());
                }
                if let Some(section) = log.sections.last_mut() {
                    section.entries.push((key.into(), value.into()));
                }
            }
        }
        log
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            if !section.raw_name.is_empty() {
                out.push_str(&format!("[{}]\n", section.raw_name));
            }
            for (key, value) in &section.entries {
                out.push_str(&format!("{key}={value}\n"));
            }
            out.push('\n');
        }
        out
    }

    fn load(root: &Path) -> Self {
        fs::read(root.join(LOG_FILE))
            .map(|bytes| Self::parse(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let path = root.join(LOG_FILE);
        let temp = path.with_file_name(".rbutil.log.part");
        let write = || -> io::Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(self.text().as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &path)
        };
        write().map_err(|e| {
            let _ = fs::remove_file(&temp);
            format!("Could not write the install log: {e}")
        })
    }

    fn paths(&self, name: &str) -> Vec<String> {
        self.sections
            .iter()
            .filter(|s| s.name == name)
            .flat_map(|s| s.entries.iter().map(|(key, _)| decode_key(key)))
            .collect()
    }

    fn paths_outside(&self, names: &[String]) -> HashSet<String> {
        self.sections
            .iter()
            .filter(|s| !names.contains(&s.name))
            .flat_map(|s| s.entries.iter().map(|(key, _)| decode_key(key)))
            .collect()
    }

    fn names(&self) -> Vec<String> {
        self.sections
            .iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| s.name.clone())
            .collect()
    }

    fn remove(&mut self, name: &str) {
        self.sections.retain(|s| s.name != name);
    }

    /// Replaces the section, or adds it at the end.
    fn set(&mut self, name: &str, files: &[String], label: &str) {
        let entries = files
            .iter()
            .map(|path| (encode_key(path), encode_value(label)))
            .collect();
        let section = LogSection {
            name: name.into(),
            raw_name: encode_key(name),
            entries,
        };
        match self.sections.iter_mut().find(|s| s.name == name) {
            Some(existing) => *existing = section,
            None => self.sections.push(section),
        }
    }
}

// Zip install ---------------------------------------------------------------------

/// A zip path as a safe relative path, or an error.
fn safe_path(name: &str) -> Result<PathBuf, String> {
    let name = name.replace('\\', "/");
    let path = PathBuf::from(&name);
    let safe = !name.is_empty()
        && !name.contains(':')
        && path.components().all(|c| matches!(c, Component::Normal(_)));
    if safe {
        Ok(path)
    } else {
        Err(format!("The download holds an unsafe path: {name}"))
    }
}

fn rel_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

struct Item {
    index: usize,
    path: PathBuf,
    size: u64,
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<File>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(file).map_err(|e| format!("The download is not a zip file: {e}"))
}

fn plan(archive: &mut zip::ZipArchive<File>) -> Result<(Vec<Item>, u64), String> {
    let mut items = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let path = safe_path(&entry.name().map_err(|e| e.to_string())?)?;
        if entry.is_dir() {
            continue;
        }
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if kind != 0 && kind != 0o100000 {
            return Err("The download holds a special file".into());
        }
        total = total
            .checked_add(entry.size())
            .filter(|n| *n <= MAX_UNPACKED)
            .ok_or("The download is too large")?;
        items.push(Item {
            index,
            path,
            size: entry.size(),
        });
    }
    // The boot file goes last, so a cut leaves the old one working.
    items.sort_by_key(|item| rel_string(&item.path) == BINARY);
    Ok((items, total))
}

fn read_entry(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| format!("The download has no {name}"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn write_through_temp(dest: &Path, data: &mut dyn Read) -> io::Result<()> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let temp = dest.with_file_name(format!(".{name}.part"));
    let result = (|| {
        let mut file = File::create(&temp)?;
        io::copy(data, &mut file)?;
        file.sync_all()?;
        fs::rename(&temp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn sync_dir(dir: &Path) {
    if let Ok(file) = File::open(dir) {
        let _ = file.sync_all();
    }
}

/// Extracts the whole zip into `root` and returns the file paths, relative
/// and slash separated.
fn install_zip(
    zip_path: &Path,
    root: &Path,
    progress: &dyn Fn(&str),
) -> Result<Vec<String>, String> {
    let mut archive = open_zip(zip_path)?;
    let (items, total) = plan(&mut archive)?;
    let needed = total + SPACE_MARGIN;
    if let Some(free) = transfer::free_space(root)
        && free < needed
    {
        return Err(format!(
            "Not enough space on the player: {} needed, {} free",
            crate::engine::format_free(needed),
            crate::engine::format_free(free)
        ));
    }
    let mut dirs = HashSet::new();
    let mut files = Vec::with_capacity(items.len());
    let step = (items.len() / 10).max(1);
    for (n, item) in items.iter().enumerate() {
        if n % step == 0 {
            progress(&format!("Copying files {n}/{}", items.len()));
        }
        let dest = root.join(&item.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            dirs.insert(parent.to_path_buf());
        }
        let rel = rel_string(&item.path);
        let mut entry = archive.by_index(item.index).map_err(|e| e.to_string())?;
        if rel == BINARY {
            let mut bytes = Vec::with_capacity(usize::try_from(item.size).unwrap_or(0));
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            check_binary(&bytes)?;
            write_through_temp(&dest, &mut bytes.as_slice()).map_err(|e| e.to_string())?;
            if fs::read(&dest).map_err(|e| e.to_string())? != bytes {
                return Err("rockbox.sansa did not match after writing".into());
            }
        } else {
            write_through_temp(&dest, &mut entry).map_err(|e| format!("{rel}: {e}"))?;
        }
        files.push(rel);
    }
    for dir in dirs.iter().chain([&root.to_path_buf()]) {
        sync_dir(dir);
    }
    Ok(files)
}

fn tidy_dirs(root: &Path, files: &[String]) {
    let mut dirs: Vec<PathBuf> = files
        .iter()
        .flat_map(|f| {
            Path::new(f)
                .ancestors()
                .skip(1)
                .filter(|a| !a.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        let _ = fs::remove_dir(root.join(dir));
    }
}

/// Removes the files, then the folders they leave empty. Returns how many
/// files were removed.
fn delete_files(root: &Path, files: &[String]) -> usize {
    let mut removed = 0;
    let mut safe = Vec::new();
    for file in files {
        let Ok(path) = safe_path(file) else { continue };
        if fs::remove_file(root.join(&path)).is_ok() {
            removed += 1;
        }
        safe.push(rel_string(&path));
    }
    tidy_dirs(root, &safe);
    sync_dir(root);
    removed
}

fn record(root: &Path, section: &str, files: &[String], label: &str) -> Result<(), String> {
    let mut log = RbLog::load(root);
    log.set(section, files, label);
    log.save(root)
}

/// Installs a zip and records it. `replace_stale` also deletes files an
/// earlier install of the same section had and this zip lacks.
fn install_logged(
    zip_path: &Path,
    root: &Path,
    section: &str,
    label: &str,
    replace_stale: bool,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let old = RbLog::load(root).paths(section);
    let files = install_zip(zip_path, root, progress)?;
    if replace_stale {
        let new: HashSet<&String> = files.iter().collect();
        let others = RbLog::load(root).paths_outside(&[section.to_string()]);
        let stale: Vec<String> = old
            .into_iter()
            .filter(|f| !new.contains(f) && !others.contains(f))
            .collect();
        delete_files(root, &stale);
    }
    record(root, section, &files, label)
}

fn require_drive(root: &Path) -> Result<(), String> {
    if root.is_dir() {
        Ok(())
    } else {
        Err("The player is not mounted".into())
    }
}

// Builds ------------------------------------------------------------------------------

fn verify_build(zip_path: &Path) -> Result<Info, String> {
    let mut archive = open_zip(zip_path)?;
    let info = parse_info(&String::from_utf8_lossy(&read_entry(
        &mut archive,
        INFO_FILE,
    )?));
    if info.target != TARGET {
        return Err(format!("This build is for {}, not the Clip+", info.target));
    }
    let binary = read_entry(&mut archive, BINARY)?;
    if info.binary_size.is_some_and(|n| n != binary.len() as u64) {
        return Err("rockbox.sansa has the wrong size".into());
    }
    check_binary(&binary)?;
    Ok(info)
}

fn install_build_zip(
    zip_path: &Path,
    root: &Path,
    progress: &dyn Fn(&str),
) -> Result<String, String> {
    require_drive(root)?;
    let info = verify_build(zip_path)?;
    install_logged(zip_path, root, BASE_SECTION, &info.version, true, progress)?;
    Ok(info.version)
}

/// Moves a finished build zip to `current.zip` and the one before it to `previous.zip`.
fn keep_zip(work: &Path, zip_path: &Path) -> io::Result<()> {
    let (current, previous) = (work.join(CURRENT), work.join(PREVIOUS));
    if zip_path == previous {
        let swap = work.join("swap.zip.part");
        if current.exists() {
            fs::rename(&current, &swap)?;
        }
        fs::rename(&previous, &current)?;
        if swap.exists() {
            fs::rename(&swap, &previous)?;
        }
        return Ok(());
    }
    if current.exists() {
        fs::rename(&current, &previous)?;
    }
    fs::rename(zip_path, &current)
}

/// Downloads and installs a build. Returns the installed version.
pub fn install_build(
    agent: &ureq::Agent,
    work: &Path,
    root: &Path,
    build: &Build,
    progress: &dyn Fn(&str),
) -> Result<String, String> {
    require_drive(root)?;
    fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let part = work.join(PART);
    let _ = fs::remove_file(&part);
    progress(&format!("Downloading Rockbox {}", build.version));
    net::download_checked(agent, &build.url, &part)?;
    let result = install_build_zip(&part, root, progress);
    match &result {
        Ok(_) => {
            if let Err(e) = keep_zip(work, &part) {
                eprintln!("clipplus-manager: could not keep the build: {e}");
                let _ = fs::remove_file(&part);
            }
        }
        Err(_) => {
            let _ = fs::remove_file(&part);
        }
    }
    result
}

pub fn has_previous(work: &Path) -> bool {
    work.join(PREVIOUS).is_file()
}

/// Installs the build that was on the host before the last update.
pub fn install_previous(
    work: &Path,
    root: &Path,
    progress: &dyn Fn(&str),
) -> Result<String, String> {
    let zip_path = work.join(PREVIOUS);
    if !zip_path.is_file() {
        return Err("There is no earlier build to go back to".into());
    }
    progress("Installing the earlier build");
    let version = install_build_zip(&zip_path, root, progress)?;
    if let Err(e) = keep_zip(work, &zip_path) {
        eprintln!("clipplus-manager: could not swap builds: {e}");
    }
    Ok(version)
}

fn install_download(
    agent: &ureq::Agent,
    work: &Path,
    root: &Path,
    url: &str,
    section: &str,
    label: &str,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    require_drive(root)?;
    fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let part = work.join(PART);
    let _ = fs::remove_file(&part);
    progress(&format!("Downloading {section}"));
    let result = net::download_checked(agent, url, &part)
        .and_then(|_| install_logged(&part, root, section, label, false, progress));
    let _ = fs::remove_file(&part);
    result
}

pub fn install_fonts(
    agent: &ureq::Agent,
    work: &Path,
    root: &Path,
    build: &Build,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    install_download(
        agent,
        work,
        root,
        &build.fonts_url,
        "Fonts",
        &build.version,
        progress,
    )
}

pub fn install_voice(
    agent: &ureq::Agent,
    work: &Path,
    root: &Path,
    info: &BuildInfo,
    build: &Build,
    language: &str,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let url = info.voice_url(build.channel, language)?;
    let section = format!("Prerendered Voice ({language})");
    install_download(agent, work, root, &url, &section, &build.version, progress)
}

// Themes --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub author: String,
    pub size: u64,
    pub version: String,
    pub about: String,
    pub preview: String,
    pub archive: String,
}

fn decode_entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn theme_url(path: &str) -> String {
    format!("{THEMES_HOST}{}", path.replace(' ', "%20"))
}

pub fn parse_themes(text: &str) -> Result<Vec<Theme>, String> {
    let ini = parse_ini(text);
    if let Some(error) = ini.get("error")
        && error.get("code").is_some_and(|c| c != "0")
    {
        return Err(error
            .get("description")
            .cloned()
            .unwrap_or_else(|| "The theme server returned an error".into()));
    }
    let mut themes: Vec<Theme> = ini
        .iter()
        .filter(|(id, _)| !matches!(id.as_str(), "error" | "status"))
        .filter_map(|(id, fields)| {
            let get = |key: &str| {
                fields
                    .get(key)
                    .map(|v| decode_entities(v))
                    .unwrap_or_default()
            };
            let archive = get("archive");
            let name = get("name");
            if archive.is_empty() || name.is_empty() {
                return None;
            }
            let image = get("image");
            Some(Theme {
                id: id.clone(),
                name,
                author: get("author"),
                size: get("size").parse().unwrap_or(0),
                version: get("version"),
                about: get("about"),
                preview: if image.is_empty() {
                    image
                } else {
                    theme_url(&image)
                },
                archive: theme_url(&archive),
            })
        })
        .collect();
    themes.sort_by_key(|t| t.name.to_lowercase());
    Ok(themes)
}

/// Themes for the installed (or newest) version string.
pub fn list_themes(agent: &ureq::Agent, version: &str) -> Result<Vec<Theme>, String> {
    let id = installed_id(version);
    let (release, revision) = if id.contains('.') {
        (id.as_str(), "")
    } else {
        ("", id.as_str())
    };
    let url = format!(
        "{THEMES_HOST}/rbutilqt.php?target={TARGET}&release={release}&revision={revision}&rbutilver={RBUTIL_VERSION}"
    );
    parse_themes(&net::get_text(agent, &url)?)
}

pub fn install_theme(
    agent: &ureq::Agent,
    work: &Path,
    root: &Path,
    theme: &Theme,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let section = format!("Theme: {}", theme.name);
    install_download(
        agent,
        work,
        root,
        &theme.archive,
        &section,
        &theme.version,
        progress,
    )
}

// Uninstall and settings ------------------------------------------------------------------

/// Removes the listed sections: their files, unless another section also
/// lists them, and the folders that end up empty. Returns the file count.
pub fn uninstall_sections(root: &Path, sections: &[String]) -> Result<usize, String> {
    let mut log = RbLog::load(root);
    let others = log.paths_outside(sections);
    let files: Vec<String> = sections
        .iter()
        .flat_map(|s| log.paths(s))
        .filter(|f| !others.contains(f))
        .collect();
    let removed = delete_files(root, &files);
    for section in sections {
        log.remove(section);
    }
    log.save(root)?;
    Ok(removed)
}

/// Smart uninstall of everything the log lists, except the bootloader entry.
/// `full` removes the whole `.rockbox` folder, settings and database too.
pub fn uninstall(root: &Path, full: bool) -> Result<usize, String> {
    require_drive(root)?;
    if full {
        let dir = root.join(".rockbox");
        let count = walk_count(&dir);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        sync_dir(root);
        return Ok(count);
    }
    let log = RbLog::load(root);
    let sections: Vec<String> = log
        .names()
        .into_iter()
        .filter(|s| s != BOOTLOADER_SECTION)
        .collect();
    if sections.is_empty() {
        return Err("There is no Rockbox install log on the player".into());
    }
    uninstall_sections(root, &sections)
}

fn walk_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => walk_count(&e.path()),
            _ => 1,
        })
        .sum()
}

/// Sets `key: value` in a Rockbox settings file, keeping every other line and
/// the line ending style.
fn edit_setting(text: &str, key: &str, value: &str) -> String {
    let eol = if text.contains("\r\n") || text.is_empty() {
        "\r\n"
    } else {
        "\n"
    };
    let line = format!("{key}: {value}");
    let mut found = false;
    let mut lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|raw| {
            let content = raw.trim_end_matches(['\r', '\n']);
            if !found
                && content.split(':').next().is_some_and(|k| k.trim() == key)
                && content.contains(':')
            {
                found = true;
                format!("{line}{}", &raw[content.len()..])
            } else {
                raw.to_string()
            }
        })
        .collect();
    if !found {
        if let Some(last) = lines.last_mut()
            && !last.ends_with('\n')
        {
            last.push_str(eol);
        }
        lines.push(format!("{line}{eol}"));
    }
    lines.concat()
}

/// Turns the player's database auto update on or off in `config.cfg`.
pub fn set_database_autoupdate(root: &Path, on: bool) -> Result<(), String> {
    if !root.join(".rockbox").is_dir() {
        return Err("Rockbox is not installed on the player".into());
    }
    let path = root.join(CONFIG_FILE);
    let text = match fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let edited = edit_setting(&text, "tagcache_autoupdate", if on { "on" } else { "off" });
    write_through_temp(&path, &mut edited.as_bytes()).map_err(|e| e.to_string())?;
    sync_dir(&root.join(".rockbox"));
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_binary(payload: &[u8]) -> Vec<u8> {
    let sum = payload
        .iter()
        .fold(MODEL_NUMBER, |sum, b| sum.wrapping_add(u32::from(*b)));
    [&sum.to_be_bytes()[..], b"cli+", payload].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    const BUILD_INFO: &str = r#"[dailies]
timestamp="20261114"
rev="cb36cae2bc"
[daily]
build_url=https://download.rockbox.org/daily/%TARGET%/rockbox-%TARGET%-%VERSION%.zip
voice_url=https://download.rockbox.org/daily/%TARGET%/voice-%TARGET%-%VERSION%-%LANGUAGE%.zip
font_url=https://download.rockbox.org/daily/fonts/rockbox-fonts-%VERSION%.zip
sansaclipplus=20261114

[development]
build_url=https://build.rockbox.org/data/rockbox-%TARGET%.zip
; No voices currently generated for dev builds
voice_url=https://download.rockbox.org/daily/%TARGET%/voice-%TARGET%-%VERSION%-%LANGUAGE%.zip
font_url=https://download.rockbox.org/daily/fonts/rockbox-fonts-%VERSION%.zip
sansaclipplus=f9fe698319

[release]
build_url=https://download.rockbox.org/release/%VERSION%/rockbox-%TARGET%-%VERSION%.zip
voice_url=https://download.rockbox.org/release/%VERSION%/voice-%TARGET%-%VERSION%-%LANGUAGE%.zip
font_url=https://download.rockbox.org/release/%VERSION%/rockbox-fonts-%VERSION%.zip
sansaclipplus=4.0
"#;

    fn make_zip(files: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, data) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    fn build_zip(version: &str, extra: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let binary = test_binary(version.as_bytes());
        let info = format!(
            "Target: sansaclipplus\nVersion: {version}\nBinary: rockbox.sansa\nBinary size: {}\n",
            binary.len()
        );
        let mut files: Vec<(&str, &[u8])> = vec![(BINARY, &binary), (INFO_FILE, info.as_bytes())];
        files.extend_from_slice(extra);
        make_zip(&files)
    }

    fn none(_: &str) {}

    #[test]
    fn resolves_the_three_channels() {
        let info = BuildInfo::parse(BUILD_INFO);
        let dev = info.build(Channel::Dev).unwrap();
        assert_eq!(dev.version, "f9fe698319");
        assert_eq!(dev.compare, "f9fe698319");
        assert_eq!(
            dev.url,
            "https://build.rockbox.org/data/rockbox-sansaclipplus.zip"
        );
        assert_eq!(
            dev.fonts_url,
            "https://download.rockbox.org/daily/fonts/rockbox-fonts-20261114.zip"
        );
        let daily = info.build(Channel::Daily).unwrap();
        assert_eq!(daily.compare, "cb36cae2bc");
        assert_eq!(
            daily.url,
            "https://download.rockbox.org/daily/sansaclipplus/rockbox-sansaclipplus-20261114.zip"
        );
        let release = info.build(Channel::Release).unwrap();
        assert_eq!(release.compare, "4.0");
        assert_eq!(
            release.url,
            "https://download.rockbox.org/release/4.0/rockbox-sansaclipplus-4.0.zip"
        );
        assert_eq!(
            release.fonts_url,
            "https://download.rockbox.org/release/4.0/rockbox-fonts-4.0.zip"
        );
    }

    #[test]
    fn voice_exists_for_daily_and_release_only() {
        let info = BuildInfo::parse(BUILD_INFO);
        assert!(info.voice_url(Channel::Dev, "english").is_err());
        assert_eq!(
            info.voice_url(Channel::Release, "english").unwrap(),
            "https://download.rockbox.org/release/4.0/voice-sansaclipplus-4.0-english.zip"
        );
        assert_eq!(
            info.voice_url(Channel::Daily, "english").unwrap(),
            "https://download.rockbox.org/daily/sansaclipplus/voice-sansaclipplus-20261114-english.zip"
        );
    }

    #[test]
    fn a_version_can_carry_its_own_url() {
        let info = BuildInfo::parse(
            "[release]\nbuild_url=https://x/%VERSION%.zip\nsansaclipplus=4.1,https://y/own.zip\nfont_url=f\n",
        );
        let build = info.build(Channel::Release).unwrap();
        assert_eq!(build.version, "4.1");
        assert_eq!(build.url, "https://y/own.zip");
        assert!(
            BuildInfo::parse("[release]\nbuild_url=x\n")
                .build(Channel::Release)
                .is_err()
        );
    }

    #[test]
    fn reads_the_installed_version() {
        let info = parse_info(
            "Target: sansaclipplus\nTarget id: 66\nVersion: f9fe698319-261114\nBinary size: 990112\nFeatures: a:b\n",
        );
        assert_eq!(info.target, "sansaclipplus");
        assert_eq!(info.version, "f9fe698319-261114");
        assert_eq!(info.binary_size, Some(990_112));
        assert_eq!(installed_id("f9fe698319-261114"), "f9fe698319");
        assert_eq!(installed_id("r1234-260101"), "1234");
        assert_eq!(installed_id("4.0"), "4.0");
    }

    #[test]
    fn updates_are_a_difference_in_hash_or_release() {
        let info = BuildInfo::parse(BUILD_INFO);
        let dev = info.build(Channel::Dev).unwrap();
        assert!(!update_available("f9fe698319-261114", &dev));
        assert!(update_available("0123456789-261113", &dev));
        assert!(update_available("4.0", &dev));
        let release = info.build(Channel::Release).unwrap();
        assert!(!update_available("4.0", &release));
        assert!(update_available("3.15", &release));
        let daily = info.build(Channel::Daily).unwrap();
        assert!(!update_available("cb36cae2bc-261114", &daily));
    }

    #[test]
    fn checks_the_boot_file() {
        assert!(check_binary(&test_binary(b"abc")).is_ok());
        let mut bad = test_binary(b"abc");
        bad[9] ^= 1;
        assert!(check_binary(&bad).is_err());
        let mut other = test_binary(b"abc");
        other[4..8].copy_from_slice(b"c200");
        assert!(check_binary(&other).is_err());
        assert!(check_binary(b"short").is_err());
    }

    #[test]
    fn the_log_matches_the_qt_format_and_round_trips() {
        let sample = "[Bootloader]\nbootloader-clipplus.sansa=2019-08-09T15:26:29Z\n\n\
[Rockbox%20%28Base%29]\n.rockbox\\langs\\english.lng=f9fe698319-261114\n.rockbox\\wps\\my%20theme\\a.bmp=f9fe698319-261114\n\n\
[Theme%3A%20Pure%20X3]\n.rockbox\\themes\\pure_x3.cfg=2024-01-26 21:40:53\n\n";
        let log = RbLog::parse(sample);
        assert_eq!(
            log.names(),
            ["Bootloader", "Rockbox (Base)", "Theme: Pure X3"]
        );
        assert_eq!(
            log.paths(BASE_SECTION),
            [".rockbox/langs/english.lng", ".rockbox/wps/my theme/a.bmp"]
        );
        assert_eq!(log.text(), sample);

        let mut log = RbLog::default();
        log.set(
            BASE_SECTION,
            &[
                ".rockbox/rockbox.sansa".into(),
                ".rockbox/wps/my theme/a.bmp".into(),
            ],
            "f9fe698319-261114",
        );
        let text = log.text();
        assert!(text.contains("[Rockbox%20%28Base%29]\n"));
        assert!(text.contains(".rockbox\\wps\\my%20theme\\a.bmp=f9fe698319-261114\n"));
        assert_eq!(
            RbLog::parse(&text).paths(BASE_SECTION),
            log.paths(BASE_SECTION)
        );
        assert_eq!(encode_key("Theme: A&B"), "Theme%3A%20A%26B");
        assert_eq!(decode_key("a%20b%U00e9"), "a b\u{e9}");
    }

    #[test]
    fn parses_the_theme_list() {
        let text = r#"[error]
code=0

[status]
msg=""

[pure_x3]
name="Pure X3 &amp; &quot;Co&quot;"
size=4116
descriptionfile=""
image="/themes/128x64/pure_x3/wps-dump 240127-005101.png"
archive="/download.php?themeid=3471"
author="Igor"
version="2024-01-26 21:40:53"
about="Simple &lt;b&gt;"
"#;
        let themes = parse_themes(text).unwrap();
        assert_eq!(themes.len(), 1);
        let theme = &themes[0];
        assert_eq!(theme.id, "pure_x3");
        assert_eq!(theme.name, "Pure X3 & \"Co\"");
        assert_eq!(theme.about, "Simple <b>");
        assert_eq!(theme.size, 4116);
        assert_eq!(
            theme.archive,
            "https://themes.rockbox.org/download.php?themeid=3471"
        );
        assert_eq!(
            theme.preview,
            "https://themes.rockbox.org/themes/128x64/pure_x3/wps-dump%20240127-005101.png"
        );
        let error = parse_themes("[error]\ncode=1\ndescription=\"No themes\"\n");
        assert_eq!(error.unwrap_err(), "No themes");
    }

    #[test]
    fn edits_one_setting_and_keeps_the_rest() {
        let text = "volume: -20\r\ntagcache_autoupdate: off\r\nshuffle: on\r\n";
        assert_eq!(
            edit_setting(text, "tagcache_autoupdate", "on"),
            "volume: -20\r\ntagcache_autoupdate: on\r\nshuffle: on\r\n"
        );
        assert_eq!(
            edit_setting("volume: -20\r\nshuffle: on", "tagcache_autoupdate", "on"),
            "volume: -20\r\nshuffle: on\r\ntagcache_autoupdate: on\r\n"
        );
        assert_eq!(
            edit_setting("", "tagcache_autoupdate", "on"),
            "tagcache_autoupdate: on\r\n"
        );
        assert_eq!(edit_setting("a: 1\n", "b", "2"), "a: 1\nb: 2\n");
        assert_eq!(
            edit_setting("tagcache_autoupdate_x: 1\n", "tagcache_autoupdate", "on"),
            "tagcache_autoupdate_x: 1\ntagcache_autoupdate: on\n"
        );
    }

    #[test]
    fn autoupdate_writes_config_cfg_on_the_drive() {
        let drive = tempfile::tempdir().unwrap();
        assert!(set_database_autoupdate(drive.path(), true).is_err());
        fs::create_dir(drive.path().join(".rockbox")).unwrap();
        set_database_autoupdate(drive.path(), true).unwrap();
        let path = drive.path().join(CONFIG_FILE);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tagcache_autoupdate: on\r\n"
        );
        set_database_autoupdate(drive.path(), false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tagcache_autoupdate: off\r\n"
        );
    }

    #[test]
    fn installs_a_build_and_logs_it() {
        let drive = tempfile::tempdir().unwrap();
        let zip = build_zip(
            "f9fe698319-261114",
            &[
                (".rockbox/rocks/a.rock", b"a"),
                (".rockbox/wps/my theme/a.bmp", b"b"),
            ],
        );
        let version = install_build_zip(zip.path(), drive.path(), &none).unwrap();
        assert_eq!(version, "f9fe698319-261114");
        assert_eq!(
            fs::read(drive.path().join(".rockbox/rocks/a.rock")).unwrap(),
            b"a"
        );
        assert!(crate::player::is_rockbox_volume(drive.path()));
        let log = RbLog::load(drive.path());
        assert_eq!(log.paths(BASE_SECTION).len(), 4);
        assert!(
            log.paths(BASE_SECTION)
                .contains(&".rockbox/wps/my theme/a.bmp".to_string())
        );
        // Four files and the log, no temp files.
        assert_eq!(walk_count(&drive.path().join(".rockbox")), 5);
    }

    #[test]
    fn the_boot_file_is_written_last() {
        let zip = build_zip("v1", &[(".rockbox/a", b"a"), (".rockbox/b", b"b")]);
        let mut archive = open_zip(zip.path()).unwrap();
        let (items, _) = plan(&mut archive).unwrap();
        assert_eq!(rel_string(&items.last().unwrap().path), BINARY);
    }

    #[test]
    fn an_update_removes_stale_files_and_keeps_user_state() {
        let drive = tempfile::tempdir().unwrap();
        let old = build_zip(
            "v1",
            &[
                (".rockbox/rocks/old.rock", b"o"),
                (".rockbox/rocks/keep.rock", b"k"),
            ],
        );
        install_build_zip(old.path(), drive.path(), &none).unwrap();
        fs::write(drive.path().join(".rockbox/config.cfg"), "volume: 1\r\n").unwrap();
        fs::write(drive.path().join(".rockbox/rocks/mine.rock"), "m").unwrap();
        let new = build_zip(
            "v2",
            &[
                (".rockbox/rocks/keep.rock", b"k2"),
                (".rockbox/codecs/new.codec", b"c"),
            ],
        );
        install_build_zip(new.path(), drive.path(), &none).unwrap();
        let rockbox = drive.path().join(".rockbox");
        assert!(!rockbox.join("rocks/old.rock").exists());
        assert_eq!(fs::read(rockbox.join("rocks/keep.rock")).unwrap(), b"k2");
        assert!(rockbox.join("rocks/mine.rock").exists());
        assert!(rockbox.join("config.cfg").exists());
        assert!(rockbox.join("codecs/new.codec").exists());
        let info = parse_info(&fs::read_to_string(rockbox.join("rockbox-info.txt")).unwrap());
        assert_eq!(info.version, "v2");
        assert!(
            !RbLog::load(drive.path())
                .paths(BASE_SECTION)
                .contains(&".rockbox/rocks/old.rock".to_string())
        );
    }

    #[test]
    fn a_stale_file_that_another_section_owns_stays() {
        let drive = tempfile::tempdir().unwrap();
        let fonts = make_zip(&[(".rockbox/fonts/x.fnt", b"f")]);
        let old = build_zip("v1", &[(".rockbox/fonts/x.fnt", b"f")]);
        install_build_zip(old.path(), drive.path(), &none).unwrap();
        install_logged(fonts.path(), drive.path(), "Fonts", "v1", false, &none).unwrap();
        let new = build_zip("v2", &[]);
        install_build_zip(new.path(), drive.path(), &none).unwrap();
        assert!(drive.path().join(".rockbox/fonts/x.fnt").exists());
    }

    #[test]
    fn rejects_unsafe_paths_before_writing_anything() {
        for name in ["../evil", "/etc/evil", ".rockbox/../../evil", "C:/evil"] {
            let drive = tempfile::tempdir().unwrap();
            let zip = build_zip("v1", &[(name, b"x")]);
            let err = install_build_zip(zip.path(), drive.path(), &none).unwrap_err();
            assert!(err.contains("unsafe path"), "{name}: {err}");
            assert_eq!(walk_count(drive.path()), 0, "{name}");
        }
    }

    #[test]
    fn rejects_a_wrong_target_or_a_bad_boot_file() {
        let drive = tempfile::tempdir().unwrap();
        let binary = test_binary(b"x");
        let other = make_zip(&[
            (BINARY, &binary),
            (INFO_FILE, b"Target: sansaclipv2\nVersion: v\n"),
        ]);
        let err = install_build_zip(other.path(), drive.path(), &none).unwrap_err();
        assert!(err.contains("sansaclipv2"));
        let mut bad = binary.clone();
        bad[8] ^= 1;
        let corrupt = make_zip(&[
            (BINARY, &bad),
            (INFO_FILE, b"Target: sansaclipplus\nVersion: v\n"),
        ]);
        assert!(install_build_zip(corrupt.path(), drive.path(), &none).is_err());
        let size = make_zip(&[
            (BINARY, &binary),
            (INFO_FILE, b"Target: sansaclipplus\nBinary size: 3\n"),
        ]);
        assert!(install_build_zip(size.path(), drive.path(), &none).is_err());
        assert!(install_build_zip(Path::new("/nonexistent.zip"), drive.path(), &none).is_err());
        assert_eq!(walk_count(drive.path()), 0);
    }

    #[test]
    fn needs_a_mounted_drive() {
        let zip = build_zip("v1", &[]);
        let err =
            install_build_zip(zip.path(), Path::new("/nonexistent/drive"), &none).unwrap_err();
        assert!(err.contains("not mounted"));
    }

    #[test]
    fn smart_uninstall_keeps_shared_files_settings_and_the_bootloader() {
        let drive = tempfile::tempdir().unwrap();
        let build = build_zip(
            "v1",
            &[
                (".rockbox/fonts/x.fnt", b"f"),
                (".rockbox/wps/a/a.bmp", b"b"),
            ],
        );
        install_build_zip(build.path(), drive.path(), &none).unwrap();
        let theme = make_zip(&[
            (".rockbox/themes/t.cfg", b"c"),
            (".rockbox/fonts/x.fnt", b"f"),
        ]);
        install_logged(theme.path(), drive.path(), "Theme: T", "1", false, &none).unwrap();
        let mut log = RbLog::load(drive.path());
        log.set(BOOTLOADER_SECTION, &["bootloader.sansa".into()], "2019");
        log.save(drive.path()).unwrap();
        let rockbox = drive.path().join(".rockbox");
        fs::write(rockbox.join("config.cfg"), "volume: 1\r\n").unwrap();

        uninstall_sections(drive.path(), &["Theme: T".to_string()]).unwrap();
        assert!(!rockbox.join("themes").exists());
        assert!(rockbox.join("fonts/x.fnt").exists());

        uninstall(drive.path(), false).unwrap();
        assert!(!rockbox.join("fonts").exists());
        assert!(!rockbox.join("wps").exists());
        assert!(!rockbox.join("rockbox.sansa").exists());
        assert!(rockbox.join("config.cfg").exists());
        assert_eq!(RbLog::load(drive.path()).names(), ["Bootloader"]);

        uninstall(drive.path(), true).unwrap();
        assert!(!rockbox.exists());
    }

    #[test]
    fn keeps_the_last_two_builds_and_rolls_back() {
        let work = tempfile::tempdir().unwrap();
        let drive = tempfile::tempdir().unwrap();
        assert!(!has_previous(work.path()));
        let first = build_zip("v1", &[]);
        let second = build_zip("v2", &[]);
        let part = work.path().join(PART);
        fs::copy(first.path(), &part).unwrap();
        keep_zip(work.path(), &part).unwrap();
        fs::copy(second.path(), &part).unwrap();
        keep_zip(work.path(), &part).unwrap();
        assert!(has_previous(work.path()));
        assert!(!part.exists());
        install_build_zip(&work.path().join(CURRENT), drive.path(), &none).unwrap();
        let version = install_previous(work.path(), drive.path(), &none).unwrap();
        assert_eq!(version, "v1");
        let installed = fs::read_to_string(drive.path().join(INFO_FILE)).unwrap();
        assert_eq!(parse_info(&installed).version, "v1");
        assert_eq!(
            verify_build(&work.path().join(PREVIOUS)).unwrap().version,
            "v2"
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_build_info_and_themes() {
        let agent = net::agent();
        let cache = tempfile::tempdir().unwrap();
        let info = fetch_build_info(&agent, cache.path()).unwrap();
        let dev = info.build(Channel::Dev).unwrap();
        println!("dev {} {}", dev.version, dev.url);
        let again = fetch_build_info(&agent, cache.path()).unwrap();
        assert_eq!(again.build(Channel::Dev).unwrap(), dev);
        assert!(cache.path().join("build-info.etag").exists());
        for channel in [Channel::Daily, Channel::Release] {
            println!("{channel:?} {:?}", info.build(channel).unwrap());
        }
        let themes = list_themes(&agent, &dev.version).unwrap();
        println!("{} themes, first {:?}", themes.len(), themes.first());
        assert!(!themes.is_empty());
    }
}
