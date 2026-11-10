//! Copies an outbox file into the card's Tracks folder and checks the copy.
//! Only files this module wrote are ever deleted there.

use crate::fixer::temp_name;
use crate::rules;
use rustix::fs::{Advice, fadvise, statvfs};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const BUFFER: usize = 256 * 1024;
const FREE_MARGIN: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied(PathBuf),
    AlreadyThere,
}

pub fn free_space(path: &Path) -> Option<u64> {
    statvfs(path)
        .ok()
        .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
}

/// Compares file contents. `fresh` drops cached pages of `second` first, so
/// the bytes come from the device.
pub fn same_content(first: &Path, second: &Path, fresh: bool) -> io::Result<bool> {
    if fs::metadata(first)?.len() != fs::metadata(second)?.len() {
        return Ok(false);
    }
    let mut a = File::open(first)?;
    let mut b = File::open(second)?;
    if fresh {
        let _ = fadvise(&b, 0, None, Advice::DontNeed);
    }
    let mut buf_a = vec![0u8; BUFFER];
    let mut buf_b = vec![0u8; BUFFER];
    loop {
        let n = a.read(&mut buf_a)?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut buf_b[..n])?;
        if buf_a[..n] != buf_b[..n] {
            return Ok(false);
        }
    }
}

