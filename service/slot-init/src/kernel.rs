//! The kernel pieces the boot chain needs, and which of them a kernel lacks.
//!
//! A slot boots through a fixed chain: the storage device holds the data partition (its
//! filesystem is ext4, or UBIFS on raw NAND); the image is a file there, read through a loop
//! device (on NAND, a UBI volume read through ubiblock); device-mapper and its verity target
//! check the image; the verity target hashes with SHA-256; and the image is EROFS. Each piece is
//! a kernel option, and a board's kernel builds all of them in (`docs/kernel-config.md`, "Slot
//! path"). The initramfs carries no kernel modules: a board whose kernel has one of these as a
//! module does not boot a slot until the kernel is rebuilt with it built in.
//!
//! [`missing`] is pure: the [`Facts`] in, the absent pieces out. The boot reads the facts from
//! `/proc`, `/sys` and the device-mapper control node ([`crate::boot`]).

/// Which chain a board boots through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The data partition is an ext4 filesystem and the image a file on it.
    Stock,
    /// The data and boot partitions are UBIFS volumes on raw NAND, the image a UBI volume.
    Nand,
}

/// What the running kernel offers, read from the proc files and the device-mapper control node.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The filesystem names in `/proc/filesystems`.
    pub filesystems: Vec<String>,
    /// The names in `/proc/misc`.
    pub misc: Vec<String>,
    /// The names in `/proc/devices`, block and character.
    pub drivers: Vec<String>,
    /// The algorithm names in `/proc/crypto`.
    pub crypto: Vec<String>,
    /// The device-mapper targets, when the control node answers.
    pub dm_targets: Option<Vec<String>>,
    /// The UBI device the board's data is on (raw NAND).
    pub ubi: bool,
}

/// One piece of the chain.
#[derive(Debug, Clone, Copy)]
pub struct Piece {
    /// The kernel option that builds it in, without `CONFIG_`.
    pub option: &'static str,
    /// What it is for, in a few words, for the message.
    pub needed_for: &'static str,
    /// Whether the boot cannot carry on without it. A missing optional piece is logged and the
    /// boot carries on as it always did.
    pub required: bool,
    present: fn(&Facts) -> bool,
}

impl Piece {
    /// Whether the kernel has the piece.
    pub fn present(&self, facts: &Facts) -> bool {
        (self.present)(facts)
    }

    /// The line for the console, the kept log and the boot record: the option by name.
    pub fn message(&self) -> String {
        format!(
            "the kernel lacks CONFIG_{}: {}",
            self.option, self.needed_for
        )
    }
}

fn has(names: &[String], want: &str) -> bool {
    names.iter().any(|name| name == want)
}

/// The pieces of `route`, in the order the boot needs them.
pub fn pieces(route: Route) -> Vec<Piece> {
    let mut all = vec![
        Piece {
            option: "BLK_DEV_DM",
            needed_for: "device-mapper, which holds the verity device",
            required: true,
            present: |f| has(&f.misc, "device-mapper"),
        },
        Piece {
            option: "CRYPTO_SHA256",
            needed_for: "the SHA-256 that dm-verity checks the image with",
            required: true,
            present: |f| has(&f.crypto, "sha256"),
        },
        Piece {
            option: "DM_VERITY",
            needed_for: "the verity target over the slot's image",
            required: true,
            present: |f| f.dm_targets.as_ref().is_some_and(|t| has(t, "verity")),
        },
        Piece {
            option: "EROFS_FS",
            needed_for: "the slot's root filesystem, an EROFS image",
            required: true,
            present: |f| has(&f.filesystems, "erofs"),
        },
    ];
    match route {
        Route::Stock => {
            all.push(Piece {
                option: "BLK_DEV_LOOP",
                needed_for: "the loop device the image file is read through",
                required: true,
                present: |f| has(&f.drivers, "loop"),
            });
            all.push(Piece {
                option: "EXT4_FS",
                needed_for: "the data partition's filesystem",
                required: true,
                present: |f| has(&f.filesystems, "ext4"),
            });
            all.push(Piece {
                option: "VFAT_FS",
                needed_for: "the boot partition, with the boot records",
                required: false,
                present: |f| has(&f.filesystems, "vfat"),
            });
        }
        Route::Nand => {
            all.push(Piece {
                option: "MTD_UBI",
                needed_for: "the UBI device the data, boot and slot volumes are on",
                required: true,
                present: |f| f.ubi,
            });
            all.push(Piece {
                option: "UBIFS_FS",
                needed_for: "the data and boot volumes' filesystem",
                required: true,
                present: |f| has(&f.filesystems, "ubifs"),
            });
        }
    }
    all
}

