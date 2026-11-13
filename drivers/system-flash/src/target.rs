//! The target: the one device or file a board's first flash writes, and whether it may be written.
//!
//! The target's path comes only from the board file. A write is refused when the target is the
//! device the running system is on, which is found in the mount table: a mounted filesystem whose
//! device is the target, or a partition of it. A flasher runs from RAM, so nothing it mounts is on
//! the target, and the check passes; a normal image's root is on the target, and the check refuses.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The bytes a target is read and written in, and checked by.
pub const SECTOR: u64 = 512;

/// What the target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A block device: a whole disk, eMMC, SD card.
    Block,
    /// A regular file: for tests and dev boards only, named `kind = "file"` in the board file.
    File,
    /// An MTD character device (`/dev/mtdN`): raw NAND, written in erase blocks (see `nand`).
    Mtd,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "block" => Some(Self::Block),
            "file" => Some(Self::File),
            "mtd" => Some(Self::Mtd),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::File => "file",
            Self::Mtd => "mtd",
        }
    }
}

/// A target that is there: its path, kind, size and device number.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    /// The device number: `rdev` of a block device, the file system's `dev` of a file.
    pub dev: u64,
}

/// Why the target can't be used, and what the board should do.
pub type Unusable = String;

/// Looks at the target at `path`, which must be of `kind`.
///
/// # Errors
///
/// When it is missing, is of another kind, or its size can't be read.
pub fn find(path: &Path, kind: Kind) -> Result<Found, Unusable> {
    let meta = fs::metadata(path)
        .map_err(|e| format!("the target {} is not there: {e}", path.display()))?;
    let (is_kind, dev) = match kind {
        Kind::Block => (meta.file_type().is_block_device(), meta.rdev()),
        Kind::Mtd => (meta.file_type().is_char_device(), meta.rdev()),
        Kind::File => (meta.file_type().is_file(), meta.dev()),
    };
    if !is_kind {
        return Err(format!(
            "the target {} is not a {} device",
            path.display(),
            kind.word()
        ));
    }
    let size = match kind {
        Kind::File => meta.len(),
        Kind::Block | Kind::Mtd => {
            let mut file =
                File::open(path).map_err(|e| format!("the target can't be opened: {e}"))?;
            io::Seek::seek(&mut file, io::SeekFrom::End(0))
                .map_err(|e| format!("the target's size can't be read: {e}"))?
        }
    };
    Ok(Found {
        path: path.to_path_buf(),
        kind,
        size,
        dev,
    })
}

/// Opens the target for writing (`write`) or reading.
///
/// # Errors
///
/// When the OS refuses it.
pub fn open(found: &Found, write: bool) -> io::Result<File> {
    OpenOptions::new().read(true).write(write).open(&found.path)
}

/// The device number of a Linux `dev_t`: `(major, minor)`.
pub fn major_minor(dev: u64) -> (u32, u32) {
    let major = (((dev >> 8) & 0xFFF) | ((dev >> 32) & !0xFFF)) as u32;
    let minor = ((dev & 0xFF) | ((dev >> 12) & !0xFF)) as u32;
    (major, minor)
}

/// The mount point of a filesystem on the target, when the mount table in `mountinfo` has one, or
/// `None` when nothing mounted is on it. `disk_of` gives a partition's whole disk, when it is one.
///
/// Each line of `mountinfo` is `id parent major:minor root mount-point ...`.
pub fn mounted_on(
    mountinfo: &str,
    target: (u32, u32),
    disk_of: &dyn Fn((u32, u32)) -> Option<(u32, u32)>,
) -> Option<String> {
    for line in mountinfo.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_), Some(_), Some(device), Some(_), Some(mount)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let Some((major, minor)) = device.split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            continue;
        };
        let here = (major, minor);
        if here == target || disk_of(here) == Some(target) {
            return Some(mount.to_string());
        }
    }
    None
}

