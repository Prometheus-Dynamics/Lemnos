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

/// Whether `name` has the form the kernel's MMC block driver gives an eMMC's hardware areas:
/// `mmcblk<host><area>`, the area being `boot0`, `boot1`, `gp0` to `gp3` or `rpmb`
/// (drivers/mmc/core/block.c, `mmc_blk_alloc_req`; udev's persistent-storage rules use the same
/// names). The user area is `mmcblk<host>`. Returns the user area's name.
pub fn emmc_area_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mmcblk")?;
    let host = rest.bytes().take_while(u8::is_ascii_digit).count();
    if host == 0 {
        return None;
    }
    let area = &rest[host..];
    let numbered = |prefix: &str| {
        area.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    (area == "rpmb" || numbered("boot") || numbered("gp"))
        .then(|| format!("mmcblk{}", &rest[..host]))
}

/// Why a block device that is not a partition is still not a disk the boot can be on, or `None`
/// when it is one. `real` is its sysfs directory. Each rule is a fact the kernel gives:
///
/// - **An eMMC hardware area.** The MMC driver makes the card's boot areas, its general-purpose
///   areas and RPMB block devices of their own, registered as children of the user area's disk
///   (`mmc_blk_alloc_part` passes the user area's device as their parent): in sysfs their
///   directories sit inside the user area's, as a partition's does, but with no partition file.
///   One is set aside when its name is an area's ([`emmc_area_of`]) *and* its sysfs parent is that
///   user area's block device (named `mmcblk<host>`, with a `dev` file). A real disk is never set
///   aside by this: a disk's parent is a controller or a card, never another disk, and no other
///   driver names its disks `mmcblk`.
/// - **Hidden** (`hidden` reads 1): a path the kernel keeps behind another disk (an NVMe
///   multipath controller's own view of a namespace); the disk to use is the shared one.
/// - **No medium** (`size` reads 0): a card-reader slot with no card, or a removable drive with no
///   medium in it; nothing can be booted from it. A disk whose medium arrives later is seen then.
///
/// Virtual devices (loop, RAM, zram, device-mapper) sit under `devices/virtual` with no controller,
/// so they never match a controller; two real disks under one controller (two USB disks behind one
/// host controller, two NVMe namespaces) are not set aside: the boot refuses to guess between them.
pub fn set_aside(name: &str, real: &Path) -> Option<String> {
    if real.components().any(|c| c.as_os_str() == "virtual") {
        return None;
    }
    if let Some(user) = emmc_area_of(name)
        && real.parent().is_some_and(|parent| {
            parent.file_name().is_some_and(|n| n == user.as_str()) && parent.join("dev").is_file()
        })
    {
        let area = &name[user.len()..];
        return Some(format!(
            "{name}: the hardware {area} area of the eMMC {user}"
        ));
    }
    let read = |attribute: &str| fs::read_to_string(real.join(attribute)).ok();
    if read("hidden").is_some_and(|text| text.trim() == "1") {
        return Some(format!(
            "{name}: hidden by the kernel (a path behind another disk)"
        ));
    }
    if read("size").is_some_and(|text| text.trim() == "0") {
        return Some(format!("{name}: no medium (size 0)"));
    }
    None
}

