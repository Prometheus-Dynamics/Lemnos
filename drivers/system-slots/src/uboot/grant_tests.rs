//! The `uboot` record's environment through the daemon's root phase. The environment partition is handed
//! over as its own node (numbered from the disk's GPT, named through sysfs), and the daemon's writes go
//! through that file. The disk node is never the daemon's to write, and a FAT environment on the boot
//! partition makes the hand-over read only, refused by name with its reason.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use lemnos_core::{Config, ConfigValue};
use lemnos_slot_store::gpt;
use lemnos_slot_store::uboot_env::{self, Geometry};

use super::{Place, UbootEnv};
use crate::boot::BootRecord;
use crate::boot::Restarter;
use crate::tests::{TempDir, TestRoots, boot_tree};

/// The board file's `boot_sources` entry: the boot disk's controller is the device-tree node the fixture makes.
const SOURCE: &str = "/soc/mmc@7e202000=/soc/mmc@7e202000";
/// The environment's size (`uboot_env_size`) and the partition it lives in: block 100 to 227 of the disk, 128 blocks.
const SIZE: usize = 1024;
const FIRST: u64 = 100;
const LAST: u64 = 227;

/// The environment partition as U-Boot saves it: one valid copy at the top of the partition, with
/// `default` set, and the other copy erased.
fn env_partition(default: &str) -> Vec<u8> {
    let g = Geometry::in_partition(SIZE, 0, LAST - FIRST + 1).unwrap();
    let list = vec![("default".to_string(), default.to_string())];
    let data = uboot_env::encode_vars(&list, g.data_len()).unwrap();
    let mut part = vec![0xFF; (LAST - FIRST + 1) as usize * 512];
    let at = g.offset as usize;
    part[at..at + SIZE].copy_from_slice(&uboot_env::encode_copy(&data, 1));
    part
}

/// A board with a boot disk (`mmcblk0`: the boot partition 1 and the environment partition 2, found
/// through a fake sysfs and a fake `dev` directory as the root phase reads them).
struct Board {
    dir: TempDir,
    disk: PathBuf,
    boot: PathBuf,
    env: PathBuf,
    /// The disk's bytes as the board was made, to show the disk node is never written.
    disk_bytes: Vec<u8>,
}

impl Board {
    fn new(label: &str, boot_image: &[u8]) -> Self {
        let dir = TempDir::new(label);
        // Partition 1 and its sysfs entry, and the controller, the way the root phase's own fixture makes them.
        let boot = boot_tree(&dir.0, boot_image);
        let sys = dir.0.join("sys");
        let part2 = sys.join("devices/sd-host/mmcblk0/mmcblk0p2");
        fs::create_dir_all(&part2).unwrap();
        fs::write(part2.join("partition"), "2\n").unwrap();
        symlink(&part2, sys.join("class/block/mmcblk0p2")).unwrap();
        let dev = dir.0.join("dev");
        let part = env_partition("os-1");
        let mut disk = gpt::image(
            &[([1; 16], 34, 99), (gpt::ENV_TYPE_BYTES, FIRST, LAST)],
            256,
        );
        disk[FIRST as usize * 512..(LAST as usize + 1) * 512].copy_from_slice(&part);
        let node = dev.join("mmcblk0");
        fs::write(&node, &disk).unwrap();
        let env = dev.join("mmcblk0p2");
        fs::write(&env, &part).unwrap();
        Self {
            disk: node,
            boot,
            env,
            disk_bytes: disk,
            dir,
        }
    }

    /// The root phase's configuration for this board: the `uboot` record over the boot disk.
    fn root_line(&self) -> String {
        let entries = [
            ("record", ConfigValue::Str("uboot")),
            ("uboot_env_size", ConfigValue::Int(SIZE as i64)),
        ];
        let cfg = Config::new(&entries).with_lists(&[("boot_sources", &[SOURCE])]);
        crate::root_phase(&cfg)
    }

    /// The daemon's `uboot` record, as the daemon builds it: the boot disk, the environment's place and the
    /// boot partition.
    fn record(&self) -> UbootEnv {
        UbootEnv {
            disk: Ok(self.disk.clone()),
            place: Place::gpt(SIZE).unwrap(),
            restarter: Restarter {
                systemctl: "/nonexistent/systemctl".into(),
                check: None,
            },
            boot_node: Ok(self.boot.clone()),
            partitions: None,
        }
    }

    fn disk_unchanged(&self) -> bool {
        fs::read(&self.disk).unwrap() == self.disk_bytes
    }
}

#[test]
fn the_daemon_writes_the_environment_through_the_partition_the_root_phase_handed_over() {
    let board = Board::new(
        "uboot-grant-handover",
        &crate::bay_fixture::uboot_image("os-1"),
    );
    let _roots = TestRoots::set(&board.dir.0);
    let line = board.root_line();
    // The environment partition is handed over by its own node, read-write: no FAT environment is on the boot partition.
    assert!(
        line.contains(&format!("{}: handed over read-write", board.env.display())),
        "{line}"
    );
    let mut record = board.record();
    record
        .set_default("os-1")
        .expect("the daemon's write of the environment goes through the handed-over partition");
    // The partition holds the write, read back; the disk node is the same bytes as the board was made.
    assert_eq!(
        record.var("default").unwrap().as_deref(),
        Some("os-1"),
        "read back"
    );
    assert!(board.disk_unchanged(), "the disk node was written");
}

#[test]
fn a_fat_environment_on_the_boot_partition_makes_the_hand_over_read_only_and_the_write_is_refused_by_name()
 {
    let board = Board::new(
        "uboot-grant-fat",
        &crate::bay_fixture::uboot_image_with_env("os-1"),
    );
    let _roots = TestRoots::set(&board.dir.0);
    let line = board.root_line();
    assert!(
        line.contains(&format!("{}: handed over read only", board.env.display())),
        "{line}"
    );
    // The GPT write names the FAT environment before it reaches the handed file: refused by name, nothing written.
    let err = board
        .record()
        .set_default("os-1")
        .expect_err("a write under a read-only hand-over is refused")
        .to_string();
    assert!(
        err.contains("refusing to write the U-Boot environment on the GPT partition"),
        "{err}"
    );
    assert!(err.contains("uboot.env"), "the reason is named: {err}");
    assert!(err.contains("nothing was written"), "{err}");
    assert!(board.disk_unchanged(), "the disk node was written");
    assert_eq!(
        fs::read(&board.env).unwrap(),
        env_partition("os-1"),
        "the environment partition was written"
    );
}