/// The device numbers the running system could be on, for the partition with MTD index `index`:
/// its `mtdblock` device, and the UBI volumes attached from it (`/class/ubi/ubiN` with `mtd_num`
/// equal to `index`, and their `ubiN_M` volumes, whose `dev` file gives the number).
pub fn mtd_devices(sysroot: &Path, index: u32) -> Vec<(u32, u32)> {
    let mut devices = vec![(MTDBLOCK_MAJOR, index)];
    let ubi = sysroot.join("class").join("ubi");
    let Ok(entries) = fs::read_dir(&ubi) else {
        return devices;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    for device in names.iter().filter(|n| is_ubi_device(n)) {
        let mtd = fs::read_to_string(ubi.join(device).join("mtd_num"))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok());
        if mtd != Some(index) {
            continue;
        }
        for volume in names
            .iter()
            .filter(|n| n.starts_with(&format!("{device}_")))
        {
            let dev = fs::read_to_string(ubi.join(volume).join("dev")).ok();
            if let Some(pair) = dev.as_deref().and_then(parse_dev) {
                devices.push(pair);
            }
        }
    }
    devices
}

/// The major of `mtdblock` devices.
pub const MTDBLOCK_MAJOR: u32 = 31;

/// Whether `name` is a UBI device (`ubiN`, not a volume `ubiN_M`).
fn is_ubi_device(name: &str) -> bool {
    name.strip_prefix("ubi")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// `major:minor`, as a sysfs `dev` file holds it.
fn parse_dev(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The whole disk a partition belongs to, read from sysfs, or `None` for a whole disk.
pub fn sysfs_disk((major, minor): (u32, u32)) -> Option<(u32, u32)> {
    let dir = format!("/sys/dev/block/{major}:{minor}");
    if !Path::new(&dir).join("partition").exists() {
        return None;
    }
    let text = fs::read_to_string(Path::new(&dir).join("..").join("dev")).ok()?;
    parse_dev(&text)
}

/// Whether the target's first 4 KiB hold anything but zeros (what `info` reports as `holds_data`).
pub fn holds_something(file: &File) -> bool {
    use std::os::unix::fs::FileExt;
    let mut buf = [0u8; 4096];
    match file.read_at(&mut buf, 0) {
        Ok(n) => buf[..n].iter().any(|&b| b != 0),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_t_splits_into_major_and_minor() {
        // 259:2 as the kernel encodes it: the minor in the low byte, the major above it.
        let dev = (259u64 << 8) | 2;
        assert_eq!(major_minor(dev), (259, 2));
    }

    #[test]
    fn a_mounted_partition_of_the_target_is_found_and_others_are_not() {
        let table = "\
22 1 0:21 / / rw - ext4 /dev/root rw
23 22 179:2 / /boot/firmware rw - vfat /dev/mmcblk0p1 rw
24 22 259:0 / /srv/data rw - ext4 /dev/nvme0n1 rw
";
        let no_parent = |_: (u32, u32)| None;
        // The SD card's whole disk (179:0) holds the mounted boot partition (179:2 here, a partition
        // of 179:0 by the disk map below).
        let parent = |(maj, min): (u32, u32)| {
            if (maj, min) == (179, 2) {
                Some((179, 0))
            } else {
                None
            }
        };
        assert_eq!(
            mounted_on(table, (179, 0), &parent).as_deref(),
            Some("/boot/firmware")
        );
        assert_eq!(
            mounted_on(table, (259, 0), &no_parent).as_deref(),
            Some("/srv/data")
        );
        assert_eq!(
            mounted_on(table, (8, 0), &no_parent),
            None,
            "nothing mounted on that device"
        );
        assert_eq!(
            mounted_on(table, (0, 21), &no_parent).as_deref(),
            Some("/"),
            "the running root"
        );
    }

    #[test]
    fn the_devices_of_an_mtd_partition_include_its_ubi_volumes() {
        let root = std::env::temp_dir().join(format!("lemnos-flash-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let class = root.join("class");
        fs::create_dir_all(class.join("ubi/ubi0_1")).unwrap();
        fs::create_dir_all(class.join("ubi/ubi0")).unwrap();
        fs::create_dir_all(class.join("ubi/ubi1")).unwrap();
        fs::write(class.join("ubi/ubi0/mtd_num"), "4\n").unwrap();
        fs::write(class.join("ubi/ubi1/mtd_num"), "5\n").unwrap();
        fs::write(class.join("ubi/ubi0_1/dev"), "254:3\n").unwrap();
        let devices = mtd_devices(&root, 4);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(devices, vec![(31, 4), (254, 3)]);
    }

    #[test]
    fn a_block_target_must_be_a_block_device_and_a_missing_one_is_said_so() {
        let missing = std::env::temp_dir().join("lemnos-flash-no-such-target");
        assert!(
            find(&missing, Kind::Block)
                .unwrap_err()
                .contains("is not there")
        );
    }
}