/// The pieces of `route` that `facts` show absent, in order.
pub fn missing(route: Route, facts: &Facts) -> Vec<Piece> {
    pieces(route)
        .into_iter()
        .filter(|piece| !piece.present(facts))
        .collect()
}

/// The line for a data partition on a board with no block device at all: the storage driver is
/// the piece that is absent.
pub fn storage_message() -> String {
    "the kernel lacks CONFIG_MMC_BLOCK (or CONFIG_BLK_DEV_NVME): no block device holds the data partition"
        .to_string()
}

/// The last word of each line, which is the name in `/proc/misc`, `/proc/devices` and
/// `/proc/filesystems` (the filesystem's name follows `nodev` or a tab).
fn last_words(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_whitespace().last())
        .map(str::to_string)
        .collect()
}

/// The filesystem names in `/proc/filesystems`.
pub fn filesystems(text: &str) -> Vec<String> {
    last_words(text)
}

/// The names in `/proc/misc`.
pub fn misc(text: &str) -> Vec<String> {
    last_words(text)
}

/// The driver names in `/proc/devices`.
pub fn drivers(text: &str) -> Vec<String> {
    // The section headers ("Block devices:") end in a colon and name no driver.
    last_words(text)
        .into_iter()
        .filter(|word| !word.ends_with(':'))
        .collect()
}

