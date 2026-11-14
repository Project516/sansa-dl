//! The player as a whole: its drives, the firmware on it, and MTP mode.
//! USB changes arrive as kernel uevents; nothing polls.

use crate::card::{CardInfo, Volumes};
use crate::rockbox;
use rustix::fs::statvfs;
use rustix::net::netlink::{KOBJECT_UEVENT, SocketAddrNetlink};
use rustix::net::{AddressFamily, RecvFlags, SocketType, bind, recv, socket};
use std::fs;
use std::path::{Path, PathBuf};

const USB_ID: &str = "0781";
const MTP_PRODUCT: &str = "74d0";
const SYSFS_USB: &str = "/sys/bus/usb/devices";
const REDIRECT: &str = "rockbox_main.clip+";
const KERNEL_GROUP: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeState {
    pub mount: PathBuf,
    pub total: u64,
    pub free: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Firmware {
    Stock,
    Rockbox {
        version: String,
    },
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerState {
    pub connected: bool,
    pub mtp: bool,
    pub internal: Option<VolumeState>,
    pub card: Option<VolumeState>,
    pub firmware: Firmware,
    /// The patched original firmware waits in the internal root for the player to flash.
    pub bootloader_pending: bool,
}

/// True when the volume holds a Clip+ Rockbox install the player boots from.
pub fn is_rockbox_volume(mount: &Path) -> bool {
    if mount.join(REDIRECT).exists() {
        return false;
    }
    let Some(info) = read_info(mount) else {
        return false;
    };
    let Ok(binary) = fs::read(mount.join(".rockbox/rockbox.sansa")) else {
        return false;
    };
    info.target == rockbox::TARGET
        && info.binary_size.is_none_or(|n| n == binary.len() as u64)
        && rockbox::check_binary(&binary).is_ok()
}

fn read_info(mount: &Path) -> Option<rockbox::Info> {
    let text = fs::read_to_string(mount.join(".rockbox/rockbox-info.txt")).ok()?;
    Some(rockbox::parse_info(&text))
}

fn volume_state(info: Option<&CardInfo>) -> Option<VolumeState> {
    let mount = info?.mount.clone()?;
    let stat = statvfs(&mount).ok()?;
    Some(VolumeState {
        total: stat.f_blocks.saturating_mul(stat.f_frsize),
        free: stat.f_bavail.saturating_mul(stat.f_frsize),
        mount,
    })
}

fn firmware(mount: Option<&Path>) -> Firmware {
    let Some(mount) = mount else {
        return Firmware::Unknown;
    };
    match read_info(mount) {
        Some(info) if info.target == rockbox::TARGET => Firmware::Rockbox {
            version: info.version,
        },
        _ if !mount.join(".rockbox").exists() => Firmware::Stock,
        _ => Firmware::Unknown,
    }
}

/// Reads the drives. Does file I/O, so call it from a worker thread.
pub fn read_state(volumes: &Volumes, mtp: bool) -> PlayerState {
    let internal = volume_state(volumes.internal.as_ref());
    let mount = internal.as_ref().map(|v| v.mount.as_path());
    PlayerState {
        connected: volumes.internal.is_some() || mtp,
        mtp,
        firmware: firmware(mount),
        bootloader_pending: mount.is_some_and(|m| m.join("clppa.bin").exists()),
        card: volume_state(volumes.card.as_ref()),
        internal,
    }
}

/// True when a Clip+ in MTP mode is on the bus. `skip` names a bus id that is
/// going away: the kernel sends the remove event before it deletes the entry.
pub fn mtp_present(sysfs: &Path, skip: Option<&str>) -> bool {
    let Ok(entries) = fs::read_dir(sysfs) else {
        return false;
    };
    entries.flatten().any(|entry| {
        if skip.is_some_and(|s| entry.file_name() == s) {
            return false;
        }
        let read = |name: &str| fs::read_to_string(entry.path().join(name)).unwrap_or_default();
        read("idVendor").trim() == USB_ID && read("idProduct").trim() == MTP_PRODUCT
    })
}

/// The bus id of a USB device that appeared or went away, from a kernel uevent.
fn usb_event(message: &[u8]) -> Option<(bool, &str)> {
    let mut fields = message
        .split(|b| *b == 0)
        .filter_map(|f| str::from_utf8(f).ok());
    fields.next()?;
    let (mut action, mut path, mut usb, mut device) = ("", "", false, false);
    for field in fields {
        match field.split_once('=') {
            Some(("ACTION", v)) => action = v,
            Some(("DEVPATH", v)) => path = v,
            Some(("SUBSYSTEM", "usb")) => usb = true,
            Some(("DEVTYPE", "usb_device")) => device = true,
            _ => {}
        }
    }
    let id = path.rsplit('/').next().filter(|id| !id.is_empty())?;
    (usb && device && (action == "add" || action == "remove")).then_some((action == "remove", id))
}

fn watch_usb(sysfs: &Path, mut on_change: impl FnMut(bool)) {
    let mut last = mtp_present(sysfs, None);
    on_change(last);
    let fd = socket(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        Some(KOBJECT_UEVENT),
    )
    .and_then(|fd| bind(&fd, &SocketAddrNetlink::new(0, KERNEL_GROUP)).map(|()| fd));
    let fd = match fd {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("clipplus-manager: usb watch: {e}");
            return;
        }
    };
    let mut buf = vec![0u8; 8192];
    loop {
        let skip = match recv(&fd, &mut buf, RecvFlags::empty()) {
            Ok((n, _)) => match usb_event(&buf[..n.min(buf.len())]) {
                Some((true, id)) => Some(id.to_string()),
                Some((false, _)) => None,
                None => continue,
            },
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::NOBUFS) => None,
            Err(e) => {
                eprintln!("clipplus-manager: usb watch: {e}");
                return;
            }
        };
        let now = mtp_present(sysfs, skip.as_deref());
        if now != last {
            last = now;
            on_change(now);
        }
    }
}