/// The block devices that are not partitions, split into the disks the boot can be on and those
/// set aside, each with its reason ([`set_aside`]). Both sorted by name.
pub fn block_disks(sys: &Path) -> (Vec<String>, Vec<String>) {
    let mut disks = Vec::new();
    let mut aside = Vec::new();
    for entry in fs::read_dir(sys.join("class/block"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.path().join("partition").exists() {
            continue;
        }
        let Ok(real) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if real.join("partition").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        match set_aside(&name, &real) {
            Some(why) => aside.push(why),
            None => disks.push(name),
        }
    }
    disks.sort();
    aside.sort();
    (disks, aside)
}

/// The names of the disks the boot can be on: [`block_disks`] less those it sets aside.
pub fn whole_disks(sys: &Path) -> Vec<String> {
    block_disks(sys).0
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

/// What a look for the boot disk found (see [`find_boot_disk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootDisk {
    /// The one whole disk under the source's controller.
    One(String),
    /// No whole disk sits under the controller yet: the sentence names the controller and every
    /// disk seen with its controller. A controller registers its disk some time after it probes
    /// (an eMMC's or SD card's initialisation, a USB disk's enumeration), so this one is worth a wait.
    NoneYet(String),
    /// A refusal no wait changes: the table does not list the source, or two disks sit under it.
    Refused(String),
}

/// The boot disk for `source` on this board: the one whole disk under the controller the table
/// gives it. None or several are refused by name, with what was seen.
pub fn boot_disk(sys: &Path, table: &Table, source: &str) -> Result<String, String> {
    match find_boot_disk(sys, table, source) {
        BootDisk::One(disk) => Ok(disk),
        BootDisk::NoneYet(why) | BootDisk::Refused(why) => Err(why),
    }
}

/// One look for the boot disk for `source`, telling "none yet" (worth a wait) apart from a refusal.
pub fn find_boot_disk(sys: &Path, table: &Table, source: &str) -> BootDisk {
    let controller = match table.controller(source) {
        Ok(controller) => controller,
        Err(why) => return BootDisk::Refused(why),
    };
    let (disks, aside) = block_disks(sys);
    let aside = if aside.is_empty() {
        String::new()
    } else {
        format!("; set aside: {}", aside.join("; "))
    };
    let hits: Vec<&String> = disks
        .iter()
        .filter(|disk| controllers_of(sys, disk).iter().any(|c| c == controller))
        .collect();
    match hits.len() {
        1 => BootDisk::One(hits[0].clone()),
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
            BootDisk::NoneYet(format!(
                "no whole disk sits under {controller} (boot source {source}); disks seen: {}{aside}",
                if seen.is_empty() {
                    "none".into()
                } else {
                    seen.join(", ")
                }
            ))
        }
        n => {
            let names: Vec<&str> = hits.iter().map(|d| d.as_str()).collect();
            BootDisk::Refused(format!(
                "{n} whole disks sit under {controller} ({}): refusing to guess{aside}",
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

    /// The reference board's block devices exactly as its sysfs lists them (a RAM-boot listing of
    /// `/sys/class/block` with each device's real path and attributes): the controller's `of_node`
    /// is on `mmc_host`; the card's user area `mmcblk0` is in the card's `block` directory; its boot
    /// areas sit inside `mmcblk0`'s directory, as its partitions do, with no partition file
    /// (`force_ro` and `ro` 1, 8192 sectors each); loops and device-mapper under `virtual`.
    fn emmc_board(sys: &Sys) {
        let host = sys.controller(
            "/soc@107c000000/mmc@fff000",
            "platform/soc@107c000000/1000fff000.mmc/mmc_host",
        );
        let card = host.join("mmc0/mmc0:0001/block");
        sys.disk(&card, "mmcblk0", &[("mmcblk0p1", 1), ("mmcblk0p2", 2)]);
        let user = card.join("mmcblk0");
        sys.disk(&user, "mmcblk0boot0", &[]);
        sys.disk(&user, "mmcblk0boot1", &[]);
        let attrs = |dir: &Path, list: &[(&str, &str)]| {
            for (attribute, value) in list {
                fs::write(dir.join(attribute), format!("{value}\n")).unwrap();
            }
        };
        attrs(
            &user,
            &[
                ("size", "30535680"),
                ("hidden", "0"),
                ("removable", "0"),
                ("ro", "0"),
                ("force_ro", "0"),
                ("ext_range", "256"),
                ("dev", "179:0"),
            ],
        );
        for (area, dev) in [("mmcblk0boot0", "179:32"), ("mmcblk0boot1", "179:64")] {
            attrs(
                &user.join(area),
                &[
                    ("size", "8192"),
                    ("hidden", "0"),
                    ("removable", "0"),
                    ("ro", "1"),
                    ("force_ro", "1"),
                    ("ro_lock_until_next_power_on", "0"),
                    ("ext_range", "256"),
                    ("dev", dev),
                ],
            );
        }
        attrs(
            &user.join("mmcblk0p1"),
            &[("size", "161792"), ("ro", "0"), ("dev", "179:1")],
        );
        attrs(
            &user.join("mmcblk0p2"),
            &[("size", "30371840"), ("ro", "0"), ("dev", "179:2")],
        );
        let virt = sys.path().join("devices/virtual/block");
        fs::create_dir_all(&virt).unwrap();
        sys.disk(&virt, "dm-0", &[]);
        attrs(
            &virt.join("dm-0"),
            &[
                ("size", "81992"),
                ("hidden", "0"),
                ("ro", "1"),
                ("ext_range", "1"),
                ("dev", "254:0"),
            ],
        );
        for n in 0..8 {
            let name = format!("loop{n}");
            sys.disk(&virt, &name, &[]);
            let (size, ro) = if n == 0 { ("82656", "1") } else { ("0", "0") };
            let dev = format!("7:{n}");
            attrs(
                &virt.join(&name),
                &[
                    ("size", size),
                    ("hidden", "0"),
                    ("ro", ro),
                    ("ext_range", "256"),
                    ("dev", &dev),
                ],
            );
        }
    }

    fn emmc_table() -> Table {
        Table::parse(&["rpi-boot-mode-1=/soc@107c000000/mmc@fff000"]).unwrap()
    }

    /// The board's own shape: the eMMC's boot areas sit beside its user area, under the same card,
    /// with no partition file. Counted as disks they made three under the controller, and the boot
    /// was refused at every boot ("3 whole disks ... refusing to guess").
    #[test]
    fn on_the_board_s_emmc_the_user_area_is_the_boot_disk() {
        let sys = Sys::new("emmc-board");
        emmc_board(&sys);
        assert_eq!(
            boot_disk(sys.path(), &emmc_table(), "rpi-boot-mode-1").unwrap(),
            "mmcblk0"
        );
        let (disks, aside) = block_disks(sys.path());
        assert!(
            disks.contains(&"mmcblk0".to_string())
                && disks
                    .iter()
                    .all(|d| d == "mmcblk0" || d.starts_with("loop") || d == "dm-0"),
            "{disks:?}"
        );
        assert_eq!(
            aside,
            [
                "mmcblk0boot0: the hardware boot0 area of the eMMC mmcblk0",
                "mmcblk0boot1: the hardware boot1 area of the eMMC mmcblk0"
            ]
        );
    }

    /// A genuine two-disk case is still refused, and the refusal names what was set aside and why.
    #[test]
    fn two_real_disks_are_still_refused_and_the_refusal_names_what_was_set_aside() {
        let sys = Sys::new("emmc-two");
        emmc_board(&sys);
        let card = sys
            .path()
            .join("devices/platform/soc@107c000000/1000fff000.mmc/mmc_host/mmc0/mmc0:0001/block");
        sys.disk(&card, "mmcblk9", &[]);
        let why = boot_disk(sys.path(), &emmc_table(), "rpi-boot-mode-1").unwrap_err();
        assert_eq!(
            why,
            "2 whole disks sit under /soc@107c000000/mmc@fff000 (mmcblk0, mmcblk9): refusing to guess; \
             set aside: mmcblk0boot0: the hardware boot0 area of the eMMC mmcblk0; \
             mmcblk0boot1: the hardware boot1 area of the eMMC mmcblk0"
        );
    }

    /// An area name with no user area beside it is not set aside: the name alone decides nothing.
    #[test]
    fn an_area_name_without_its_user_area_beside_it_is_counted() {
        let sys = Sys::new("emmc-orphan");
        let host = sys.controller("/soc/mmc@7e202000", "sd-host");
        sys.disk(&host, "mmcblk1boot0", &[]);
        assert_eq!(whole_disks(sys.path()), ["mmcblk1boot0"]);
        assert_eq!(
            boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap(),
            "mmcblk1boot0"
        );
    }

    /// Behind one USB controller: the stick that holds the system, and a card reader's empty slot
    /// (no medium, size 0). The empty slot is set aside; a hidden multipath view too.
    #[test]
    fn an_empty_reader_slot_and_a_hidden_path_are_set_aside_by_their_sysfs_facts() {
        let sys = Sys::new("usb-reader");
        let usb = sys.controller("/soc/mmc@7e202000", "usb-host");
        sys.disk(&usb, "sda", &[("sda1", 1), ("sda2", 2)]);
        sys.disk(&usb, "sdb", &[]);
        sys.disk(&usb, "nvme0c0n1", &[]);
        fs::write(usb.join("sda/size"), "30000000\n").unwrap();
        fs::write(usb.join("sdb/size"), "0\n").unwrap();
        fs::write(usb.join("nvme0c0n1/hidden"), "1\n").unwrap();
        assert_eq!(
            boot_disk(sys.path(), &table(), "rpi-boot-mode-1").unwrap(),
            "sda"
        );
        assert_eq!(
            block_disks(sys.path()).1,
            [
                "nvme0c0n1: hidden by the kernel (a path behind another disk)",
                "sdb: no medium (size 0)"
            ]
        );
    }

    #[test]
    fn only_the_kernel_s_area_names_have_a_user_area() {
        for (name, user) in [
            ("mmcblk0boot0", "mmcblk0"),
            ("mmcblk1boot1", "mmcblk1"),
            ("mmcblk0gp0", "mmcblk0"),
            ("mmcblk12gp3", "mmcblk12"),
            ("mmcblk0rpmb", "mmcblk0"),
        ] {
            assert_eq!(emmc_area_of(name).as_deref(), Some(user), "{name}");
        }
        for name in [
            "mmcblk0",
            "mmcblk10",
            "mmcblk0p1",
            "sda",
            "nvme0n1",
            "mmcblkboot0",
            "mmcblk0boot",
            "mmcblk0bootx",
        ] {
            assert_eq!(emmc_area_of(name), None, "{name}");
        }
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
