//! The `uboot` boot record, for boards that boot through U-Boot's standard boot flow.
//!
//! Each slot keeps its own kernel, device trees and initramfs, so the loader chooses before any
//! kernel runs. At install the driver writes `slots/<id>/extlinux/extlinux.conf` from the slot's
//! files and its command line. The record is U-Boot's own redundant environment, on its own raw
//! partition ([`UbootEnv`]): the variables `default=<id>`, `trial=<id>` (unset when none) and
//! `trial_attempts=0..3`, the boots that may still start the trial. A static script,
//! `packaging/uboot/boot.cmd`, reads them, and for a trial with attempts takes one, saves the
//! environment and then starts the trial; when that returns, or there are none, it starts the
//! default; when the default can't be started, the fixed `extlinux/extlinux.conf` at the boot
//! partition's root, the install-time copy of the default's. [`steps`] is that decision, which the
//! script mirrors. The file-based record of the earlier design (`boot-record.env` on the boot
//! partition) is refused at run time: nothing on the boot partition is written for it.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemnos_slot_store::bays::{self, Protected};
use lemnos_slot_store::boot_node;
use lemnos_slot_store::boot_raw::{BayVolume, Trial};
use lemnos_slot_store::gpt;
use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};
use lemnos_slot_store::uboot_env::{self, FileEnv, Geometry, Place};

use crate::boot::{BootRecord, Restarter};
use crate::store::{Layout, replace, sync_dir, valid_id};

pub use lemnos_slot_store::records::MAX_ATTEMPTS;
pub use lemnos_slot_store::records::uboot::{KEYS, RECORD_FILE, Record};

/// The slot variables of U-Boot's environment, every one the writers ([`BootRecord`] here and
/// slot-init's fallback) set and `packaging/uboot/boot.cmd` reads, and no others: the default's
/// slot and bay, the previous slot's bay (the environment holds no id for it), and the trial's slot,
/// bay and attempts left. A test holds the script's reads, the writers' writes and this list equal.
pub const VARIABLES: [&str; 6] = [
    "default",
    "default_bay",
    "previous_bay",
    "trial",
    "trial_bay",
    "trial_attempts",
];
/// The restart count (BB2): `lemnos_restarts`, the boots attempted since the last good boot. The script
/// reads it and raises it (one `saveenv` for each write), and lemnosd clears it, so it is listed apart from
/// the record's variables, which the record writers own.
pub const COUNT_VARIABLES: [&str; 1] = ["lemnos_restarts"];

/// Where a slot's loader configuration is, under its boot directory.
pub const EXTLINUX: &str = "extlinux/extlinux.conf";

/// One step of the loader script, in the order it takes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Takes one attempt from the trial: writes the record with `attempts` left, and only then
    /// `sysboot`s the trial `id`. When the write fails, the trial is skipped.
    Trial { id: String, attempts: u8 },
    /// `sysboot` this slot's configuration. Returns only when that slot couldn't be started.
    Boot(String),
    /// `sysboot` the fixed configuration at the boot partition's root.
    Root,
}

/// What the loader script does with `record` (`None`: it couldn't be read): each step is tried
/// when the one before it returned. A trial with attempts left (1 to 3) takes one: its boot has
/// `attempts - 1` left in the record. With none (0), the trial is skipped.
pub fn steps(record: Option<&Record>) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(record) = record {
        if !record.trial.is_empty() && (1..=MAX_ATTEMPTS).contains(&record.attempts) {
            steps.push(Step::Trial {
                id: record.trial.clone(),
                attempts: record.attempts - 1,
            });
        }
        if !record.default.is_empty() {
            steps.push(Step::Boot(record.default.clone()));
        }
    }
    steps.push(Step::Root);
    steps
}

/// The slot's device tree, by the fixed contract name the boot script loads it by.
pub const BOARD_DTB: &str = "board.dtb";

/// Whether a file name may go into a loader configuration as it is: the configuration's words
/// are split at white space, and `#` starts a comment.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'#' && b != b'"' && b != b'\\')
}

