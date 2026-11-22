//! Everything the boot does to the system, behind one trait: [`crate::sys::System`] makes the
//! system calls on a board, and the tests' fake writes down what it was asked. Files on the
//! mounted partitions are read and written directly: in the tests those are temporary
//! directories.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_slot_store::manifest::Verity;
use lemnos_slot_store::ubi::Ubi;

/// What `/init` does to the system. Each failure is a sentence for the log.
pub trait Ops {
    /// One line for the kernel log, where a console and the saved log show it.
    fn log(&self, line: &str);
    /// A line that says why the boot stops (the rescue's reason): the kernel log at error level, so
    /// a console kept at a quiet level and a ramoops console (`docs/slots.md`, "A refusal without a
    /// console") still carry it. The same line as [`Ops::log`] otherwise.
    fn alert(&self, line: &str) {
        self.log(line);
    }
    /// `mount -t <fstype> -o <opts> <source> <target>`. `ro` and `noatime` in `opts` are
    /// mount flags; the rest goes to the filesystem.
    fn mount(&self, fstype: &str, opts: &str, source: &Path, target: &Path) -> Result<(), String>;
    /// Moves the mount at `source` to `target`.
    fn move_mount(&self, source: &Path, target: &Path) -> Result<(), String>;
    /// Binds the file `source` over the file `target`, read-only: a bind mount, then a read-only
    /// remount of it. On failure nothing stays mounted over `target`.
    fn bind_read_only(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn umount(&self, target: &Path);
    /// The device-mapper targets the running kernel has, by name (`verity`). Built in or loaded:
    /// an error when there is no device-mapper control node.
    fn dm_targets(&self) -> Result<Vec<String>, String>;
    /// A dm-verity device named `name` over `image`, as `verity` describes: through a read-only
    /// loop device when `image` is a file, directly when it is a block device. Returns the verity
    /// device's node.
    fn open_verity(&self, image: &Path, name: &str, verity: &Verity) -> Result<String, String>;
    /// The read-only block device over UBI volume `name` (`ubiblockN_M`): its node.
    fn ubi_block(&self, ubi: &Ubi, name: &str) -> Result<PathBuf, String>;
    /// Replaces UBI volume `name` with `bytes`, atomically (written beside, renamed over).
    fn ubi_replace(&self, ubi: &Ubi, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// The kernel's log, for keeping after a failure.
    fn kernel_log(&self) -> String;
    fn sync(&self);
    /// Gives the directory `dir` itself (never a link, never what it holds) to `uid`:`gid`.
    fn chown_dir(&self, dir: &Path, uid: u32, gid: u32) -> Result<(), String>;
    /// A restart, with the boot loader's argument when given (`0 tryboot`). Returns only when
    /// it couldn't.
    fn restart(&self, argument: Option<&str>);
    /// Into the new root, as PID 1. On a board it returns only when it couldn't.
    fn switch_root(&self, new_root: &Path) -> Result<(), String>;
    fn sleep(&self, time: Duration);
    /// Arms the hardware watchdog: the first act once devtmpfs is mounted, so that a hang from here on
    /// resets the board. The default does nothing (the tests' fake).
    fn arm_watchdog(&self) {}
    /// Stops feeding the watchdog: from here on nothing feeds it, so it resets the board unless the
    /// daemon takes it over. The default does nothing.
    fn stop_feeding(&self) {}
    /// Holds the boot after a grow whose call did not return: feeding has stopped (the caller did that),
    /// so the watchdog resets the board. The system's version never returns; the tests' fake returns.
    fn hold_for_reset(&self) {}
    /// Asks the firmware for its flashing mode at the next boot (the Pi family's RPIBOOT: the board
    /// then waits for a USB host with no timeout). The caller restarts when this returns `Ok`.
    fn request_flash_mode(&self) -> Result<(), String>;
    /// Clears the firmware's boot count (the Pi family's mailbox tag `0x0003808d`, value zero): done
    /// before a restart that is Lemnos's own, so it never counts toward the count fallback.
    fn clear_boot_count(&self) -> Result<(), String>;
    /// The boot partition's raw records and pointers, opened on its block node `dev` (nothing is
    /// mounted). The tests use an in-memory fake.
    fn open_boot(
        &self,
        dev: &Path,
    ) -> Result<Box<dyn lemnos_slot_store::boot_raw::BootRaw>, String> {
        lemnos_slot_store::boot_raw::open_records(dev)
    }
    /// The U-Boot environment's node, opened read-write (nothing is mounted), and its length in
    /// bytes. The tests use an in-memory device, with a cut at a chosen byte.
    fn open_uboot_env(
        &self,
        node: &Path,
    ) -> Result<(Box<dyn lemnos_slot_store::uboot_env::EnvDev>, u64), String> {
        lemnos_slot_store::uboot_env::FileEnv::open_sized(node)
            .map(|(dev, bytes)| {
                (
                    Box::new(dev) as Box<dyn lemnos_slot_store::uboot_env::EnvDev>,
                    bytes,
                )
            })
            .map_err(|error| format!("{}: {error}", node.display()))
    }
    /// The bay a slot's boot files are in on the boot partition at `dev`: the set's directory and each
    /// payload's stored length. Read raw, through [`lemnos_slot_store::bays::slot_bay_on`]; both the
    /// Pi firmware form and the U-Boot form carry the same bay records.
    fn slot_bay(&self, dev: &Path, slot: &str) -> Result<(String, Vec<(String, u64)>), String> {
        lemnos_slot_store::boot_raw::BayVolume::open(dev)?.slot_bay(slot)
    }
    /// A file on the boot partition, `path` relative to its root, read without a mount: the FAT
    /// volume at `dev` is read raw ([`lemnos_slot_store::bootcheck::fat_read`]). `None` when the file
    /// is not there. The tests read their tree of files instead.
    fn read_boot_file(&self, dev: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
        let mut file = std::fs::File::open(dev).map_err(|e| format!("{}: {e}", dev.display()))?;
        lemnos_slot_store::bootcheck::fat_read(&mut file, path)
    }
}
