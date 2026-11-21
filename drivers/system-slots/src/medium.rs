//! The storage medium the board booted from, and the check a slot's `medium` must pass.
//!
//! A slot's image is a file on the data partition: slot-init opens it through a loop device and
//! dm-verity (`docs/slots.md`, "How a slot boots"), and the data partition is what the board's
//! storage is. So the medium is the medium of the disk the data partition is mounted from:
//! `/proc/self/mountinfo` names its device, `/sys/dev/block/<major:minor>` its disk, and the
//! disk's controller says what it is. An SD card's host reports its card as `SD` and an eMMC as
//! `MMC` (`/sys/block/<disk>/device/type`); a raw flash disk is an MTD block device whose MTD is
//! `nor` or `nand` (`/sys/class/mtd/mtd<n>/type`). Anything else, a UBI volume among it, is unknown,
//! and a slot is then not refused on medium.
//!
//! The boot strap register is not read here: no driver maps it, and the controller the kernel
//! reports is what the board really read its data from.

use std::fs;
use std::path::Path;

/// The medium the board booted from, `None` when it can't be told.
pub fn booted() -> Option<&'static str> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    from_mounts(&mountinfo, Path::new("/sys"))
}

/// [`booted`] over a mount table's text and a sysfs root (`/sys`, or a stand-in in tests).
pub fn from_mounts(mountinfo: &str, sys: &Path) -> Option<&'static str> {
    let devnum = mount_device(mountinfo, "/data")?;
    let node = fs::canonicalize(sys.join("dev/block").join(devnum)).ok()?;
    // A partition's sysfs directory is inside its disk's.
    let disk = if node.join("partition").exists() {
        node.parent()?
    } else {
        node.as_path()
    };
    let name = disk.file_name()?.to_str()?;
    let card_type = read_trimmed(&sys.join("block").join(name).join("device/type"));
    let mtd_type = name
        .strip_prefix("mtdblock")
        .and_then(|n| read_trimmed(&sys.join("class/mtd").join(format!("mtd{n}")).join("type")));
    medium_of(name, card_type.as_deref(), mtd_type.as_deref())
}

/// The medium of a disk named `disk`, from its controller's type (`SD` or `MMC` for an mmc
/// block device, `nor` or `nand` for an MTD block device). `None` for any other.
pub fn medium_of(
    disk: &str,
    card_type: Option<&str>,
    mtd_type: Option<&str>,
) -> Option<&'static str> {
    let medium = if disk.starts_with("mmcblk") {
        match card_type? {
            "SD" => "sd",
            "MMC" => "emmc",
            _ => return None,
        }
    } else if disk.starts_with("mtdblock") {
        match mtd_type? {
            "nor" => "spi-nor",
            "nand" => "spi-nand",
            _ => return None,
        }
    } else {
        return None;
    };
    Some(medium)
}

/// The `major:minor` of the device mounted at `point`, from a mount table's text.
fn mount_device(mountinfo: &str, point: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(4) == Some(&point) {
            fields.get(2).map(|d| d.to_string())
        } else {
            None
        }
    })
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

