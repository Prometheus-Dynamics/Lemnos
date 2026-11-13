//! Slots on raw NAND, over UBI: each slot's image is a static UBI volume instead of a file, and
//! the U-Boot record is a small static volume instead of a file on the boot partition. The data
//! and the boot directories are UBIFS volumes, mounted where the ext4 and FAT partitions would
//! be, so every other file stays where [`crate::store`] puts it.
//!
//! Volume names on the UBI device (`ubi0`, say):
//!
//! ```text
//! data                 UBIFS: the data directory (/data)
//! boot                 UBIFS: the boot directory, slots/<id>/ with each slot's kernel and extlinux.conf
//! slot-<id>            static: the slot's EROFS image with its hash tree, exactly image.size bytes
//! slot-new-<n>         static: an image being written by install job n
//! boot-record          static: the U-Boot record, as boot-record.env's text
//! boot-record-new      static: the next record, before it is renamed over boot-record
//! ```
//!
//! What a volume is comes from sysfs (`class/ubi/ubi0_N/name`, `data_bytes`), so reading needs no
//! system calls. Changing a volume goes through [`UbiWrite`]: [`System`] on a board, [`Fake`] in
//! tests. Every replacement is written to a new volume and renamed over the old one, which UBI
//! does in one atomic step, so a power cut leaves the old volume or the new one.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The data directory's volume.
pub const DATA_VOLUME: &str = "data";
/// The boot directory's volume.
pub const BOOT_VOLUME: &str = "boot";
/// The U-Boot record's volume.
pub const RECORD_VOLUME: &str = "boot-record";
/// What an image volume's name starts with.
pub const IMAGE_PREFIX: &str = "slot-";
/// What an install's work volume's name starts with.
pub const WORK_PREFIX: &str = "slot-new-";

/// Slot `id`'s image volume.
pub fn image_volume(id: &str) -> String {
    format!("{IMAGE_PREFIX}{id}")
}

/// Install job `job`'s work volume.
pub fn work_volume(job: u64) -> String {
    format!("{WORK_PREFIX}{job}")
}

/// A UBI device: its sysfs, its device nodes, and its name (`ubi0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ubi {
    /// Where sysfs is (`/sys`).
    pub sys: PathBuf,
    /// Where the device nodes are (`/dev`).
    pub dev: PathBuf,
    /// The device's name.
    pub device: String,
}

/// One volume, as sysfs shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub number: i32,
    pub name: String,
    /// What it holds: for a static volume, what was last written whole.
    pub bytes: u64,
    /// Its character device (`/dev/ubi0_N`).
    pub node: PathBuf,
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

impl Ubi {
    fn class(&self) -> PathBuf {
        self.sys.join("class/ubi")
    }

    /// Whether the device is there.
    pub fn present(&self) -> bool {
        self.class().join(&self.device).is_dir()
    }

    /// Every volume on the device.
    pub fn volumes(&self) -> Vec<Volume> {
        let prefix = format!("{}_", self.device);
        let mut found: Vec<Volume> = fs::read_dir(self.class())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let dir = entry.file_name().to_string_lossy().into_owned();
                let number = dir.strip_prefix(&prefix)?.parse().ok()?;
                let at = entry.path();
                Some(Volume {
                    number,
                    name: read_trim(&at.join("name"))?,
                    bytes: read_trim(&at.join("data_bytes"))?.parse().ok()?,
                    node: self.dev.join(&dir),
                })
            })
            .collect();
        found.sort_by_key(|v| v.number);
        found
    }

    /// The volume named `name`.
    pub fn volume(&self, name: &str) -> Option<Volume> {
        self.volumes().into_iter().find(|v| v.name == name)
    }

    /// The bytes free for new volumes.
    ///
    /// # Errors
    ///
    /// The device's sysfs can't be read.
    pub fn free(&self) -> io::Result<u64> {
        let at = self.class().join(&self.device);
        let number = |name: &str| -> io::Result<u64> {
            read_trim(&at.join(name))
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| io::Error::other(format!("{}/{name}", at.display())))
        };
        Ok(number("avail_eraseblocks")?.saturating_mul(number("eraseblock_size")?))
    }

    /// What volume `name` holds.
    ///
    /// # Errors
    ///
    /// There is no such volume, or it can't be read.
    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        let volume = self
            .volume(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no volume {name}")))?;
        let mut out = Vec::new();
        File::open(&volume.node)?
            .take(volume.bytes)
            .read_to_end(&mut out)?;
        Ok(out)
    }
}