/// On success the outbox file is deleted. It is also deleted when an identical
/// file is already on the card. On failure it stays and no partial file is left.
pub fn copy_to_card(outbox_file: &Path, tracks: &Path) -> io::Result<Outcome> {
    fs::create_dir_all(tracks)?;
    let ext = outbox_file
        .extension()
        .map_or_else(String::new, |e| e.to_string_lossy().into_owned());
    let stem = outbox_file
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let stem = rules::safe_stem(&stem, &ext);

    let mut target = None;
    for number in 1.. {
        let candidate = tracks.join(rules::numbered_name(&stem, &ext, number));
        if !candidate.exists() {
            target = Some(candidate);
            break;
        }
        if same_content(outbox_file, &candidate, false)? {
            fs::remove_file(outbox_file)?;
            return Ok(Outcome::AlreadyThere);
        }
    }
    let target = target.expect("loop ends with a free name");

    let length = fs::metadata(outbox_file)?.len();
    if free_space(tracks).is_some_and(|free| free < length + FREE_MARGIN) {
        return Err(io::Error::other("The Sansa is full"));
    }

    let temp = tracks.join(temp_name());
    let mut written = temp.clone();
    let result = (|| {
        let mut source = File::open(outbox_file)?;
        let mut sink = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        let mut buf = vec![0u8; BUFFER];
        loop {
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sink.write_all(&buf[..n])?;
        }
        sink.sync_all()?;
        drop(sink);
        if target.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "name taken on the card"));
        }
        fs::rename(&temp, &target)?;
        written = target.clone();
        if !same_content(outbox_file, &target, true)? {
            return Err(io::Error::other(format!(
                "The copy of {} does not match the original",
                target.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            fs::remove_file(outbox_file)?;
            Ok(Outcome::Copied(target))
        }
        Err(e) => {
            let _ = fs::remove_file(&written);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("outbox");
        let tracks = dir.path().join("card").join("Tracks");
        fs::create_dir_all(&outbox).unwrap();
        (dir, outbox, tracks)
    }

    fn song(outbox: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = outbox.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn copies_and_deletes_the_outbox_file() {
        let (_dir, outbox, tracks) = setup();
        let file = song(&outbox, "Song.mp3", b"audio");
        let outcome = copy_to_card(&file, &tracks).unwrap();
        assert_eq!(outcome, Outcome::Copied(tracks.join("Song.mp3")));
        assert_eq!(fs::read(tracks.join("Song.mp3")).unwrap(), b"audio");
        assert!(!file.exists());
        let leftovers: Vec<_> = fs::read_dir(&tracks).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn identical_file_is_dropped_not_copied_again() {
        let (_dir, outbox, tracks) = setup();
        fs::create_dir_all(&tracks).unwrap();
        fs::write(tracks.join("Song.mp3"), b"audio").unwrap();
        let file = song(&outbox, "Song.mp3", b"audio");
        assert_eq!(copy_to_card(&file, &tracks).unwrap(), Outcome::AlreadyThere);
        assert!(!file.exists());
        assert_eq!(fs::read_dir(&tracks).unwrap().count(), 1);
    }

    #[test]
    fn different_file_gets_a_number_and_the_old_one_stays() {
        let (_dir, outbox, tracks) = setup();
        fs::create_dir_all(&tracks).unwrap();
        fs::write(tracks.join("Song.mp3"), b"old").unwrap();
        fs::write(tracks.join("Song (2).mp3"), b"older").unwrap();
        let file = song(&outbox, "Song.mp3", b"new");
        let outcome = copy_to_card(&file, &tracks).unwrap();
        assert_eq!(outcome, Outcome::Copied(tracks.join("Song (3).mp3")));
        assert_eq!(fs::read(tracks.join("Song.mp3")).unwrap(), b"old");
        assert_eq!(fs::read(tracks.join("Song (2).mp3")).unwrap(), b"older");
        assert_eq!(fs::read(tracks.join("Song (3).mp3")).unwrap(), b"new");
    }

    #[test]
    fn identical_numbered_copy_is_recognized() {
        let (_dir, outbox, tracks) = setup();
        fs::create_dir_all(&tracks).unwrap();
        fs::write(tracks.join("Song.mp3"), b"old").unwrap();
        fs::write(tracks.join("Song (2).mp3"), b"new").unwrap();
        let file = song(&outbox, "Song.mp3", b"new");
        assert_eq!(copy_to_card(&file, &tracks).unwrap(), Outcome::AlreadyThere);
        assert_eq!(fs::read_dir(&tracks).unwrap().count(), 2);
    }

    #[test]
    fn unsafe_characters_are_replaced_on_the_card() {
        let (_dir, outbox, tracks) = setup();
        let file = song(&outbox, "A: B? C.mp3", b"x");
        copy_to_card(&file, &tracks).unwrap();
        assert!(tracks.join("A_ B_ C.mp3").exists());
    }

    #[test]
    fn long_names_are_cut_under_the_limit() {
        let (_dir, outbox, tracks) = setup();
        let file = song(&outbox, &format!("{}.mp3", "x".repeat(250)), b"x");
        let Outcome::Copied(path) = copy_to_card(&file, &tracks).unwrap() else {
            panic!("expected a copy");
        };
        assert!(path.file_name().unwrap().len() <= rules::MAX_NAME_BYTES);
    }

    #[test]
    fn failed_copy_keeps_the_outbox_file() {
        let (_dir, outbox, tracks) = setup();
        let file = song(&outbox, "Song.mp3", b"audio");
        // A file where the Tracks folder should be stands in for a card that went away.
        fs::create_dir_all(tracks.parent().unwrap()).unwrap();
        fs::write(&tracks, b"not a folder").unwrap();
        assert!(copy_to_card(&file, &tracks).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"audio");
    }

    #[test]
    fn interrupted_copy_leaves_no_partial_file() {
        let (_dir, outbox, tracks) = setup();
        // Opening a folder works but reading it fails, after the temp file exists.
        let source = outbox.join("Song.mp3");
        fs::create_dir(&source).unwrap();
        assert!(copy_to_card(&source, &tracks).is_err());
        assert!(source.is_dir());
        assert_eq!(fs::read_dir(&tracks).unwrap().count(), 0);
    }

    #[test]
    fn same_content_compares_bytes() {
        let (_dir, outbox, _) = setup();
        let a = song(&outbox, "a", b"abcdef");
        let b = song(&outbox, "b", b"abcdeg");
        let c = song(&outbox, "c", b"abc");
        assert!(same_content(&a, &a, false).unwrap());
        assert!(!same_content(&a, &b, false).unwrap());
        assert!(!same_content(&a, &c, true).unwrap());
    }
}
