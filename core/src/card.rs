//! The Sansa card over UDisks2. A card is a vfat filesystem labeled SANSA.
//! Changes arrive as D-Bus signals; nothing polls.

use std::collections::HashMap;
use std::path::PathBuf;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const LABEL: &str = "SANSA";

type Properties = HashMap<String, OwnedValue>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    pub block: String,
    pub device: String,
    pub drive: Option<String>,
    pub mount: Option<PathBuf>,
}

fn string(props: &Properties, key: &str) -> Option<String> {
    String::try_from(props.get(key)?.try_clone().ok()?).ok()
}

fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end]))
}

/// The first vfat filesystem labeled SANSA among the UDisks2 objects.
pub fn find_card(objects: &Objects) -> Option<CardInfo> {
    let mut found: Vec<CardInfo> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let block = interfaces.get(BLOCK)?;
            let filesystem = interfaces.get(FILESYSTEM)?;
            let label = string(block, "IdLabel")?;
            if !label.eq_ignore_ascii_case(LABEL) || string(block, "IdType")? != "vfat" {
                return None;
            }
            let device = block
                .get("PreferredDevice")
                .or_else(|| block.get("Device"))
                .and_then(|v| Vec::<u8>::try_from(v.try_clone().ok()?).ok())
                .map(|b| bytes_to_path(&b).to_string_lossy().into_owned())
                .unwrap_or_default();
            let drive = block
                .get("Drive")
                .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok())
                .map(|p| p.as_str().to_string())
                .filter(|p| p != "/");
            let mount = filesystem
                .get("MountPoints")
                .and_then(|v| Vec::<Vec<u8>>::try_from(v.try_clone().ok()?).ok())
                .and_then(|points| points.first().map(|p| bytes_to_path(p)));
            Some(CardInfo {
                block: path.as_str().to_string(),
                device,
                drive,
                mount,
            })
        })
        .collect();
    found.sort_by(|a, b| a.block.cmp(&b.block));
    found.into_iter().next()
}

/// The text of a D-Bus error, without the wrapping.
fn describe(error: zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(_, Some(text), _) if !text.is_empty() => text,
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        other => other.to_string(),
    }
}

#[derive(Clone)]
pub struct Udisks {
    conn: Connection,
}

impl Udisks {
    pub fn connect() -> Result<Self, String> {
        Connection::system()
            .map(|conn| Self { conn })
            .map_err(|e| format!("No connection to UDisks2: {}", describe(e)))
    }

    fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> Result<Proxy<'a>, String> {
        Proxy::new(&self.conn, DEST, path, interface).map_err(describe)
    }

    pub fn scan(&self) -> Result<Option<CardInfo>, String> {
        let manager = self.proxy(ROOT, "org.freedesktop.DBus.ObjectManager")?;
        let objects: Objects = manager.call("GetManagedObjects", &()).map_err(describe)?;
        Ok(find_card(&objects))
    }

    /// Mounts the card when needed and returns where it is mounted.
    pub fn mount(&self, card: &CardInfo) -> Result<PathBuf, String> {
        if let Some(mount) = &card.mount {
            return Ok(mount.clone());
        }
        let options: HashMap<&str, Value> = HashMap::new();
        let proxy = self.proxy(&card.block, FILESYSTEM)?;
        match proxy.call::<_, _, String>("Mount", &(options,)) {
            Ok(path) => Ok(PathBuf::from(path)),
            Err(e) => {
                let text = describe(e);
                // Another mounter won the race.
                match self.scan() {
                    Ok(Some(now)) if now.mount.is_some() => Ok(now.mount.unwrap_or_default()),
                    _ => Err(text),
                }
            }
        }
    }

    /// Unmounts the card and powers its drive off.
    pub fn eject(&self, card: &CardInfo) -> Result<(), String> {
        let options = || HashMap::<&str, Value>::new();
        let filesystem = self.proxy(&card.block, FILESYSTEM)?;
        if let Err(e) = filesystem.call::<_, _, ()>("Unmount", &(options(),)) {
            let text = describe(e);
            if !text.contains("not mounted") && !text.contains("NotMounted") {
                return Err(text);
            }
        }
        if let Some(drive) = &card.drive {
            let proxy = self.proxy(drive, DRIVE)?;
            let can_power_off = proxy.get_property::<bool>("CanPowerOff").unwrap_or(false);
            if can_power_off {
                proxy
                    .call::<_, _, ()>("PowerOff", &(options(),))
                    .map_err(describe)?;
            }
        }
        Ok(())
    }

    /// Watches for the card on its own thread for the life of the app.
    /// `on_connect` gets every new UDisks2 connection; `on_change` gets the
    /// card now and whenever it appears, goes away, or is (un)mounted, and
    /// errors while UDisks2 is unreachable. Lost connections are retried.
    pub fn watch(
        on_connect: impl Fn(Udisks) + Send + 'static,
        mut on_change: impl FnMut(Result<Option<CardInfo>, String>) + Send + 'static,
    ) {
        std::thread::spawn(move || {
            let mut last: Option<Option<CardInfo>> = None;
            let mut delay = RETRY_MIN;
            loop {
                let outcome = Udisks::connect().and_then(|udisks| {
                    on_connect(udisks.clone());
                    let started = std::time::Instant::now();
                    let result = udisks.follow(&mut last, &mut on_change);
                    if started.elapsed() >= RETRY_MAX {
                        delay = RETRY_MIN;
                    }
                    result
                });
                if let Err(e) = outcome {
                    eprintln!("clipplus-manager: card watch: {e}");
                    last = None;
                    on_change(Err(e));
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(RETRY_MAX);
            }
        });
    }

    /// Reports card changes until the signal stream fails or ends.
    fn follow(
        &self,
        last: &mut Option<Option<CardInfo>>,
        on_change: &mut impl FnMut(Result<Option<CardInfo>, String>),
    ) -> Result<(), String> {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(DEST)
            .map_err(describe)?
            .build();
        let signals =
            MessageIterator::for_match_rule(rule, &self.conn, Some(64)).map_err(describe)?;
        let mut report = |now| report_change(last, on_change, now);
        report(self.scan());
        for message in signals {
            let message = message.map_err(describe)?;
            if relevant(&message) {
                report(self.scan());
            }
        }
        Err("UDisks2 signal stream ended".into())
    }
}