/// What changes volumes. Every name is a volume name on the one device.
pub trait UbiWrite: Send + Sync {
    /// Makes a static volume of `bytes` bytes.
    fn make(&self, name: &str, bytes: u64) -> io::Result<()>;
    /// Opens the volume to be written whole: exactly `size` bytes written to the returned file
    /// are its new contents.
    fn update(&self, name: &str, size: u64) -> io::Result<File>;
    /// Renames `from` to `to` in one atomic step, removing a volume already called `to`.
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;
    fn remove(&self, name: &str) -> io::Result<()>;
    /// A read-only block device over the volume: its node.
    fn block(&self, name: &str) -> io::Result<PathBuf>;
}

/// Replaces volume `name` with `bytes`: written whole to `<name>-new`, then renamed over it.
///
/// # Errors
///
/// A step failed; the old volume is then as it was.
pub fn replace(ubi: &Ubi, writer: &dyn UbiWrite, name: &str, bytes: &[u8]) -> io::Result<()> {
    let new = format!("{name}-new");
    if ubi.volume(&new).is_some() {
        writer.remove(&new)?;
    }
    let len = bytes.len() as u64;
    writer.make(&new, len.max(1))?;
    let mut file = writer.update(&new, len)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    writer.rename(&new, name)
}

#[cfg(any(test, feature = "fake", all(feature = "linux", target_os = "linux")))]
fn not_found(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no volume {name}"))
}

/// [`UbiWrite`] on a board: the UBI ioctls through lemnos-linux-sys.
#[cfg(all(feature = "linux", target_os = "linux"))]
#[derive(Debug, Clone)]
pub struct System {
    pub ubi: Ubi,
}

#[cfg(all(feature = "linux", target_os = "linux"))]
impl System {
    fn device(&self) -> io::Result<File> {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.ubi.dev.join(&self.ubi.device))
    }

    fn find(&self, name: &str) -> io::Result<Volume> {
        self.ubi.volume(name).ok_or_else(|| not_found(name))
    }
}

#[cfg(all(feature = "linux", target_os = "linux"))]
impl UbiWrite for System {
    fn make(&self, name: &str, bytes: u64) -> io::Result<()> {
        lemnos_linux_sys::ubi::make_volume(&self.device()?, name, bytes, true).map(drop)
    }

    fn update(&self, name: &str, size: u64) -> io::Result<File> {
        let volume = self.find(name)?;
        let file = fs::OpenOptions::new().write(true).open(&volume.node)?;
        lemnos_linux_sys::ubi::begin_update(&file, size)?;
        Ok(file)
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let volume = self.find(from)?;
        lemnos_linux_sys::ubi::rename_volumes(&self.device()?, &[(volume.number, to)])
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        let volume = self.find(name)?;
        lemnos_linux_sys::ubi::remove_volume(&self.device()?, volume.number)
    }

    fn block(&self, name: &str) -> io::Result<PathBuf> {
        let volume = self.find(name)?;
        let node = self.ubi.dev.join(format!(
            "ubiblock{}_{}",
            self.ubi.device.trim_start_matches("ubi"),
            volume.number
        ));
        if !node.exists() {
            lemnos_linux_sys::ubi::create_block(&File::open(&volume.node)?)?;
        }
        Ok(node)
    }
}

/// [`UbiWrite`] over a directory tree standing in for sysfs and `/dev`: a volume is a sysfs
/// directory with `name` and `data_bytes` and a plain file as its node. For tests; a step named
/// in `refuse` (`make`, `update`, `rename`, `remove`, `block`) fails.
#[cfg(any(test, feature = "fake"))]
#[derive(Debug)]
pub struct Fake {
    pub ubi: Ubi,
    pub refuse: std::sync::Mutex<Vec<&'static str>>,
}