/// The algorithm names in `/proc/crypto`: the value of each `name` line.
pub fn crypto(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.trim() == "name")
        .map(|(_, value)| value.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stock board's kernel with every piece built in.
    fn whole() -> Facts {
        Facts {
            filesystems: vec!["ext4".into(), "vfat".into(), "erofs".into()],
            misc: vec!["device-mapper".into()],
            drivers: vec!["loop".into()],
            crypto: vec!["sha256".into()],
            dm_targets: Some(vec!["verity".into()]),
            ubi: false,
        }
    }

    fn absent(route: Route, facts: &Facts) -> Vec<&'static str> {
        missing(route, facts).iter().map(|p| p.option).collect()
    }

    #[test]
    fn a_kernel_with_every_piece_lacks_none() {
        assert!(absent(Route::Stock, &whole()).is_empty());
    }

    #[test]
    fn each_missing_piece_is_named_by_its_option() {
        type Break = fn(&mut Facts);
        let cases: [(Break, &str); 6] = [
            (|f| f.misc.clear(), "BLK_DEV_DM"),
            (|f| f.crypto.clear(), "CRYPTO_SHA256"),
            (|f| f.dm_targets = Some(Vec::new()), "DM_VERITY"),
            (|f| f.filesystems.retain(|n| n != "erofs"), "EROFS_FS"),
            (|f| f.drivers.clear(), "BLK_DEV_LOOP"),
            (|f| f.filesystems.retain(|n| n != "ext4"), "EXT4_FS"),
        ];
        for (break_it, option) in cases {
            let mut facts = whole();
            break_it(&mut facts);
            assert_eq!(absent(Route::Stock, &facts), [option]);
            let piece = missing(Route::Stock, &facts)[0];
            assert!(
                piece
                    .message()
                    .starts_with(&format!("the kernel lacks CONFIG_{option}: "))
            );
        }
    }

    #[test]
    fn no_device_mapper_control_node_means_no_verity_target_either() {
        let mut facts = whole();
        facts.misc.clear();
        facts.dm_targets = None;
        assert_eq!(absent(Route::Stock, &facts), ["BLK_DEV_DM", "DM_VERITY"]);
    }

    #[test]
    fn the_boot_partition_filesystem_is_optional() {
        let mut facts = whole();
        facts.filesystems.retain(|n| n != "vfat");
        assert_eq!(absent(Route::Stock, &facts), ["VFAT_FS"]);
        assert!(!missing(Route::Stock, &facts)[0].required);
    }

    #[test]
    fn raw_nand_needs_ubi_and_ubifs_and_no_loop_or_ext4() {
        let mut facts = whole();
        facts.filesystems = vec!["ubifs".into(), "erofs".into()];
        facts.drivers.clear();
        facts.ubi = true;
        assert!(absent(Route::Nand, &facts).is_empty());
        facts.ubi = false;
        facts.filesystems.retain(|n| n != "ubifs");
        assert_eq!(absent(Route::Nand, &facts), ["MTD_UBI", "UBIFS_FS"]);
    }

    #[test]
    fn the_proc_files_are_read_by_name() {
        assert_eq!(
            filesystems("nodev\tsysfs\n\text4\n\tvfat\nnodev\terofs\n"),
            ["sysfs", "ext4", "vfat", "erofs"]
        );
        assert_eq!(misc("236 device-mapper\n"), ["device-mapper"]);
        assert_eq!(
            drivers("Character devices:\n  1 mem\nBlock devices:\n  7 loop\n"),
            ["mem", "loop"]
        );
        assert_eq!(
            crypto("name         : sha256\ndriver       : sha256-generic\nname : crc32c\n"),
            ["sha256", "crc32c"]
        );
    }

    /// The options this module names are exactly the file's `boot_check.checked` list
    /// (`contracts/slot-kernel.toml`, the machine-read source the gate also reads): every piece
    /// of either route, plus the two storage names `storage_message` gives.
    #[test]
    fn the_options_named_here_are_the_files_boot_check_list() {
        use std::collections::BTreeSet;
        let file: toml::Table = include_str!("../../../contracts/slot-kernel.toml")
            .parse()
            .unwrap();
        let listed: BTreeSet<String> = file["boot_check"]["checked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        let mut named: BTreeSet<String> = BTreeSet::new();
        for route in [Route::Stock, Route::Nand] {
            named.extend(pieces(route).iter().map(|piece| piece.option.to_string()));
        }
        let storage = storage_message();
        for option in ["MMC_BLOCK", "BLK_DEV_NVME"] {
            assert!(storage.contains(&format!("CONFIG_{option}")), "{storage}");
            named.insert(option.to_string());
        }
        assert_eq!(named, listed);
    }

    /// slot-init mounts the slot's root as an EROFS image (usually compressed), so the contract carries
    /// both symbols built in; removing either, or the mount, breaks this.
    #[test]
    fn slot_inits_use_of_erofs_is_tied_to_the_contracts_two_symbols() {
        let file: toml::Table = include_str!("../../../contracts/slot-kernel.toml")
            .parse()
            .unwrap();
        let always: Vec<&str> = file["always"]["all"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let boot = std::fs::read_to_string(repo().join("service/slot-init/src/boot.rs")).unwrap();
        let mounts_erofs = boot.contains("mount(\"erofs\"");
        assert!(mounts_erofs, "slot-init no longer mounts the root as erofs");
        for symbol in ["EROFS_FS", "EROFS_FS_ZIP"] {
            assert!(always.contains(&symbol), "{symbol} is not in always.all");
        }
    }

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The mailbox symbols the contract names for the Pi firmware family.
    const MAILBOX_GROUP: [&str; 5] = [
        "BRCM_CHAR_DRIVERS",
        "MAILBOX",
        "BCM2835_MBOX",
        "BCM_VCIO",
        "RASPBERRYPI_FIRMWARE",
    ];

    /// slot-init's Pi form can ask the firmware for its flashing mode through the mailbox, so the
    /// contract must carry the built-in group for it, and the group is there only because slot-init
    /// uses the mailbox: take either away and this fails. (The version is the contract's: the group
    /// is a new meaning, version 2.)
    #[test]
    fn slot_inits_use_of_the_mailbox_is_tied_to_the_contracts_firmware_group() {
        let file: toml::Table = include_str!("../../../contracts/slot-kernel.toml")
            .parse()
            .unwrap();
        assert_eq!(file["version"].as_integer(), Some(3));
        let cargo = std::fs::read_to_string(repo().join("service/slot-init/Cargo.toml")).unwrap();
        let sys = std::fs::read_to_string(repo().join("service/slot-init/src/sys.rs")).unwrap();
        let uses = cargo.contains("lemnos-driver-rpi-firmware-sys")
            && sys.contains("lemnos_driver_rpi_firmware_sys::Mailbox");
        let group: Vec<String> = file["firmware"]["rpi-tryboot"]["all"]
            .as_array()
            .expect("the contract has the rpi-tryboot group")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            uses,
            "slot-init no longer uses the mailbox: drop the group from the contract too"
        );
        assert_eq!(
            group, MAILBOX_GROUP,
            "the group names exactly what the mailbox needs"
        );
        // /dev/vcio exists only through devtmpfs: it is required of every slot board.
        let always = file["always"]["all"].as_array().unwrap();
        assert!(always.iter().any(|v| v.as_str() == Some("DEVTMPFS")));
    }

    /// No static device node: slot-init never makes one, so `/dev/vcio` is whatever devtmpfs gives.
    #[test]
    fn slot_init_makes_no_device_node_of_its_own() {
        for file in ["boot.rs", "sys.rs", "partition.rs", "kernel.rs", "ops.rs"] {
            let text =
                std::fs::read_to_string(repo().join("service/slot-init/src").join(file)).unwrap();
            let code: String = text.split("#[cfg(test)]").next().unwrap().to_string();
            for banned in ["mknod", "makedev", "CharDevice"] {
                assert!(!code.contains(banned), "{file} has {banned}");
            }
        }
    }

    /// The checker the contract's firmware group is read by refuses a configuration in which a
    /// symbol of the group is `=m` or unset, naming the symbol, and accepts one with all built in.
    #[test]
    fn the_checker_refuses_a_mailbox_symbol_that_is_a_module_or_unset() {
        let dir = std::env::temp_dir().join(format!("slot-init-kconfig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = repo().join("scripts/check-kernel-config.py");
        let run = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            std::process::Command::new("python3")
                .arg("-I")
                .arg(&script)
                .arg("--check-config")
                .arg(&path)
                .args(["--firmware", "rpi-tryboot"])
                .output()
                .unwrap()
        };
        let mut good = String::from("CONFIG_DEVTMPFS=y\n");
        for name in MAILBOX_GROUP {
            good.push_str(&format!("CONFIG_{name}=y\n"));
        }
        let out = run("good.config", &good);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for symbol in MAILBOX_GROUP.iter().copied().chain(["DEVTMPFS"]) {
            let as_module =
                good.replace(&format!("CONFIG_{symbol}=y"), &format!("CONFIG_{symbol}=m"));
            let out = run("module.config", &as_module);
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            assert!(!out.status.success(), "{symbol} =m was accepted");
            assert!(
                err.contains(&format!("CONFIG_{symbol} must be built in")) && err.contains("=m"),
                "{err}"
            );
            let unset = good.replace(&format!("CONFIG_{symbol}=y\n"), "");
            let out = run("unset.config", &unset);
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            assert!(!out.status.success(), "{symbol} unset was accepted");
            assert!(
                err.contains(&format!("CONFIG_{symbol} must be built in")) && err.contains("unset"),
                "{err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