/// The loader configuration for slot `id`, whose boot directory holds `files` (paths relative to
/// it), started with `append` as its command line. The kernel is the first of `Image`,
/// `Image.gz`, `zImage`, `vmlinuz`, `kernel*.img`; the initramfs `initramfs*` or `initrd*`; the
/// device tree is `board.dtb` at the top, the contract name the boot script loads it by. The label
/// has no `fdt` line: the script loads the dtb, takes the boot device's path from U-Boot's own
/// tree, and names the boot source in `/chosen` before the label starts.
pub fn extlinux(id: &str, files: &[String], append: &str) -> Result<String, String> {
    if !valid_id(id) {
        return Err(format!("`{id}` isn't a slot id"));
    }
    let top: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| !f.contains('/'))
        .collect();
    let find = |want: &dyn Fn(&str) -> bool| top.iter().copied().find(|f| want(f));
    let kernel = ["Image", "Image.gz", "zImage", "vmlinuz"]
        .iter()
        .find_map(|k| find(&|f| f == *k))
        .or_else(|| find(&|f| f.starts_with("vmlinuz")))
        .or_else(|| find(&|f| f.starts_with("kernel") && f.ends_with(".img")))
        .ok_or("the slot has no kernel (Image, zImage, vmlinuz or kernel*.img)")?;
    let initrd = find(&|f| f.starts_with("initramfs") || f.starts_with("initrd"))
        .ok_or("the slot has no initramfs (initramfs* or initrd*)")?;
    if !top.contains(&BOARD_DTB) {
        return Err(format!(
            "the slot has no {BOARD_DTB} (the device tree the boot script loads)"
        ));
    }
    let dir = format!("/slots/{id}");
    for name in [kernel, initrd] {
        if !plain(name) {
            return Err(format!(
                "the boot file name `{name}` can't go into extlinux.conf"
            ));
        }
    }
    if append.contains(['\n', '\r']) {
        return Err("the command line has a line break".into());
    }
    Ok(format!(
        "# Written by the board's slot driver for slot {id}. Rewritten at each install.\n\
         default {id}\n\
         timeout 0\n\
         label {id}\n\
         \x20 kernel {dir}/{kernel}\n\
         \x20 initrd {dir}/{initrd}\n\
         \x20 append {append}\n"
    ))
}

/// Every file under `dir`, as paths relative to it.
fn files_under(dir: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(at)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(root, &path, out)?;
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// The `uboot` record over the boot partition at `boot`. On raw NAND (`record = "ubi"`) the
/// record is the static UBI volume `boot-record` instead of the file, replaced whole by a rename
/// ([`lemnos_slot_store::ubi::replace`]); `packaging/uboot/boot-ubi.cmd` reads it.
#[derive(Clone)]
pub struct Uboot {
    pub boot: PathBuf,
    pub restarter: Restarter,
    /// The UBI device and what changes it, when the record is a volume.
    pub volume: Option<(Ubi, Arc<dyn UbiWrite>)>,
}

impl Uboot {
    fn path(&self) -> PathBuf {
        self.boot.join(RECORD_FILE)
    }

    /// The record as it stands; an unreadable one is empty.
    pub fn read(&self) -> Record {
        let text = match &self.volume {
            Some((device, _)) => device
                .read(ubi::RECORD_VOLUME)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok()),
            None => fs::read_to_string(self.path()).ok(),
        };
        text.map(|text| Record::parse(&text)).unwrap_or_default()
    }

    fn write(&self, record: &Record) -> io::Result<()> {
        match &self.volume {
            Some((device, writer)) => ubi::replace(
                device,
                &**writer,
                ubi::RECORD_VOLUME,
                record.text().as_bytes(),
            ),
            // A record file on the boot partition is changed by no run-time code: the U-Boot form
            // keeps its record in the U-Boot environment (see [`UbootEnv`]).
            None => Err(io::Error::other(format!(
                "{} is not written at run time: the U-Boot record is in the U-Boot environment",
                self.path().display()
            ))),
        }
    }
}

impl BootRecord for Uboot {
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        let mut record = self.read();
        record.default = id.to_string();
        self.write(&record)
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()> {
        let mut record = self.read();
        record.trial = id.to_string();
        record.attempts = attempts;
        self.write(&record)
    }

    fn pending(&self) -> io::Result<Option<(String, u8)>> {
        let record = self.read();
        Ok((!record.trial.is_empty()).then_some((record.trial, record.attempts)))
    }

    fn set_attempts(&mut self, attempts: u8) -> io::Result<()> {
        let mut record = self.read();
        if record.trial.is_empty() {
            return Ok(());
        }
        record.attempts = attempts;
        self.write(&record)
    }

    fn clear_trial(&mut self) -> io::Result<()> {
        let mut record = self.read();
        record.trial.clear();
        record.attempts = 0;
        self.write(&record)
    }

    fn install(&self, slot: &str, _payloads: &[&[u8]]) -> io::Result<()> {
        Err(io::Error::other(format!(
            "{slot} isn't installed: payload bays on raw NAND (UBI) are parked, so this boot form installs no slot (docs/slots.md, \"The boot partition: payload bays\")"
        )))
    }

    fn free(&self, _slot: &str) -> io::Result<()> {
        Err(io::Error::other(
            "payload bays on raw NAND (UBI) are parked, so this boot form frees no bay (docs/slots.md, \"The boot partition: payload bays\")",
        ))
    }

    fn restart(&mut self) -> io::Result<()> {
        // The script starts a trial that has attempts left on any restart.
        self.restarter.run(None)
    }