#[cfg(any(test, feature = "fake"))]
impl Fake {
    /// A device with `free` bytes in 128 KiB erase blocks, and no volumes.
    pub fn new(ubi: Ubi, free: u64) -> Self {
        let at = ubi.class().join(&ubi.device);
        fs::create_dir_all(&at).unwrap();
        fs::create_dir_all(&ubi.dev).unwrap();
        fs::write(at.join("eraseblock_size"), "131072\n").unwrap();
        fs::write(
            at.join("avail_eraseblocks"),
            format!("{}\n", free / 131_072),
        )
        .unwrap();
        fs::write(ubi.dev.join(&ubi.device), b"").unwrap();
        Self {
            ubi,
            refuse: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Makes `name` holding `bytes` at once, as a test's starting state.
    pub fn put(&self, name: &str, bytes: &[u8]) {
        self.make(name, bytes.len() as u64).unwrap();
        self.update(name, bytes.len() as u64)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn step(&self, what: &str) -> io::Result<()> {
        if self.refuse.lock().unwrap().contains(&what) {
            Err(io::Error::other(format!("{what} refused by the test")))
        } else {
            Ok(())
        }
    }

    fn dir(&self, number: i32) -> PathBuf {
        self.ubi
            .class()
            .join(format!("{}_{number}", self.ubi.device))
    }

    fn find(&self, name: &str) -> io::Result<Volume> {
        self.ubi.volume(name).ok_or_else(|| not_found(name))
    }
}

#[cfg(any(test, feature = "fake"))]
impl UbiWrite for Fake {
    fn make(&self, name: &str, bytes: u64) -> io::Result<()> {
        self.step("make")?;
        if self.ubi.volume(name).is_some() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let blocks = bytes.div_ceil(131_072);
        let at = self
            .ubi
            .class()
            .join(&self.ubi.device)
            .join("avail_eraseblocks");
        let avail: u64 = read_trim(&at).and_then(|s| s.parse().ok()).unwrap_or(0);
        if blocks > avail {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        fs::write(&at, format!("{}\n", avail - blocks))?;
        let number = (0..).find(|n| !self.dir(*n).exists()).unwrap_or(0);
        let dir = self.dir(number);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("name"), format!("{name}\n"))?;
        fs::write(dir.join("data_bytes"), "0\n")?;
        fs::write(
            dir.join("reserved_bytes"),
            format!("{}\n", blocks * 131_072),
        )?;
        fs::write(
            self.ubi.dev.join(format!("{}_{number}", self.ubi.device)),
            b"",
        )
    }

    fn update(&self, name: &str, size: u64) -> io::Result<File> {
        self.step("update")?;
        let volume = self.find(name)?;
        fs::write(
            self.dir(volume.number).join("data_bytes"),
            format!("{size}\n"),
        )?;
        File::create(&volume.node)
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.step("rename")?;
        let volume = self.find(from)?;
        if let Some(old) = self.ubi.volume(to) {
            self.remove(&old.name)?;
        }
        fs::write(self.dir(volume.number).join("name"), format!("{to}\n"))
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        self.step("remove")?;
        let volume = self.find(name)?;
        let dir = self.dir(volume.number);
        let reserved: u64 = read_trim(&dir.join("reserved_bytes"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let at = self
            .ubi
            .class()
            .join(&self.ubi.device)
            .join("avail_eraseblocks");
        let avail: u64 = read_trim(&at).and_then(|s| s.parse().ok()).unwrap_or(0);
        fs::write(&at, format!("{}\n", avail + reserved / 131_072))?;
        fs::remove_dir_all(dir)?;
        fs::remove_file(&volume.node)
    }

    fn block(&self, name: &str) -> io::Result<PathBuf> {
        self.step("block")?;
        let volume = self.find(name)?;
        Ok(self.ubi.dev.join(format!(
            "ubiblock{}_{}",
            self.ubi.device.trim_start_matches("ubi"),
            volume.number
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;

    fn device(dir: &TempDir) -> Fake {
        Fake::new(
            Ubi {
                sys: dir.0.join("sys"),
                dev: dir.0.join("dev"),
                device: "ubi0".into(),
            },
            1 << 20,
        )
    }

    #[test]
    fn volumes_are_read_from_sysfs() {
        let dir = TempDir::new("ubi-read");
        let fake = device(&dir);
        fake.put("data", b"");
        fake.put(&image_volume("os-1"), b"image bytes");
        let ubi = &fake.ubi;
        let image = ubi.volume("slot-os-1").unwrap();
        assert_eq!((image.number, image.bytes), (1, 11));
        assert_eq!(image.node, dir.0.join("dev/ubi0_1"));
        assert_eq!(ubi.read("slot-os-1").unwrap(), b"image bytes");
        assert!(ubi.read("slot-os-9").is_err());
        assert_eq!(ubi.free().unwrap(), (8 - 1) * 131_072);
        assert!(ubi.present());
    }

    #[test]
    fn a_replacement_is_written_beside_and_renamed_over() {
        let dir = TempDir::new("ubi-replace");
        let fake = device(&dir);
        replace(&fake.ubi, &fake, RECORD_VOLUME, b"default=a\n").unwrap();
        replace(&fake.ubi, &fake, RECORD_VOLUME, b"default=b\n").unwrap();
        assert_eq!(fake.ubi.read(RECORD_VOLUME).unwrap(), b"default=b\n");
        let names: Vec<String> = fake.ubi.volumes().into_iter().map(|v| v.name).collect();
        assert_eq!(names, [RECORD_VOLUME]);
        // A rename that fails leaves the old record.
        fake.refuse.lock().unwrap().push("rename");
        assert!(replace(&fake.ubi, &fake, RECORD_VOLUME, b"default=c\n").is_err());
        assert_eq!(fake.ubi.read(RECORD_VOLUME).unwrap(), b"default=b\n");
        // The leftover beside it goes on the next try.
        fake.refuse.lock().unwrap().clear();
        replace(&fake.ubi, &fake, RECORD_VOLUME, b"default=d\n").unwrap();
        assert_eq!(fake.ubi.volumes().len(), 1);
    }
}