/// The refusal for a slot whose `medium` is known and not the medium the board booted from: the
/// slot's name and both media, in words. `None` when they agree, or when either is unknown (an
/// empty string: the slot records none, or the board's medium could not be read).
pub fn mismatch(id: &str, slot: &str, booted: &str) -> Option<String> {
    if slot.is_empty() || booted.is_empty() || slot == booted {
        return None;
    }
    Some(format!(
        "slot {id} is for medium {slot}, and this board booted from {booted}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in sysfs: `/dev/block/179:2` links to a partition of `mmcblk0` whose card is `type`.
    fn sysfs(root: &Path, card_type: &str) {
        let disk = root.join("devices/platform/mmc/mmc_host/mmc1/mmc1:0001/block/mmcblk0");
        fs::create_dir_all(disk.join("mmcblk0p2")).unwrap();
        fs::write(disk.join("mmcblk0p2/partition"), "2\n").unwrap();
        fs::create_dir_all(root.join("dev/block")).unwrap();
        std::os::unix::fs::symlink(disk.join("mmcblk0p2"), root.join("dev/block/179:2")).unwrap();
        fs::create_dir_all(root.join("block/mmcblk0/device")).unwrap();
        fs::write(
            root.join("block/mmcblk0/device/type"),
            format!("{card_type}\n"),
        )
        .unwrap();
    }

    fn mounts(data: &str) -> String {
        format!(
            "21 1 0:20 / / rw - ext4 /dev/root rw\n\
             30 21 179:2 / /data rw,relatime shared:9 - ext4 /dev/mmcblk0p2 rw\n{data}"
        )
    }

    /// A stand-in sysfs directory, removed when the test ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("medium-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The data partition on an SD card reads as `sd`, on an eMMC as `emmc`.
    #[test]
    fn a_data_partition_on_an_sd_card_or_an_emmc_gives_its_medium() {
        let sd = Scratch::new("sd");
        sysfs(&sd.0, "SD");
        assert_eq!(from_mounts(&mounts(""), &sd.0), Some("sd"));
        let emmc = Scratch::new("emmc");
        sysfs(&emmc.0, "MMC");
        assert_eq!(from_mounts(&mounts(""), &emmc.0), Some("emmc"));
    }

    /// A card that is neither an SD card nor an eMMC, and a board with no data mount, are unknown
    /// rather than guessed.
    #[test]
    fn an_unknown_card_or_no_data_mount_is_unknown() {
        let root = Scratch::new("sdio");
        sysfs(&root.0, "SDIO");
        assert_eq!(from_mounts(&mounts(""), &root.0), None);
        assert_eq!(
            from_mounts("21 1 0:20 / / rw - ext4 /dev/root rw\n", &root.0),
            None
        );
    }

    /// A raw flash data partition is `spi-nor` or `spi-nand` by its MTD's type.
    #[test]
    fn the_mtd_type_gives_the_flash_medium() {
        assert_eq!(medium_of("mtdblock0", None, Some("nor")), Some("spi-nor"));
        assert_eq!(medium_of("mtdblock0", None, Some("nand")), Some("spi-nand"));
        assert_eq!(medium_of("mtdblock0", None, Some("ram")), None);
        assert_eq!(medium_of("mtdblock0", None, None), None);
        assert_eq!(medium_of("sda", Some("SD"), None), None);
        assert_eq!(medium_of("mmcblk0", None, None), None);
    }

    /// Every medium the function gives is one the manifest names.
    #[test]
    fn every_medium_given_is_a_known_one() {
        use lemnos_slot_store::manifest::MEDIUMS;
        for (disk, card, mtd) in [
            ("mmcblk0", Some("SD"), None),
            ("mmcblk0", Some("MMC"), None),
            ("mtdblock0", None, Some("nor")),
            ("mtdblock0", None, Some("nand")),
        ] {
            let medium = medium_of(disk, card, mtd).unwrap();
            assert!(MEDIUMS.contains(&medium), "{medium}");
        }
    }

    /// The check: a known slot medium that differs from a known booted one is named; agreeing or
    /// unknown on either side is not refused.
    #[test]
    fn a_different_known_medium_is_refused_by_name() {
        assert_eq!(
            mismatch("os-2", "emmc", "sd").unwrap(),
            "slot os-2 is for medium emmc, and this board booted from sd"
        );
        assert_eq!(mismatch("os-2", "sd", "sd"), None);
        assert_eq!(mismatch("os-2", "", "sd"), None, "the slot records none");
        assert_eq!(
            mismatch("os-2", "emmc", ""),
            None,
            "the boot medium is unknown"
        );
        assert_eq!(mismatch("os-2", "", ""), None);
        assert!(mismatch("os-2", "spi-nor", "spi-nand").is_some());
    }
}