fn report_change(
    last: &mut Option<Option<CardInfo>>,
    on_change: &mut impl FnMut(Result<Option<CardInfo>, String>),
    now: Result<Option<CardInfo>, String>,
) {
    match now {
        Ok(card) if last.as_ref() == Some(&card) => {}
        Ok(card) => {
            *last = Some(card.clone());
            on_change(Ok(card));
        }
        Err(e) => {
            *last = None;
            on_change(Err(e));
        }
    }
}

const RETRY_MIN: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

fn relevant(message: &zbus::Message) -> bool {
    let header = message.header();
    match header.member().map(|m| m.as_str()) {
        Some("InterfacesAdded" | "InterfacesRemoved") => true,
        Some("PropertiesChanged") => message
            .body()
            .deserialize::<(String, Properties, Vec<String>)>()
            .is_ok_and(|(interface, _, _)| interface == FILESYSTEM || interface == BLOCK),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a, T: Into<Value<'a>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    fn object(
        label: &str,
        fs_type: &str,
        mount: Option<&str>,
        drive: &str,
    ) -> HashMap<String, Properties> {
        let mut block = Properties::new();
        block.insert("IdLabel".into(), value(label));
        block.insert("IdType".into(), value(fs_type));
        block.insert("Device".into(), value(b"/dev/sdb1\0".to_vec()));
        block.insert(
            "Drive".into(),
            value(zbus::zvariant::ObjectPath::try_from(drive).unwrap()),
        );
        let mut filesystem = Properties::new();
        let points: Vec<Vec<u8>> = mount
            .map(|m| vec![[m.as_bytes(), b"\0"].concat()])
            .unwrap_or_default();
        filesystem.insert("MountPoints".into(), value(points));
        HashMap::from([
            (BLOCK.to_string(), block),
            (FILESYSTEM.to_string(), filesystem),
        ])
    }

    fn objects(items: Vec<(&str, HashMap<String, Properties>)>) -> Objects {
        items
            .into_iter()
            .map(|(path, ifaces)| (OwnedObjectPath::try_from(path).unwrap(), ifaces))
            .collect()
    }

    #[test]
    fn reports_the_same_card_after_a_scan_error() {
        let card = find_card(&objects(vec![(
            "/o/b1",
            object("SANSA", "vfat", None, "/"),
        )]));
        let mut last = None;
        let mut reports = Vec::new();
        let mut record = |result| reports.push(result);
        report_change(&mut last, &mut record, Ok(card.clone()));
        report_change(&mut last, &mut record, Ok(card.clone()));
        report_change(&mut last, &mut record, Err("scan failed".into()));
        report_change(&mut last, &mut record, Ok(card));
        assert_eq!(reports.len(), 3);
        assert!(reports[1].is_err());
        assert!(reports[2].as_ref().unwrap().is_some());
    }

    #[test]
    fn finds_a_mounted_card() {
        let all = objects(vec![
            (
                "/org/freedesktop/UDisks2/block_devices/sda1",
                object("OTHER", "vfat", None, "/"),
            ),
            (
                "/org/freedesktop/UDisks2/block_devices/sdb1",
                object(
                    "SANSA",
                    "vfat",
                    Some("/run/media/<user>/SANSA"),
                    "/org/freedesktop/UDisks2/drives/D",
                ),
            ),
        ]);
        let card = find_card(&all).unwrap();
        assert_eq!(card.device, "/dev/sdb1");
        assert_eq!(card.mount, Some(PathBuf::from("/run/media/<user>/SANSA")));
        assert_eq!(
            card.drive.as_deref(),
            Some("/org/freedesktop/UDisks2/drives/D")
        );
        assert!(card.block.ends_with("sdb1"));
    }

    #[test]
    fn unmounted_card_has_no_mount_point() {
        let all = objects(vec![("/o/b1", object("sansa", "vfat", None, "/"))]);
        let card = find_card(&all).unwrap();
        assert_eq!(card.mount, None);
        assert_eq!(card.drive, None);
    }

    #[test]
    fn other_filesystems_and_labels_are_ignored() {
        let all = objects(vec![
            ("/o/a", object("SANSA", "ext4", None, "/")),
            ("/o/b", object("MUSIC", "vfat", None, "/")),
        ]);
        assert_eq!(find_card(&all), None);
        assert_eq!(find_card(&Objects::new()), None);
    }
}