/// Reports whether the player is in MTP mode, now and on every USB change.
/// Runs on its own thread for the life of the app.
pub fn watch_mtp(on_change: impl FnMut(bool) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("usb-watch".into())
        .spawn(move || watch_usb(Path::new(SYSFS_USB), on_change));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb_device(sysfs: &Path, name: &str, vendor: &str, product: &str) {
        let dir = sysfs.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("idVendor"), format!("{vendor}\n")).unwrap();
        fs::write(dir.join("idProduct"), format!("{product}\n")).unwrap();
    }

    #[test]
    fn finds_mtp_mode_only_for_the_mtp_product() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!mtp_present(dir.path(), None));
        usb_device(dir.path(), "1-1", "0781", "74d1");
        usb_device(dir.path(), "1-2", "1234", "74d0");
        assert!(!mtp_present(dir.path(), None));
        usb_device(dir.path(), "1-3", "0781", "74d0");
        assert!(mtp_present(dir.path(), None));
        assert!(!mtp_present(dir.path(), Some("1-3")));
    }

    #[test]
    fn reads_usb_uevents() {
        let event = b"remove@/devices/pci0000:00/usb1/1-3\0ACTION=remove\0DEVPATH=/devices/pci0000:00/usb1/1-3\0SUBSYSTEM=usb\0DEVTYPE=usb_device\0PRODUCT=781/74d0/100\0";
        assert_eq!(usb_event(event), Some((true, "1-3")));
        let interface = b"add@/devices/usb1/1-3/1-3:1.0\0ACTION=add\0DEVPATH=/devices/usb1/1-3/1-3:1.0\0SUBSYSTEM=usb\0DEVTYPE=usb_interface\0";
        assert_eq!(usb_event(interface), None);
        let block = b"add@/devices/x/sdb\0ACTION=add\0DEVPATH=/devices/x/sdb\0SUBSYSTEM=block\0";
        assert_eq!(usb_event(block), None);
        let change = b"change@/devices/usb1/1-3\0ACTION=change\0DEVPATH=/devices/usb1/1-3\0SUBSYSTEM=usb\0DEVTYPE=usb_device\0";
        assert_eq!(usb_event(change), None);
    }

    fn rockbox_drive(root: &Path) {
        let binary = rockbox::test_binary(b"payload");
        fs::create_dir_all(root.join(".rockbox")).unwrap();
        fs::write(root.join(".rockbox/rockbox.sansa"), &binary).unwrap();
        fs::write(
            root.join(".rockbox/rockbox-info.txt"),
            format!(
                "Target: sansaclipplus\nVersion: f9fe698319-261114\nBinary size: {}\n",
                binary.len()
            ),
        )
        .unwrap();
    }

    #[test]
    fn recognizes_a_rockbox_volume_by_its_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_rockbox_volume(dir.path()));
        rockbox_drive(dir.path());
        assert!(is_rockbox_volume(dir.path()));
        assert_eq!(
            firmware(Some(dir.path())),
            Firmware::Rockbox {
                version: "f9fe698319-261114".into()
            }
        );
        fs::write(dir.path().join(REDIRECT), "").unwrap();
        assert!(!is_rockbox_volume(dir.path()));
    }

    #[test]
    fn rejects_a_corrupt_boot_file_or_another_target() {
        let dir = tempfile::tempdir().unwrap();
        rockbox_drive(dir.path());
        let binary = dir.path().join(".rockbox/rockbox.sansa");
        let mut bytes = fs::read(&binary).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&binary, bytes).unwrap();
        assert!(!is_rockbox_volume(dir.path()));
        rockbox_drive(dir.path());
        let info = dir.path().join(".rockbox/rockbox-info.txt");
        let text = fs::read_to_string(&info).unwrap();
        fs::write(&info, text.replace("sansaclipplus", "sansaclipv2")).unwrap();
        assert!(!is_rockbox_volume(dir.path()));
    }

    #[test]
    fn firmware_is_stock_without_a_rockbox_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(firmware(Some(dir.path())), Firmware::Stock);
        fs::create_dir(dir.path().join(".rockbox")).unwrap();
        assert_eq!(firmware(Some(dir.path())), Firmware::Unknown);
        assert_eq!(firmware(None), Firmware::Unknown);
    }

    #[test]
    fn state_reports_space_and_a_pending_bootloader() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = |mount: Option<&Path>| Volumes {
            internal: Some(CardInfo {
                block: "/b".into(),
                device: "/dev/sdb1".into(),
                drive: None,
                mount: mount.map(Path::to_path_buf),
            }),
            card: None,
        };
        let state = read_state(&volumes(Some(dir.path())), false);
        let internal = state.internal.unwrap();
        assert!(state.connected && internal.total >= internal.free && internal.total > 0);
        assert!(!state.bootloader_pending);
        fs::write(dir.path().join("clppa.bin"), "x").unwrap();
        assert!(read_state(&volumes(Some(dir.path())), false).bootloader_pending);

        let unmounted = read_state(&volumes(None), false);
        assert!(unmounted.connected && unmounted.internal.is_none());
        assert_eq!(unmounted.firmware, Firmware::Unknown);

        let mtp = read_state(&Volumes::default(), true);
        assert!(mtp.connected && mtp.mtp);
        assert!(!read_state(&Volumes::default(), false).connected);
    }
}