    fn prepare(&self, dir: &Path, id: &str, cmdline: &str) -> io::Result<()> {
        let files = files_under(dir)?;
        let text = extlinux(id, &files, cmdline).map_err(io::Error::other)?;
        fs::create_dir_all(dir.join("extlinux"))?;
        fs::write(dir.join(EXTLINUX), text)
    }

    fn fallback_copy(&self, layout: &Layout, id: &str) -> io::Result<()> {
        // Only the UBI form keeps a root copy current; on a boot partition the root copy is the
        // install-time fallback, and nothing writes it at run time.
        if self.volume.is_none() {
            return Ok(());
        }
        let from = layout.boot_dir(id).join(EXTLINUX);
        let text = fs::read(&from)?;
        let dir = self.boot.join("extlinux");
        fs::create_dir_all(&dir)?;
        // The label has no fdt line: the script loads the device tree beside it, the same slot's.
        let dtb = fs::read(layout.boot_dir(id).join(BOARD_DTB))?;
        replace(&dir.join(BOARD_DTB), &dtb)?;
        replace(&dir.join("extlinux.conf"), &text)?;
        sync_dir(&self.boot)
    }
}

/// The environment variable the image's U-Boot carries when it has the slot environment change: the
/// default environment of the patched U-Boot sets `lemnos_env_rev` to [`ENV_MARKER_VALUE`]. It reaches
/// the stored environment at U-Boot's first save of a valid environment (the defaults are imported when
/// the stored one is invalid, and `saveenv` writes them with the rest). Checked by [`UbootEnv::trial_updates`]
/// and by an image builder before it builds an RK slot image (`docs/boards.md`, "The U-Boot marker").
pub const ENV_MARKER: &str = "lemnos_env_rev";
/// The value of [`ENV_MARKER`] the driver expects.
pub const ENV_MARKER_VALUE: &str = "1";
/// The variable the image's boot preamble sets to the MMC device the environment is kept on
/// (`service/ctl` `boot_dir`, `setenv lemnos_envdev {env_device}`). Not written by the driver; it is in
/// the budget because it is in the same environment.
pub const ENVDEV_VARIABLE: &str = "lemnos_envdev";
/// The largest `uboot_env_mmc_index` the board file may give: [`ENVDEV_VARIABLE`] is at most two digits.
pub const MAX_MMC_INDEX: i64 = 99;
/// The restart count's saturation (`packaging/uboot/boot.cmd`: the count stops at 8).
pub const RESTARTS_MAX: u8 = 8;
/// The variables the boot preamble (`service/ctl` `boot_dir`) sets, which the script's `saveenv` stores too.
pub const USB_VARIABLE: &str = "lemnos_usb";
pub const LED_VARIABLE: &str = "lemnos_led";
pub const RESET_VARIABLE: &str = "lemnos_rst";
/// The episodes' bay and count (BB2: `set_episodes`, a driver write on the arrival and good-boot paths).
pub const EPISODES_BAY_VARIABLE: &str = "lemnos_episodes_bay";
pub const EPISODES_VARIABLE: &str = "lemnos_episodes";
/// Most fastboot USB controller index the board file gives (two digits, as `MAX_MMC_INDEX`).
pub const MAX_USB_INDEX: i64 = 99;
/// The longest `uboot_led` label the board file gives (bytes).
pub const MAX_LED_BYTES: usize = 32;
/// The longest `uboot_reset_word` the board file gives: `0x` and eight hex digits.
pub const MAX_RESET_BYTES: usize = 10;
/// The most bytes the variables the driver, the image's preamble and the restart count store take in one
/// copy, header included: the worst case of [`worst_case_entries`] in `name=value\0` form, with the list's
/// final NUL and the 5-byte header. Derived in docs/slots.md, "What the driver writes into the environment".
pub const ENV_WORST_CASE_BYTES: usize = 394;
/// The budget those variables must stay within. A test fails when [`worst_case_copy_bytes`] exceeds it.
pub const ENV_BUDGET_BYTES: usize = 400;

/// The longest slot id the driver writes into the environment (`valid_id`: at most 64 bytes).
const WORST_ID_BYTES: usize = 64;

