//! The boot disk: which whole disk the board booted from, found from what the boot chain states
//! and the device tree, never from the content of partitions.
//!
//! The boot chain states its source in the device tree's `chosen` node: U-Boot's `lemnos,boot-source`
//! (`<devtype><devnum>` of the medium it booted from, in U-Boot's own numbering), or on a Raspberry Pi
//! the firmware's `bootloader/boot-mode` (a big-endian u32). The board file's `boot_sources` table maps
//! each source to the device-tree path of its storage controller. The disk is the one whole disk under
//! that controller: sysfs gives each device the `of_node` link to its device-tree node, and a disk's
//! ancestors include its controller.
//!
//! Everything else on the boot disk is then found on that disk only: the boot partition (partition 1)
//! and the data partition by label, so a second disk carrying the same label is never used.

use std::fs;
use std::path::{Path, PathBuf};

/// The device tree's root in sysfs, under the sysfs root.
pub const DEVICE_TREE: &str = "firmware/devicetree/base";

/// One partition the kernel lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub number: u32,
    pub disk: String,
}

/// Every partition under `sys/class/block`, by name.
pub fn partitions(sys: &Path) -> Vec<Part> {
    let mut found: Vec<Part> = fs::read_dir(sys.join("class/block"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let real = fs::canonicalize(entry.path()).ok()?;
            let number = fs::read_to_string(real.join("partition"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            let disk = real.parent()?.file_name()?.to_string_lossy().into_owned();
            Some(Part { name, number, disk })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The names of the whole disks (block devices that are not partitions).
pub fn whole_disks(sys: &Path) -> Vec<String> {
    let mut disks: Vec<String> = fs::read_dir(sys.join("class/block"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| !entry.path().join("partition").exists())
        .filter(|entry| {
            fs::canonicalize(entry.path())
                .map(|real| !real.join("partition").exists())
                .unwrap_or(false)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    disks.sort();
    disks
}

/// The device-tree path of the node a sysfs device directory is bound to (its `of_node` link), if
/// it has one.
fn of_node_path(sys: &Path, dir: &Path) -> Option<String> {
    let target = fs::canonicalize(dir.join("of_node")).ok()?;
    let base = fs::canonicalize(sys.join(DEVICE_TREE)).ok()?;
    let rest = target.strip_prefix(&base).ok()?;
    Some(format!("/{}", rest.to_string_lossy()))
}

/// The device-tree paths of a disk's controllers, nearest first: the `of_node` of each sysfs
/// ancestor of the disk, from its own directory up to the devices root.
fn controllers_of(sys: &Path, disk: &str) -> Vec<String> {
    let Ok(real) = fs::canonicalize(sys.join("class/block").join(disk)) else {
        return Vec::new();
    };
    let Ok(devices) = fs::canonicalize(sys.join("devices")) else {
        return Vec::new();
    };
    real.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(&devices))
        .filter_map(|dir| of_node_path(sys, dir))
        .collect()
}

/// The source table of the board file: `source=device-tree path` entries. A source is either the
/// boot chain's `rpi-boot-mode-<n>` or U-Boot's device-tree path of the controller it booted from
/// (the `/mmc@...` form U-Boot's live tree gives); both are kept whole, never as a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub entries: Vec<(String, String)>,
}

impl Table {
    /// Parses the board file's `boot_sources` list. Each entry is `source=/device/tree/path`; a
    /// source is `rpi-boot-mode-<n>` or a device-tree path starting with `/`, with no spaces; a
    /// source may be listed once.
    pub fn parse(items: &[&str]) -> Result<Self, String> {
        let mut entries: Vec<(String, String)> = Vec::new();
        for item in items {
            let (source, path) = item
                .split_once('=')
                .ok_or_else(|| format!("boot_sources entry {item:?} is not source=path"))?;
            let plain = !source.is_empty()
                && source
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            let device_tree = source.starts_with('/')
                && source.len() > 1
                && source.bytes().all(|b| b.is_ascii_graphic());
            if !(plain || device_tree) {
                return Err(format!(
                    "boot_sources source {source:?} must be rpi-boot-mode-<n> or a device-tree path starting with /"
                ));
            }
            if !path.starts_with('/') {
                return Err(format!(
                    "boot_sources path {path:?} must be a device-tree path starting with /"
                ));
            }
            if entries.iter().any(|(s, _)| s == source) {
                return Err(format!("boot_sources lists {source:?} twice"));
            }
            entries.push((source.to_string(), path.trim_end_matches('/').to_string()));
        }
        Ok(Self { entries })
    }

    /// The controller the table gives for `source`, or the refusal by name.
    pub fn controller(&self, source: &str) -> Result<&str, String> {
        self.entries
            .iter()
            .find(|(s, _)| s == source)
            .map(|(_, path)| path.as_str())
            .ok_or_else(|| {
                let listed: Vec<&str> = self.entries.iter().map(|(s, _)| s.as_str()).collect();
                let listed = if listed.is_empty() { "none".into() } else { listed.join(", ") };
                format!(
                    "booted from {source}, which this board's boot_sources does not list (it lists: {listed})"
                )
            })
    }
}

/// The boot source the boot chain states: U-Boot's `lemnos,boot-source`, else the firmware's
/// `bootloader/boot-mode` as `rpi-boot-mode-<n>`. `chosen` is the `chosen` node's directory.
pub fn boot_source(chosen: &Path) -> Result<String, String> {
    if let Ok(text) = fs::read(chosen.join("lemnos,boot-source")) {
        let text = String::from_utf8_lossy(&text)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        if !text.is_empty() {
            return Ok(text);
        }
    }
    let boot_mode = fs::read(chosen.join("bootloader/boot-mode")).ok();
    if let Some(word) = boot_mode.and_then(|bytes| <[u8; 4]>::try_from(bytes.as_slice()).ok()) {
        return Ok(format!("rpi-boot-mode-{}", u32::from_be_bytes(word)));
    }
    Err("the boot chain states no boot source (chosen/lemnos,boot-source and chosen/bootloader/boot-mode are both missing): refusing to guess".into())
}

/// The boot disk for `source` on this board: the one whole disk under the controller the table
/// gives it. None or several are refused by name, with what was seen.
pub fn boot_disk(sys: &Path, table: &Table, source: &str) -> Result<String, String> {
    let controller = table.controller(source)?;
    let disks = whole_disks(sys);
    let hits: Vec<&String> = disks
        .iter()
        .filter(|disk| controllers_of(sys, disk).iter().any(|c| c == controller))
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => {
            let seen: Vec<String> = disks
                .iter()
                .map(|disk| {
                    format!(
                        "{disk} under {}",
                        controllers_of(sys, disk)
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "no controller".into())
                    )
                })
                .collect();
            Err(format!(
                "no whole disk sits under {controller} (boot source {source}); disks seen: {}",
                if seen.is_empty() {
                    "none".into()
                } else {
                    seen.join(", ")
                }
            ))
        }
        n => {
            let names: Vec<&str> = hits.iter().map(|d| d.as_str()).collect();
            Err(format!(
                "{n} whole disks sit under {controller} ({}): refusing to guess",
                names.join(", ")
            ))
        }
    }
}

/// The partition of `disk` with number `number`, under `sys`.
pub fn partition_of(sys: &Path, disk: &str, number: u32) -> Option<Part> {
    partitions(sys)
        .into_iter()
        .find(|p| p.disk == disk && p.number == number)
}

/// The device node of the boot partition of `disk`: partition 1, under `dev`.
pub fn boot_partition(sys: &Path, dev: &Path, disk: &str) -> Option<PathBuf> {
    partition_of(sys, disk, 1).map(|p| dev.join(p.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A fake sysfs under the scratch directory of this test run, removed when dropped.
    struct Sys(PathBuf);

    impl Sys {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("lemnos-disk-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("class/block")).unwrap();
            fs::create_dir_all(root.join("devices")).unwrap();
            fs::create_dir_all(root.join(DEVICE_TREE)).unwrap();
            Self(root)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// A device-tree node, as a directory with a `of_node` link to it (sysfs's form).
        fn controller(&self, dt: &str, dir: &str) -> PathBuf {
            let node = self
                .path()
                .join(DEVICE_TREE)
                .join(dt.trim_start_matches('/'));
            fs::create_dir_all(&node).unwrap();
            let device = self.path().join("devices").join(dir);
            fs::create_dir_all(&device).unwrap();
            symlink(&node, device.join("of_node")).unwrap();
            device
        }
        /// A whole disk `name` under `parent` (its controller's device directory), with its
        /// partitions `(name, number)`, as the kernel lays them out.
        fn disk(&self, parent: &Path, name: &str, parts: &[(&str, u32)]) {
            let dir = parent.join(name);
            fs::create_dir_all(&dir).unwrap();
            symlink(&dir, self.path().join("class/block").join(name)).unwrap();
            for (part, number) in parts {
                let pdir = dir.join(part);
                fs::create_dir_all(&pdir).unwrap();
                fs::write(pdir.join("partition"), format!("{number}\n")).unwrap();
                symlink(&pdir, self.path().join("class/block").join(part)).unwrap();
            }
        }
    }

    impl Drop for Sys {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn table() -> Table {
        Table::parse(&[
            "rpi-boot-mode-1=/soc/mmc@7e202000",
            "/mmc@fe2c0000=/axi/pcie@1000120000",
        ])
        .unwrap()
    }

    #[test]
    fn the_disk_under_the_named_controller_is_the_boot_disk() {
        let sys = Sys::new("one");
        let sd = sys.controller("/soc/mmc@7e202000", "sd-host");
        let nvme = sys.controller("/axi/pcie@1000120000", "pcie");
        sys.disk(&sd, "mmcblk0", &[("mmcblk0p1", 1), ("mmcblk0p2", 2)]);
        sys.disk(&nvme, "nvme0n1", &[("nvme0n1p1", 1)]);
        assert_eq!(
            boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap(),
            "mmcblk0"
        );
        assert_eq!(
            boot_disk(sys.path(), &table(), "/mmc@fe2c0000").unwrap(),
            "nvme0n1"
        );
        assert_eq!(
            boot_partition(sys.path(), Path::new("/dev"), "mmcblk0"),
            Some(PathBuf::from("/dev/mmcblk0p1"))
        );
    }

    #[test]
    fn a_source_the_table_does_not_list_is_refused_by_name() {
        let sys = Sys::new("unlisted");
        let err = boot_disk(sys.path(), &table(), "usb0").unwrap_err();
        assert_eq!(
            err,
            "booted from usb0, which this board's boot_sources does not list (it lists: rpi-boot-mode-1, /mmc@fe2c0000)"
        );
    }

    #[test]
    fn no_disk_or_two_disks_under_the_controller_are_refused_by_name() {
        let sys = Sys::new("none");
        let sd = sys.controller("/soc/mmc@7e202000", "sd-host");
        let other = sys.controller("/axi/pcie@1000120000", "pcie");
        sys.disk(&other, "nvme0n1", &[("nvme0n1p1", 1)]);
        let err = boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap_err();
        assert!(err.starts_with("no whole disk sits under /soc/mmc@7e202000 (boot source rpi-boot-mode-1); disks seen: nvme0n1 under /axi/pcie@1000120000"), "{err}");
        sys.disk(&sd, "mmcblk0", &[]);
        sys.disk(&sd, "mmcblk1", &[]);
        let err = boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap_err();
        assert_eq!(
            err,
            "2 whole disks sit under /soc/mmc@7e202000 (mmcblk0, mmcblk1): refusing to guess"
        );
    }

    #[test]
    fn a_foreign_disk_on_another_controller_is_not_the_boot_disk() {
        let sys = Sys::new("foreign");
        let sd = sys.controller("/soc/mmc@7e202000", "sd-host");
        let usb = sys.controller("/axi/usb@1", "usb");
        sys.disk(&sd, "mmcblk0", &[("mmcblk0p1", 1)]);
        sys.disk(&usb, "sda", &[("sda1", 1)]);
        assert_eq!(
            boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap(),
            "mmcblk0"
        );
    }

    #[test]
    fn the_source_is_the_u_boot_statement_then_the_firmware_boot_mode() {
        let sys = Sys::new("source");
        let chosen = sys.path().join("chosen");
        fs::create_dir_all(chosen.join("bootloader")).unwrap();
        assert!(boot_source(&chosen).unwrap_err().contains("both missing"));
        fs::write(chosen.join("bootloader/boot-mode"), [0, 0, 0, 1]).unwrap();
        assert_eq!(boot_source(&chosen).unwrap(), "rpi-boot-mode-1");
        fs::write(chosen.join("lemnos,boot-source"), b"/mmc@fe2b0000\0").unwrap();
        assert_eq!(boot_source(&chosen).unwrap(), "/mmc@fe2b0000");
    }

    #[test]
    fn the_table_refuses_bad_entries_by_name() {
        assert!(
            Table::parse(&["rpi-boot-mode-1"])
                .unwrap_err()
                .contains("is not source=path")
        );
        assert!(
            Table::parse(&["MMC0=/x"])
                .unwrap_err()
                .contains("rpi-boot-mode-<n>")
        );
        assert!(
            Table::parse(&["rpi-boot-mode-1=soc"])
                .unwrap_err()
                .contains("starting with /")
        );
        assert!(
            Table::parse(&["rpi-boot-mode-1=/a", "rpi-boot-mode-1=/b"])
                .unwrap_err()
                .contains("twice")
        );
    }
}