/// The worst-case value of one of the variables the driver or the image writes, `None` for a name the
/// budget does not know (a new variable must be given a value here, or the budget test fails).
fn worst_value(name: &str) -> Option<String> {
    Some(match name {
        "default" | "trial" => "a".repeat(WORST_ID_BYTES),
        "default_bay" | "previous_bay" | "trial_bay" => {
            lemnos_slot_store::bays::set_name(lemnos_slot_store::bays::SETS - 1)
        }
        "trial_attempts" => MAX_ATTEMPTS.to_string(),
        n if n == ENV_MARKER => ENV_MARKER_VALUE.to_string(),
        n if n == ENVDEV_VARIABLE => MAX_MMC_INDEX.to_string(),
        "lemnos_restarts" => RESTARTS_MAX.to_string(),
        n if n == EPISODES_BAY_VARIABLE => {
            lemnos_slot_store::bays::set_name(lemnos_slot_store::bays::SETS - 1)
        }
        // The episodes are at most the demotion threshold, which is at most 255 (`demote_after_episodes`).
        n if n == EPISODES_VARIABLE => u8::MAX.to_string(),
        n if n == USB_VARIABLE => MAX_USB_INDEX.to_string(),
        n if n == LED_VARIABLE => "a".repeat(MAX_LED_BYTES),
        n if n == RESET_VARIABLE => format!("0x{}", "f".repeat(MAX_RESET_BYTES - 2)),
        _ => return None,
    })
}

/// The variables the driver writes (`VARIABLES`), the marker and the image's `lemnos_envdev`, each with
/// its worst-case value. A name [`worst_value`] does not know is given the longest slot id, so it is
/// over-counted rather than missed.
pub fn worst_case_entries() -> Vec<(String, String)> {
    VARIABLES
        .iter()
        .copied()
        .chain(COUNT_VARIABLES)
        .chain([
            ENV_MARKER,
            ENVDEV_VARIABLE,
            USB_VARIABLE,
            LED_VARIABLE,
            RESET_VARIABLE,
            EPISODES_BAY_VARIABLE,
            EPISODES_VARIABLE,
        ])
        .map(|name| {
            let value = worst_value(name).unwrap_or_else(|| "a".repeat(WORST_ID_BYTES));
            (name.to_string(), value)
        })
        .collect()
}

/// The bytes of one copy the worst-case entries take: the header, each `name=value\0`, and the list's
/// final NUL. The same bytes `encode_vars` produces for them.
pub fn worst_case_copy_bytes() -> usize {
    let entries: usize = worst_case_entries()
        .iter()
        .map(|(name, value)| name.len() + 1 + value.len() + 1)
        .sum();
    lemnos_slot_store::uboot_env::HEADER + entries + 1
}

/// Why a board file's `uboot_env_size` is refused, if it is: a copy must hold the worst case of the
/// driver's variables (U-Boot's own default environment is in the same copy and is not known here).
pub fn size_refusal(size: usize) -> Option<&'static str> {
    (size < ENV_WORST_CASE_BYTES).then_some(
        "is too small: the driver's own variables need 394 bytes in one copy, header included, in the worst case (docs/slots.md, \"What the driver writes into the environment\"); U-Boot's default environment needs more",
    )
}

/// The name of the environment's partition on its device: what `boot.cmd` looks for with
/// `part start mmc <dev> uboot-env` before it saves the environment.
pub const ENV_PARTITION: &str = "uboot-env";

/// Why trial updates are unavailable on a `uboot` board. A fixed set: the code is what `info` carries,
/// and the sentence is what a refused trial and `health` say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No partition named `uboot-env` on the environment's device: `boot.cmd` never saves the
    /// environment and never starts a trial.
    NoEnvPartition,
    /// The environment has no valid copy, or its device or disk can't be opened.
    EnvUnreadable,
    /// The GPT environment is written only where U-Boot reads it; the boot partition holds the FAT
    /// `uboot.env`, which U-Boot reads first, so a GPT write would not be read.
    FatEnvFile,
    /// The boot partition is unknown or can't be read as FAT, so `uboot.env` can't be ruled out.
    BootPartitionUnreadable,
    /// The image's U-Boot does not carry the slot environment change ([`ENV_MARKER`] is not the expected value).
    LoaderNotUpdated,
}

impl Reason {
    /// The fixed code `info` carries.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoEnvPartition => "no-env-partition",
            Self::EnvUnreadable => "env-unreadable",
            Self::FatEnvFile => "fat-env-file",
            Self::BootPartitionUnreadable => "boot-partition-unreadable",
            Self::LoaderNotUpdated => "loader-not-updated",
        }
    }

    /// The sentence a refused trial and `health` say: the reason, and that nothing was written.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::NoEnvPartition => {
                "trial updates are unavailable: the U-Boot environment's device has no uboot-env partition, so U-Boot's boot script never saves the environment and never starts a trial; nothing was written"
            }
            Self::EnvUnreadable => {
                "trial updates are unavailable: the U-Boot environment can't be read (no valid copy, or its device can't be opened); nothing was written"
            }
            Self::FatEnvFile => {
                "trial updates are unavailable: the boot partition holds uboot.env, which U-Boot reads before the GPT environment, so a write of the GPT environment would not be read; nothing was written"
            }
            Self::BootPartitionUnreadable => {
                "trial updates are unavailable: the boot partition can't be read as FAT, so uboot.env can't be ruled out, and the GPT environment is not written; nothing was written"
            }
            Self::LoaderNotUpdated => {
                "trial updates are unavailable: the image's U-Boot does not carry the slot environment change (lemnos_env_rev is not 1 in the environment; docs/boards.md, \"What an image builder checks\"); nothing was written"
            }
        }
    }
}

/// A reason trial updates are unavailable, with what the driver found for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    pub reason: Reason,
    /// What was found, for `info` (the error text of the read or the check).
    pub detail: String,
}

/// Whether a trial can be recorded and started on this board, as the driver found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrialUpdates {
    Available,
    Unavailable(Unavailable),
}

/// The `uboot` record's root phase, run by the daemon as root before it drops to its own user, with the
/// environment's `place`, the boot disk's node and the boot partition's node (or why either is unknown).
/// The boot partition is handed over read only: its bays and its FAT file are read at run time. The
/// environment is handed over as its own node, never the disk: the GPT form's partition (found on the
/// disk here, before the drop, and numbered through sysfs) read-write, unless the boot partition holds
/// the FAT environment U-Boot reads first, when it is read only and the daemon's writes are refused by
/// that reason. The device form's named device is handed over read-write (its writes are confined to the
/// two copies). Returns one line for the daemon's log.
pub fn root_phase(
    place: &Place,
    disk: &Result<PathBuf, String>,
    boot: &Result<PathBuf, String>,
) -> String {
    let boot_line = match boot {
        Ok(node) => match File::open(node) {
            Ok(file) => {
                boot_node::hand_over(node, file, false);
                format!("{}: handed over read only", node.display())
            }
            Err(e) => format!("{}: not handed over: {e}", node.display()),
        },
        Err(why) => format!("the boot partition isn't known: {why}"),
    };
    let env_line = match place {
        Place::Gpt { .. } => match disk {
            Ok(disk) => gpt_partition(disk, boot).unwrap_or_else(|why| {
                format!("the environment partition is not handed over: {why}")
            }),
            Err(why) => format!(
                "the boot disk isn't known, so the environment partition is not handed over: {why}"
            ),
        },
        Place::Device { device, .. } => device_node(device)
            .unwrap_or_else(|why| format!("the environment's device is not handed over: {why}")),
    };
    format!("{boot_line}; {env_line}")
}

/// The GPT form's environment partition, for [`root_phase`]: the disk's table is read by name (the root
/// phase is root), the partition's node is the kernel's `<disk>p<N>` (or `<disk><N>`) found through sysfs,
/// and that node is handed over, read-write when [`uboot_env::refuse_if_fat_env`] is clean on the boot partition.
fn gpt_partition(disk: &Path, boot: &Result<PathBuf, String>) -> Result<String, String> {
    let mut table = FileEnv::open_read(disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let (_, number) = gpt::find_env_partition_numbered(&mut table)?;
    let named = gpt::count_named(&mut table, ENV_PARTITION)?;
    drop(table);
    let node = boot_node::partition_node(disk, number)?;
    let refused = match boot {
        Ok(boot) => uboot_env::refuse_if_fat_env(boot).err(),
        Err(why) => Some(format!(
            "the boot partition isn't known, so {} can't be ruled out: {why}",
            uboot_env::FAT_ENV_FILE
        )),
    };
    let write = refused.is_none();
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .open(&node)
        .map_err(|e| format!("{}: {e}", node.display()))?;
    uboot_env::hand_over(&node, Some(disk), file, refused, named);
    Ok(format!(
        "{}: handed over {}",
        node.display(),
        if write { "read-write" } else { "read only" }
    ))
}

/// The device form's named device, for [`root_phase`]: handed over read-write.
fn device_node(device: &str) -> Result<String, String> {
    let node = PathBuf::from(device);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&node)
        .map_err(|e| format!("{device}: {e}"))?;
    uboot_env::hand_over(&node, None, file, None, 0);
    Ok(format!("{device}: handed over read-write"))
}

/// The `uboot` record: `default`, `trial` and `trial_attempts` are variables of U-Boot's own
/// redundant environment, at `place` (a raw, two-copy record, `lemnos_slot_store::uboot_env`).
/// `packaging/uboot/boot.cmd` reads them and saves the environment with `saveenv` before it starts a
/// trial. Nothing on the boot partition is written at run time.
pub struct UbootEnv {
    /// The boot disk's whole-disk node, or why it is not known.
    pub disk: Result<PathBuf, String>,
    pub place: Place,
    pub restarter: Restarter,
    /// The boot partition's block node: the payload bays and their records are written there, raw.
    /// The reason it is not known is kept: an install is then refused by it.
    pub boot_node: Result<PathBuf, String>,
    /// The device whose partition table names the `uboot-env` partition, when it is not the environment's
    /// own device (the boot disk for the GPT form, the device for the device form): `None` for both
    /// in a board file. Set by a test whose environment is a file with no partition table.
    pub partitions: Option<PathBuf>,
}

impl UbootEnv {
    /// The environment opened: its device and the geometry the place gives on it. The GPT form takes the
    /// environment partition the root phase handed over for the disk (its offsets are the partition's own);
    /// without one it opens the disk by name, which the daemon's user is refused. The device form names its device.
    fn open(&self) -> io::Result<(FileEnv, Geometry)> {
        if let (Place::Gpt { .. }, Ok(disk)) = (&self.place, &self.disk)
            && let Some(handed) = uboot_env::handover_of(disk)
        {
            let (dev, bytes) = FileEnv::open_sized(&handed.node)
                .map_err(|e| io::Error::other(format!("{}: {e}", handed.node.display())))?;
            let geometry = self
                .place
                .geometry_in_partition(bytes)
                .map_err(io::Error::other)?;
            return Ok((dev, geometry));
        }
        let node = match &self.place {
            Place::Gpt { .. } => {
                let disk = self.disk.as_ref().map_err(|why| {
                    io::Error::other(format!(
                        "the boot disk isn't known, so the U-Boot environment can't be found: {why}"
                    ))
                })?;
                self.place.node(disk)
            }
            Place::Device { .. } => self.place.node(Path::new("")),
        };
        let (mut dev, bytes) = FileEnv::open_sized(&node)
            .map_err(|e| io::Error::other(format!("{}: {e}", node.display())))?;
        let geometry = self
            .place
            .geometry(&mut dev, bytes)
            .map_err(io::Error::other)?;
        Ok((dev, geometry))
    }

    fn set(&self, updates: &[(&str, Option<&str>)]) -> io::Result<()> {
        let (mut dev, geometry) = self.open()?;
        if let Place::Gpt { .. } = &self.place {
            // The GPT environment is written only where U-Boot will read it: not while the boot
            // partition holds the FAT environment file U-Boot reads first.
            let boot = self.boot_node.as_ref().map_err(|why| {
                io::Error::other(format!(
                    "refusing to write the U-Boot environment on the GPT partition: the boot partition isn't known, so {} can't be ruled out ({why}); nothing was written",
                    uboot_env::FAT_ENV_FILE
                ))
            })?;
            uboot_env::refuse_if_fat_env(boot).map_err(io::Error::other)?;
        }
        uboot_env::set(&mut dev, &geometry, updates).map_err(io::Error::other)
    }

    /// Every variable of the environment as stored, `load_checked`'s list.
    fn list(&self) -> io::Result<Vec<(String, String)>> {
        let (mut dev, geometry) = self.open()?;
        let (_, list) = uboot_env::load_checked(&mut dev, &geometry).map_err(io::Error::other)?;
        Ok(list)
    }

    /// Whether trial updates are available on this board, found now: the environment's device has the
    /// `uboot-env` partition the script saves to; the environment reads; for the GPT form, the boot
    /// partition does not hold `uboot.env` (the write would be refused) and can be read to rule it out;
    /// and the image's U-Boot carries the slot environment change ([`ENV_MARKER`]). The first failing
    /// check is the reason. Reads only; nothing is written.
    pub fn trial_updates(&self) -> TrialUpdates {
        let fail = |reason: Reason, detail: String| {
            TrialUpdates::Unavailable(Unavailable { reason, detail })
        };
        // The GPT form's table was read by the root phase when it handed the partition over: the daemon
        // can't read the disk by name, so that count stands for it.
        let handed = match (&self.partitions, &self.place, &self.disk) {
            (None, Place::Gpt { .. }, Ok(disk)) => uboot_env::handover_of(disk),
            _ => None,
        };
        let (table, named) = match handed {
            Some(handed) => (self.disk.clone().unwrap_or_default(), Ok(handed.named)),
            None => {
                let table = match (&self.partitions, &self.place) {
                    (Some(path), _) => path.clone(),
                    (None, Place::Gpt { .. }) => match &self.disk {
                        Ok(disk) => disk.clone(),
                        Err(why) => {
                            return fail(
                                Reason::EnvUnreadable,
                                format!("the boot disk isn't known: {why}"),
                            );
                        }
                    },
                    (None, Place::Device { device, .. }) => PathBuf::from(device),
                };
                let mut dev = match FileEnv::open(&table) {
                    Ok(dev) => dev,
                    Err(e) => {
                        return fail(Reason::EnvUnreadable, format!("{}: {e}", table.display()));
                    }
                };
                let named = gpt::count_named(&mut dev, ENV_PARTITION);
                (table, named)
            }
        };
        match named {
            Ok(1..) => {}
            Ok(0) => {
                return fail(
                    Reason::NoEnvPartition,
                    format!("no partition named {ENV_PARTITION} on {}", table.display()),
                );
            }
            Err(why) => return fail(Reason::NoEnvPartition, why),
        }
        let list = match self.list() {
            Ok(list) => list,
            Err(e) => return fail(Reason::EnvUnreadable, e.to_string()),
        };
        if let Place::Gpt { .. } = &self.place {
            match &self.boot_node {
                Err(why) => return fail(Reason::BootPartitionUnreadable, why.clone()),
                Ok(boot) => match uboot_env::fat_env(boot) {
                    uboot_env::FatEnv::Absent => {}
                    uboot_env::FatEnv::Present(size) => {
                        return fail(
                            Reason::FatEnvFile,
                            format!(
                                "{} ({size} bytes) on the boot partition",
                                uboot_env::FAT_ENV_FILE
                            ),
                        );
                    }
                    uboot_env::FatEnv::Unreadable(why) => {
                        return fail(Reason::BootPartitionUnreadable, why);
                    }
                },
            }
        }
        let marker = list
            .iter()
            .find(|(key, _)| key == ENV_MARKER)
            .map(|(_, value)| value.as_str());
        if marker != Some(ENV_MARKER_VALUE) {
            return fail(
                Reason::LoaderNotUpdated,
                format!(
                    "{ENV_MARKER} is {} in the environment, expected {ENV_MARKER_VALUE}",
                    marker.map_or("unset".to_string(), |v| format!("{v:?}"))
                ),
            );
        }
        TrialUpdates::Available
    }

    /// The boot partition's bays, opened raw on its node (this form has no pointer or record files
    /// on the volume).
    fn raw(&self) -> io::Result<BayVolume> {
        let node = self.boot_node.as_ref().map_err(|why| {
            io::Error::other(format!(
                "the boot partition's device isn't known ({why}): its payload bays can't be written"
            ))
        })?;
        BayVolume::open(node).map_err(io::Error::other)
    }

    /// The boot partition's bays, read only: a slot's bay and the layout (what `set_default` reads).
    fn raw_read(&self) -> io::Result<BayVolume> {
        let node = self.boot_node.as_ref().map_err(|why| {
            io::Error::other(format!(
                "the boot partition's device isn't known ({why}): its payload bays can't be read"
            ))
        })?;
        BayVolume::open_read(node).map_err(io::Error::other)
    }

    /// The bays the environment protects: the default's (`default_bay`), the trial's (`trial_bay`),
    /// the previous's (`previous_bay`, written by [`BootRecord::set_default`]) and a trial with attempts left.
    fn protected(&self) -> io::Result<Protected> {
        let (mut dev, geometry) = self.open()?;
        let (_, list) = uboot_env::load_checked(&mut dev, &geometry).map_err(io::Error::other)?;
        let get = |name: &str| {
            list.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };
        let attempts = get("trial_attempts")
            .and_then(|a| a.parse::<u8>().ok())
            .unwrap_or(0)
            .min(MAX_ATTEMPTS);
        Ok(Protected {
            default: get("default_bay").and_then(|b| bays::set_of(&b)),
            previous: get("previous_bay").and_then(|b| bays::set_of(&b)),
            trial: get("trial_bay").and_then(|b| bays::set_of(&b)),
            pending: get("trial")
                .filter(|_| attempts > 0)
                .map(|slot| Trial { slot, attempts }),
        })
    }

    /// The bay (`bayN`) a slot's files are in: the environment's `default_bay` and `trial_bay`
    /// name it, so `boot.cmd` reads the bay's extlinux without a lookup of its own.
    fn bay_name(&self, id: &str) -> io::Result<String> {
        let (dir, _) = self.raw_read()?.slot_bay(id).map_err(io::Error::other)?;
        Ok(dir.rsplit('/').next().unwrap_or_default().to_string())
    }

    /// Every variable the environment holds, as the writer reads it.
    fn vars(&self) -> io::Result<Vec<(String, String)>> {
        let (mut dev, geometry) = self.open()?;
        let (_, list) = uboot_env::load_checked(&mut dev, &geometry).map_err(io::Error::other)?;
        Ok(list)
    }

    /// One variable's value, `None` when it is not set.
    fn var(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self
            .vars()?
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value))
    }

    /// The variables as they stand: `(default, trial, attempts)`, empty and 0 when unset.
    fn read(&self) -> io::Result<(String, String, u8)> {
        let (mut dev, geometry) = self.open()?;
        let (_, list) = uboot_env::load_checked(&mut dev, &geometry).map_err(io::Error::other)?;
        let get = |name: &str| {
            list.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        let attempts = get("trial_attempts")
            .parse::<u8>()
            .unwrap_or(0)
            .min(MAX_ATTEMPTS);
        Ok((get("default"), get("trial"), attempts))
    }
}

impl BootRecord for UbootEnv {
    fn restarts(&self) -> io::Result<Option<u8>> {
        // Unset is no restarts: the script sets the count at the first boot it runs.
        let n = match self.var("lemnos_restarts")? {
            None => 0,
            Some(text) => text.parse::<u8>().map_err(|_| {
                io::Error::other(format!("lemnos_restarts {text:?} is not a count"))
            })?,
        };
        Ok(Some(n))
    }

    fn set_restarts(&mut self, restarts: u8) -> io::Result<()> {
        self.set(&[("lemnos_restarts", Some(restarts.to_string().as_str()))])
    }

    fn episodes(&self) -> io::Result<Option<(String, u8)>> {
        let Some(slot) = self.var("lemnos_episodes_bay")? else {
            return Ok(None);
        };
        let n = self
            .var("lemnos_episodes")?
            .unwrap_or_default()
            .parse::<u8>()
            .unwrap_or(0);
        Ok(Some((slot, n)))
    }

    fn set_episodes(&mut self, slot: &str, episodes: u8) -> io::Result<()> {
        self.set(&[
            ("lemnos_episodes_bay", Some(slot)),
            ("lemnos_episodes", Some(episodes.to_string().as_str())),
        ])
    }

    fn clear_episodes(&mut self) -> io::Result<()> {
        self.set(&[("lemnos_episodes_bay", None), ("lemnos_episodes", None)])
    }

    fn trial_updates(&self) -> TrialUpdates {
        UbootEnv::trial_updates(self)
    }

    /// Points the default at the slot's bay. The bay the default had becomes `previous_bay` (as the Pi
    /// form's switch writes its previous pointer): the environment has no other record of it, and an
    /// install must not take it.
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        // The environment first: one U-Boot has not saved, or at another size, is refused by that.
        let old = self.protected()?.default.map(bays::set_name);
        let bay = self.bay_name(id)?;
        let previous = old.filter(|old| *old != bay);
        match previous {
            Some(old) => self.set(&[
                ("previous_bay", Some(&old)),
                ("default", Some(id)),
                ("default_bay", Some(&bay)),
            ]),
            None => self.set(&[("default", Some(id)), ("default_bay", Some(&bay))]),
        }
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()> {
        // The environment first, as for the default: one U-Boot has not saved is refused by that.
        self.read()?;
        let bay = self.bay_name(id)?;
        self.set(&[
            ("trial", Some(id)),
            ("trial_bay", Some(&bay)),
            ("trial_attempts", Some(&attempts.to_string())),
        ])
    }

    fn pending(&self) -> io::Result<Option<(String, u8)>> {
        let (_, trial, attempts) = self.read()?;
        Ok((!trial.is_empty()).then_some((trial, attempts)))
    }

    fn set_attempts(&mut self, attempts: u8) -> io::Result<()> {
        let (_, trial, _) = self.read()?;
        if trial.is_empty() {
            return Ok(());
        }
        self.set(&[("trial_attempts", Some(&attempts.to_string()))])
    }

    fn clear_trial(&mut self) -> io::Result<()> {
        self.set(&[
            ("trial", None),
            ("trial_bay", None),
            ("trial_attempts", None),
        ])
    }

    fn declared(&self) -> io::Result<(Vec<String>, Option<String>)> {
        Ok(crate::boot::declared_of(&self.raw_read()?.bay_layout()))
    }

    /// The slot's payloads into a free bay; the set's extlinux is composed from its command line.
    fn install(&self, slot: &str, payloads: &[&[u8]]) -> io::Result<()> {
        let protected = self.protected()?;
        self.raw()?
            .install_slot_with(slot, payloads, &protected)
            .map(|_| ())
            .map_err(io::Error::other)
    }

    fn free(&self, slot: &str) -> io::Result<()> {
        let protected = self.protected()?;
        self.raw()?
            .free_slot(slot, &protected)
            .map_err(io::Error::other)
    }

    fn restart(&mut self) -> io::Result<()> {
        // The script starts a trial that has attempts left on any restart.
        self.restarter.run(None)
    }

    /// Nothing is written to the slot's directory: the extlinux is in the bay (see [`Self::install`]).
    fn prepare(&self, _dir: &Path, _id: &str, _cmdline: &str) -> io::Result<()> {
        Ok(())
    }

    fn slot_bay(&self, id: &str) -> io::Result<(String, Vec<(String, u64)>)> {
        self.raw_read()?.slot_bay(id).map_err(io::Error::other)
    }

    /// The root copy is the install-time fallback (see [`Uboot`]): nothing is rewritten here.
    fn fallback_copy(&self, _layout: &Layout, _id: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod grant_tests;
#[cfg(test)]
pub(crate) mod hush;
#[cfg(test)]
mod tests;
