//! The board's operating system slots (`system.slots/1`) and its restart (`system.reboot/1`).
//!
//! The board side of slot management. Each slot's root filesystem is an EROFS image file on the
//! data partition (ext4), opened through dm-verity by the slot's initramfs; its kernel, device
//! trees and initramfs are in its own directory on the boot partition (FAT). Any number of slots
//! may be installed. The files and their formats are the board's boot chain's ([`store`] lists
//! them; `docs/design.md` names the reference implementation), so this driver and that boot
//! chain read each other's state. Lemnos carries the bytes: it checks a bundle against its signed
//! manifest and never looks inside the image.
//!
//! Settings, from the device's `config` in the board file:
//!
//! - `data`: the data partition's mount point, `"/data"` by default.
//! - `boot`: the boot partition's mount point, `"/boot/firmware"` by default.
//! - `record`: how the boot loader is told which slot to start: `"tryboot"` (the default: the
//!   firmware's `default.txt`, `trial.txt` and tryboot restart), `"uboot"` (U-Boot's standard
//!   boot flow: the record is U-Boot's own redundant environment on its own partition
//!   (`uboot_env_device` or `uboot_env_partuuid`, `uboot_env_size`, `uboot_env_offset`,
//!   `uboot_env_offset_redund`), a per-slot `extlinux.conf` and the static script in
//!   `packaging/uboot`; see [`uboot`]), `"ubi"` (the same on raw NAND, the record being the UBI
//!   volume `boot-record` and the script `packaging/uboot/boot-ubi.cmd`; needs `ubi`) or
//!   `"file"` (one `boot-record` file, for a loader with one fixed kernel; see [`boot`]).
//!   `docs/slots.md` describes each.
//! - `ubi`: on raw NAND, the UBI device (`"ubi0"`) whose static volumes hold the images, as
//!   `slot-<id>` ([`lemnos_slot_store::ubi`]). `data` and `boot` are then where its UBIFS
//!   volumes `data` and `boot` are mounted, and a slot's command line names the data volume as
//!   `<boot_args>.data=UBI=ubi0:data`. Unset, the images are files on the data partition.
//! - `restart_socket`: the restart helper's socket, `/run/lemnos-restart.sock` by default. The
//!   helper is a root service beside the daemon that restarts the board: through the service
//!   manager's `reboot.target`, or through BusyBox's init (see `packaging/README.md`, "The restart
//!   helper"). Before anything is written for a trial, the helper is asked whether it would restart.
//! - `keys`: given by the host from the board file's `[security] keys` (the board's one trust root,
//!   the same keys that authorize turning the switch off); a device that names it itself is refused.
//!   The file holds the keys the board trusts, one per line: 64 hex digits, a space, and the
//!   key's owner. A signed bundle's `slot.sig` is the 108-byte signature record, which names its
//!   key by id; the signature is checked against that key when the board holds it. The policy for each case is
//!   the table in [`lemnos_slot_store::signing`], shared with first flash. `list` shows each slot
//!   as `trusted` (with the owner as `signer`), `untrusted` or `unsigned`, and its `signature`
//!   word: `verified`, `not_checked_unknown_key` or `not_checked_unsigned`.
//!   key's owner. A bundle's `slot.sig`, when it has one, is always checked against them, and
//!   `list` shows each slot as `trusted` (with the owner as `signer`), `untrusted` or `unsigned`.
//! - The security switch (`lemnos_slot_store::security`, `docs/slots.md`, "The security switch"):
//!   on, a bundle not signed by a trusted key is refused. Read at each install's start. The board
//!   file's `require_signed` is refused by name (removed).
//! - `boot_args`: the name the slot's initramfs reads its kernel arguments under: a slot's
//!   command line gets `<boot_args>.slot=<id> <boot_args>.roothash=<hex>
//!   <boot_args>.data=PARTUUID=<uuid> panic=5`. Required for an install.
//! - The board file's `[board] id`, which every driver is given: a slot whose manifest names
//!   another `board` is refused at install, and never set as default or tried
//!   ([`manifest`] lists the manifest's keys).
//! - `partuuid`: the data partition's PARTUUID, which a slot's kernel command line names. Found
//!   from the mount table and `/dev/disk/by-partuuid` when not given.
//! - `reserve_mb`: free space an install leaves on the data partition (on raw NAND, on the UBI
//!   device), 2048 by default.
//! - `keep`: how many slots housekeeping keeps when the board has no setting of its own, 3 by
//!   default.
//! - `running`: the running slot, for a test; read from `board/slots/booted` otherwise.
//! - `boots_keep`: how many boots the boot history keeps (`lemnos/boots`), 16 by default.
//! - `crash_log`: the pstore mount (`/sys/fs/pstore`) a board declares a crash-log region in. Unset,
//!   there is no crash log. Set, the previous boot's record is stored in the boot history's crash
//!   index and pstore is cleared (see [`crash`], `docs/slots.md`, "The crash log"). `crash_bytes`
//!   bounds the stored tail (8192 by default, at most 65536).
//! - `reset_reason`: a file holding the board's reset reason, read at each boot for the history;
//!   unset, the first watchdog's `bootstatus` is read.
//!   The `system-reboot` driver's `data` (`/data` by default) is where it leaves the request
//!   the next boot's history names as `requested`.
//! - The trial settings, which [`trial`] and [`resolve`] apply: `auto_confirm` (true: the board
//!   confirms a healthy trial itself), `confirm_uptime_s` (30), `confirm_deadline_s` (600),
//!   `reboot_at_deadline` (true), `trial_attempts` (3, from 1 to 3) and `health`, a list of
//!   `device:<id>`, `file:<path>`, `socket:<path>` and `tcp:<address>:<port>` sources the trial
//!   must see good. The keys and defaults are in `docs/boards.md`; `docs/slots.md`, "How a trial
//!   resolves", says how they work.
//!
//! A bundle comes in two ways: streamed by its starter (`install`, then `write`), or as a file
//! a sender put in `<data>/slots/.incoming/` after asking `accept` whether it fits
//! (`install_file`, read a little on each tick). The file is deleted when its job ends, whatever
//! the outcome; `.incoming` is emptied when the driver starts and kept mode 0770, group `lemnos`.
//!
//! Failure rule: a call that can't read or write what it needs fails with the reason in the
//! board's state (`list` shows it) and changes nothing it can't finish; the next call tries
//! again. Nothing is kept across a restart of the service but the board's own files: an install
//! the restart cut short is cleared when the driver starts and reported as interrupted.

mod attempt;
/// The free space a live install keeps back when the board file names no `reserve_mb`.
pub const DEFAULT_RESERVE: u64 = 2048 << 20;

/// The health word while the boot records are another system's (route A: a slot added to a
/// disk whose boot partition is the existing system's). Stable: an operator's tooling matches it.
pub const FOREIGN: &str = "foreign: the boot records are not ours; the slots state here is another system's and is left untouched";

/// The health word while the boot records can't be read far enough to say whose they are.
pub const OWNER_UNKNOWN: &str = "foreign-unknown: the boot records can't be read to say whose they are; the slots state is left untouched until they can";

/// The health word while the state dir or the image dir can't be written on a board whose records
/// are ours: the unit grants no write there (`ReadWritePaths`, the slots drop-in), or the dirs are not
/// the daemon's user's (slot-init makes them so). `list` and `info` name the dir and the error.
/// A write of the slots state failed and no later one in the same dir has succeeded (static, for the
/// driver's health; `info` and `list` name the file and the error as `write_failed`).
pub const WRITE_FAILED: &str = "write-failed: a write of the slots state failed, so what the board shows may be behind what it did";

/// The state's word after a trial's restart failed: the trial was disarmed (see the state's error).
pub const RESTART_FAILED: &str = "restart-failed";
pub const STATE_UNWRITABLE: &str = "state-unwritable: the slots state, image or boot-file dir can't be written by the daemon, so no slot can be installed, switched or confirmed";

/// Whether the daemon can write its state dir and its image dir: a file is made in each and removed.
/// Only on a board whose records are ours (another system's dirs are never touched).
///
/// # Errors
///
/// The first dir that can't be written, and why.
fn state_dirs_writable(layout: &Layout) -> core::result::Result<(), String> {
    // Every dir the daemon writes in (`Layout::daemon_dirs`), the boot-file store included: a store
    // the daemon can't write fails an install only at its finish, after every byte (BOOTFILES-OWNER).
    for dir in layout.daemon_dirs() {
        // A dir that isn't there yet is made, as the writer makes it; one the daemon can't make is
        // the first error.
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let probe = dir.join(format!(".lemnosd-write-check-{}", std::process::id()));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        fs::remove_file(&probe).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// How often an unknown owner is looked at again: the failure rule's cap.
const OWNER_RETRY_NS: u64 = 8_000_000_000;

#[cfg(test)]
mod bay_fixture;
pub mod boot;
mod boot_guard;
#[cfg(test)]
mod boot_guard_tests;
#[cfg(test)]
mod boot_writes_audit;
pub mod budget;
mod count;
#[cfg(test)]
mod count_tests;
pub mod grow;
mod install;
mod medium;
pub mod offline;
#[cfg(test)]
mod reboot_to_tests;
mod resolve;
pub use lemnos_slot_store::tar;
pub use lemnos_slot_store::{json, manifest, store};
pub mod crash;
pub mod trial;
pub mod uboot;
pub mod words;

#[cfg(test)]
mod offline_tests;
#[cfg(test)]
mod route_a_tests;
#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lemnos_contract_indicator::{Indicator as IndicatorContract, NO_PROGRESS};
use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_contract_system_reboot::Reboot;
use lemnos_contract_system_slots::Slots;
use lemnos_core::{
    BoardView, CallerId, Config, ConfigError, Dev, Health, JobId, JobState, Key, Kind, Lifecycle,
    Reply, Result, Retry, Status, driver,
};

use attempt::{Attempt, Ended};
use boot::{BootRecord, Check, FileRecord, Restarter, Tryboot};
pub use install::{Admit, SIGNATURE_LEN, TrustedKey, parse_keys, signature_check};
use install::{Failure, Finish, Guard, INCOMING, Install, NO_SPACE};
use json::quoted;
use lemnos_slot_store::boot_node;
use lemnos_slot_store::boot_raw::Ownership;
use lemnos_slot_store::boots;
use lemnos_slot_store::disk::Table;
use lemnos_slot_store::security;
use lemnos_slot_store::signing::{self, Outcome};
use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};
use lemnos_slot_store::uboot_env::Place;
pub use offline::{
    FirstSlot, OfflineBoard, check_bay, check_bay_manifest, check_layout, open_boot_image,
    write_first_slot,
};
use store::{DEFAULT_KEEP, Layout, Meta, State, read_line, valid_id};
use trial::{Log, Policy, Resolved, Source};
use uboot::{TrialUpdates, Uboot, UbootEnv};

#[driver("system-slots", provides = [Slots, SecurityPrecondition])]
pub struct SystemSlots {
    layout: Layout,
    record: Box<dyn BootRecord>,
    keys: Vec<TrustedKey>,
    /// The security switch's view (`lemnos_slot_store::security`): read at each install's start.
    partuuid: String,
    boot_args: String,
    /// The board's `boot_sources` entries as given (`source=path`): each slot's command line must
    /// carry `<boot_args>.src.<entry>` for every one.
    boot_sources: Vec<String>,
    /// Whether trial updates are available on this board, as the record last found it (see
    /// [`uboot::TrialUpdates`]); `info` and `health` say it, and `trial` checks it again before it writes.
    trial_updates: TrialUpdates,
    /// Why the boot partition's `config.txt` cannot select the default, read when the driver is made (the
    /// record's own check, `BootRecord::config_refusal`). `None` when it can. The driver still runs: its
    /// health says so, `list` carries it, and the writes that need the file are refused by name.
    config_refusal: Option<&'static str>,
    /// Whether the boot records are this system's (`BootRecord::ownership`). While they are not, the
    /// slots state here is another system's: nothing is written at start, on a tick or by a call,
    /// and every call that would write is refused (see [`SystemSlots::ours`]). `Ours` and `NotOurs`
    /// are kept; `Unknown` is looked at again on each tick and call.
    owner: Ownership,
    /// Why the state dir or the image dir can't be written, on a board whose records are ours
    /// (see [`state_dirs_writable`]): `None` when both can. Looked at again every 8 s while set.
    state_unwritable: Option<String>,
    /// The words for the refusal the method being run is about to return, taken by
    /// [`Lifecycle::refusal`] right after it.
    why: Option<String>,
    /// When the state dirs are next looked at while they can't be written (monotonic ns).
    state_check_ns: u64,
    /// When the record's trial updates are next looked at (monotonic ns), while the board has a record
    /// that can say no.
    trial_watch_ns: u64,
    /// The boot partition's block node, found from the boot chain's source and `boot_sources`
    /// (refused by name when not found). Its reason record is written raw here.
    boot_node: core::result::Result<PathBuf, String>,
    /// The board file's `[board] id`. A slot built for another board is never booted.
    board_id: String,
    /// The medium the board booted from, in place of [`medium::booted`] (a stand-in for tests of a
    /// medium the test host does not have). `None` on a board: the medium is read from the system.
    booted_override: Option<String>,
    running: Option<String>,
    reserve: u64,
    keep: u32,
    calling: CallerId,
    install: Option<Install>,
    next_job: u64,
    /// The board's indicator, when it has one: it shows the install's phase.
    #[needs(optional)]
    indicator: Option<Dev<dyn IndicatorContract>>,
    /// The state last sent to the indicator, so only a change is sent.
    shown: Option<(&'static str, u16)>,
    /// The buffer an install from a file reads through, kept between ticks.
    read_buf: Vec<u8>,
    /// The clock a tick's install work is timed by ([`budget`]): the monotonic clock, or a test's.
    clock: budget::Clock,
    /// How long one tick's install work may run: a quarter of the host's call deadline.
    tick_limit_ns: u64,
    /// On raw NAND, what changes the UBI device's volumes (`layout.ubi` names the device).
    ubi_write: Option<Arc<dyn UbiWrite>>,
    /// The trial settings, and the health sources the board file names.
    policy: Policy,
    sources: Vec<Source>,
    /// One per source, kept between looks (a socket's busy time).
    probes: Vec<trial::Probe>,
    /// A deadline restart waiting to write its reason (see [`resolve`]).
    giving_up: Option<resolve::GiveUp>,
    /// Why the driver's record of the trial could not be written, when it could not.
    record_error: Option<String>,
    /// This boot's id: a trial's attempt is given back once per boot.
    boot: String,
    /// When the driver first ticked (monotonic) and when the board's boot began (wall clock).
    started_ns: Option<u64>,
    started_wall: SystemTime,
    /// The board as the host showed it at the last look (see [`resolve`]).
    seen: Vec<trial::Dev>,
    looked: bool,
    /// Whether this boot's device baseline is written.
    snapshot_written: bool,
    /// How many boots the history keeps (`boots_keep`).
    boots_keep: usize,
    /// The declared crash log (`crash_log`) and its tail size (`crash_bytes`), and what the last
    /// boot's record step said, for `info`.
    crash: Option<Box<dyn crash::Pstore>>,
    crash_bytes: usize,
    crash_note: String,
    /// Where the board's reset reason is read from, when the board file names a file for it.
    reset_reason: Option<PathBuf>,
    /// The last install's signature result, and the slot it made (`""` when it was refused):
    /// `info` shows it.
    last_signed: Option<(String, std::result::Result<Outcome, String>)>,
    /// How the last install attempt stands or ended, for `info`: kept for the daemon's life, and
    /// replaced when the next attempt starts (see [`attempt`]).
    attempt: Option<Attempt>,
    /// The boot-file check before lemnosd's own restarts and on a slow period (see
    /// [`boot_guard`]), and when the next slow-period check is due.
    check: Option<Check>,
    next_check_ns: u64,
    /// The slow period's look ([`boot::Look`]) and the answer of the one running on its thread.
    look: Option<boot::Look>,
    looking: Option<std::sync::mpsc::Receiver<boot_guard::Seen>>,
    /// How the board is restarted: asked before a trial is armed, and watched after a restart the
    /// helper accepted, so one that fails after all disarms the trial (see [`boot::Watch`]).
    restarter: Option<Restarter>,
    /// The firmware's boot count and mailbox, for the `tryboot` form (see [`count`]).
    firmware: Option<count::Firmware>,
    /// While the boot records can't be read to say whose they are: whether this boot's count has been
    /// cleared once the daemon ran its uptime (see [`count::clear_unjudged`]).
    unjudged_cleared: bool,
    /// What that clear did, for `info` and `list` (beside the foreign words): never written to the
    /// state, which is not known to be this system's.
    unjudged_note: Option<String>,
    /// `[slots] demote_after_episodes`: the episodes that demote a default (see [`count`]).
    demote_after: u8,
    /// The free bytes the offline writer names in place of the data partition's (see [`offline`]).
    offline_free: Option<u64>,
    /// A boot count the firmware refused to clear, retried from the tick until it clears (see
    /// [`count::Uncleared`]). `health`, `retrying` and `info` say so while it is set.
    uncleared: Option<count::Uncleared>,
    /// The data partition's grow to the disk's end, while it has not finished (see [`grow`]).
    /// `None` when the board's `grow_data` is `false`, or the grow is done.
    grow: Option<grow::Task>,
}

/// The slot layout the settings name, and the UBI writer when the layout is on raw NAND.
fn layout_of(cfg: &Config<'_>) -> (Layout, Option<Arc<dyn UbiWrite>>) {
    let mut layout = Layout::new(
        PathBuf::from(cfg.str("data").unwrap_or("/data")),
        PathBuf::from(cfg.str("boot").unwrap_or("/boot/firmware")),
    );
    layout.ubi = cfg.str("ubi").map(|device| Ubi {
        sys: PathBuf::from("/sys"),
        dev: PathBuf::from("/dev"),
        device: device.to_string(),
    });
    let ubi_write: Option<Arc<dyn UbiWrite>> = layout
        .ubi
        .clone()
        .map(|ubi| Arc::new(ubi::System { ubi }) as Arc<dyn UbiWrite>);
    (layout, ubi_write)
}

/// The boot partition's block node, found from the boot chain's source and the board's
/// `boot_sources`; refused by name when it can't be.
fn boot_node_of(cfg: &Config<'_>) -> core::result::Result<PathBuf, String> {
    match boot_table(cfg) {
        Ok(Some(table)) => boot_node::node_for_table(&table),
        Ok(None) => Err("raw NAND keeps its record in the UBI volume: no boot disk".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The daemon's root phase for a slots device, run as root before the drop to the daemon's user,
/// which can't open the boot node by name. For the `tryboot` record the boot partition is probed
/// read only and handed over read-write only when its records are ours
/// (`lemnos_slot_store::boot_raw::prepare`). For the `uboot` record the boot partition is handed over
/// read only, and the environment's own partition (the GPT form) or named device is handed over, read-write
/// unless the boot partition holds the FAT environment ([`uboot::root_phase`]). Raw NAND's volumes come and
/// go and are not handed over: under the daemon's own user their writes fail, by name, in health. Returns one
/// line for the daemon's log.
#[must_use]
pub fn root_phase(cfg: &Config<'_>) -> String {
    use lemnos_slot_store::boot_raw::{Prepared, prepare};
    match cfg.str("record").unwrap_or("tryboot") {
        "tryboot" => {
            let boot = match boot_node_of(cfg) {
                Ok(node) => match prepare(&node) {
                    Prepared::ReadWrite => format!("{}: handed over read-write", node.display()),
                    Prepared::ReadOnly(why) => {
                        format!("{}: handed over read only ({why})", node.display())
                    }
                    Prepared::Failed(why) => format!("not handed over: {why}"),
                },
                Err(why) => format!("the boot partition isn't known: {why}"),
            };
            // The firmware mailbox the boot count is cleared through: root-only on a board, so it is
            // held across the drop and every clear takes a copy of the held descriptor.
            let vcio = firmware_of(cfg).map(|firmware| firmware.vcio);
            match vcio {
                Some(vcio) => match lemnos_driver_rpi_firmware_sys::hold(&vcio) {
                    Ok(()) => format!("{boot}; {}: held for the boot count", vcio.display()),
                    Err(why) => format!(
                        "{boot}; {}: not held ({why}): the boot count can't be cleared",
                        vcio.display()
                    ),
                },
                None => boot,
            }
        }
        "uboot" => match uboot_env_place(cfg) {
            Ok(place) => uboot::root_phase(&place, &boot_disk_of(cfg), &boot_node_of(cfg)),
            Err(_) => {
                "the uboot record's environment settings are refused: nothing is handed over".into()
            }
        },
        other => format!(
            "the {other} record's node is not handed over: its writes need root, and fail by name under the daemon's user"
        ),
    }
}

/// The daemon's root phase for a restart device: the firmware mailbox, root-only on a board, is held across the
/// drop for the `tryboot` record, so the guard's boot count and the way back (`reboot_to`) take copies of the held
/// descriptor. The other records have no firmware and hold nothing. Returns one line for the daemon's log.
#[must_use]
pub fn reboot_root_phase(cfg: &Config<'_>) -> String {
    match firmware_of(cfg) {
        Some(firmware) => match lemnos_driver_rpi_firmware_sys::hold(&firmware.vcio) {
            Ok(()) => format!("{}: held for the restart", firmware.vcio.display()),
            Err(why) => format!(
                "{}: not held ({why}): the boot count and the way back can't be asked",
                firmware.vcio.display()
            ),
        },
        None => "no firmware mailbox for this record: nothing held".into(),
    }
}

/// The boot disk's whole-disk node (`/dev/<disk>`), found as the boot partition is: from the boot
/// chain's source and the board's `boot_sources`. The U-Boot environment is read and written on it
/// (the GPT form finds its partition there); refused by name when the disk can't be found.
fn boot_disk_of(cfg: &Config<'_>) -> core::result::Result<PathBuf, String> {
    match boot_table(cfg) {
        Ok(Some(table)) => boot_node::disk_for_table(&table),
        Ok(None) => Err("raw NAND keeps its record in the UBI volume: no boot disk".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The firmware's count and mailbox, for the `tryboot` form only: the other forms have no count.
fn firmware_of(cfg: &Config<'_>) -> Option<count::Firmware> {
    (cfg.str("record").unwrap_or("tryboot") == "tryboot").then(|| count::Firmware {
        device_tree: PathBuf::from(cfg.str("device_tree").unwrap_or("/proc/device-tree")),
        vcio: PathBuf::from(
            cfg.str("vcio")
                .unwrap_or(lemnos_driver_rpi_firmware_sys::DEFAULT_DEVICE),
        ),
    })
}

fn restarter(cfg: &Config<'_>, check: Option<Check>) -> Restarter {
    Restarter {
        socket: PathBuf::from(
            cfg.str("restart_socket")
                .unwrap_or(lemnos_restart::client::DEFAULT_SOCKET),
        ),
        check,
        watch: boot::Watch::default(),
        #[cfg(test)]
        keep: None,
    }
}

/// The board's `boot_sources` table, parsed. Required on every board that boots from a disk (every
/// record but `ubi`); `None` on the raw NAND form, which has no such table.
fn boot_table(cfg: &Config<'_>) -> core::result::Result<Option<Table>, ConfigError<'static>> {
    if cfg.str("record") == Some("ubi") {
        return Ok(None);
    }
    let items = cfg
        .list("boot_sources")
        .ok_or(ConfigError::new(
            "boot_sources",
            "is required on a board that boots from a disk: source=device-tree path entries (see docs/boards.md)",
        ))?;
    Table::parse(items)
        .map(Some)
        .map_err(|_| ConfigError::new(
            "boot_sources",
            "must list source=device-tree path entries: a source is rpi-boot-mode-<n> or a path starting with /",
        ))
}

/// The boot record the `record` setting names, with its restarter (checked when `check` is given).
fn record_for(
    cfg: &Config<'_>,
    layout: &Layout,
    ubi_write: &Option<Arc<dyn UbiWrite>>,
    check: Option<Check>,
) -> Result<Box<dyn BootRecord>> {
    record_with(cfg, layout, ubi_write, restarter(cfg, check))
}

/// The boot record the `record` setting names, restarting through `restarter`.
fn record_with(
    cfg: &Config<'_>,
    layout: &Layout,
    ubi_write: &Option<Arc<dyn UbiWrite>>,
    restarter: Restarter,
) -> Result<Box<dyn BootRecord>> {
    Ok(match cfg.str("record").unwrap_or("tryboot") {
        "tryboot" => Box::new(Tryboot {
            boot: layout.boot.clone(),
            node: boot_node_of(cfg),
            restarter,
        }),
        "file" => Box::new(FileRecord {
            boot: layout.boot.clone(),
            restarter,
        }),
        "uboot" => Box::new(UbootEnv {
            disk: boot_disk_of(cfg),
            place: uboot_env_place(cfg).map_err(|_| Status::INVALID_ARGUMENT)?,
            restarter,
            boot_node: boot_node_of(cfg),
            partitions: None,
        }),
        "ubi" => match (&layout.ubi, ubi_write) {
            (Some(device), Some(writer)) => Box::new(Uboot {
                boot: layout.boot.clone(),
                restarter,
                volume: Some((device.clone(), writer.clone())),
            }),
            _ => return Err(Status::INVALID_ARGUMENT),
        },
        _ => return Err(Status::INVALID_ARGUMENT),
    })
}

/// The U-Boot environment's place: GPT (the one partition of the environment's type GUID on the boot
/// disk, only `uboot_env_size` stated) or, with `uboot_env_device`, fixed offsets of that device.
/// `uboot_env_partuuid` is refused by name: the environment is found by type, never by a PARTUUID.
fn uboot_env_place(cfg: &Config<'_>) -> core::result::Result<Place, ConfigError<'static>> {
    if cfg.str("uboot_env_partuuid").is_some() {
        return Err(ConfigError::new(
            "uboot_env_partuuid",
            "is not a key: the U-Boot environment is found by its GPT partition type GUID 3de21764-95bd-54bd-a5c3-4abe786f38a8 on the boot disk (use uboot_env_device with offsets for an environment outside the GPT)",
        ));
    }
    let size = cfg
        .int("uboot_env_size")
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(ConfigError::new(
            "uboot_env_size",
            "is required for the uboot record: CONFIG_ENV_SIZE in bytes",
        ))?;
    if let Some(why) = uboot::size_refusal(size) {
        return Err(ConfigError::new("uboot_env_size", why));
    }
    let int = |key: &'static str| {
        cfg.int(key)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(ConfigError::new(
                key,
                "is required with uboot_env_device: the copy's byte offset on the device",
            ))
    };
    match cfg.str("uboot_env_device") {
        None => {
            for key in ["uboot_env_offset", "uboot_env_offset_redund"] {
                if cfg.int(key).is_some() {
                    return Err(ConfigError::new(
                        key,
                        "needs uboot_env_device: the GPT form places both copies itself",
                    ));
                }
            }
            Place::gpt(size).map_err(|_| {
                ConfigError::new("uboot_env_size", "is too small: CONFIG_ENV_SIZE is at least 16 bytes")
            })
        }
        Some(device) => Place::device(
            device,
            size,
            int("uboot_env_offset")?,
            int("uboot_env_offset_redund")?,
        )
        .map_err(|_| {
            ConfigError::new(
                "uboot_env_device",
                "must be an absolute block device path (not /dev/mtd* or /dev/ubi*) with sector-aligned, non-overlapping offsets and a size of at least 16 bytes",
            )
        }),
    }
}

/// The check over a record of its own: [`boot_guard::check`], which may point the record at the
/// previous slot. Before a restart (`restart`) it also clears the firmware's boot count, after the
/// check has passed: an intended restart never counts.
fn guard_check(
    layout: Layout,
    node: core::result::Result<PathBuf, String>,
    record: Box<dyn BootRecord>,
    firmware: Option<count::Firmware>,
) -> Check {
    let record = Mutex::new(record);
    Check(Arc::new(move |restart| {
        let mut record = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Another system's boot records: its state is never read to move a pointer or written.
        if record.ownership() != Ownership::Ours {
            return Ok(None);
        }
        let note = boot_guard::check(&layout, &node, record.as_mut(), restart)?;
        // The U-Boot form's restart count is cleared before an intended restart too, with a write only
        // when it is not already 0 (see `count`).
        if restart
            && firmware.is_none()
            && let Ok(Some(restarts)) = record.restarts()
            && restarts != 0
            && let Err(e) = record.set_restarts(0)
        {
            let mut state = State::load(&layout);
            state.error = format!(
                "the restart count couldn't be cleared before this restart ({e}): this boot is counted"
            );
            let _ = state.save(&layout);
        }
        if restart && let Some(firmware) = &firmware {
            // A count that can't be cleared does not stop the restart the board was asked for: the
            // restart then counts one boot, and the state says so.
            if let Err(e) = count::ClearCount::clear_count(firmware) {
                let mut state = State::load(&layout);
                state.error = format!(
                    "the boot count couldn't be cleared before this restart ({e}): this boot is counted"
                );
                let _ = state.save(&layout);
            }
        }
        Ok(note)
    }))
}

/// The slow period's look over a record of its own: [`boot_guard::look`], which writes nothing. Run on
/// the look's thread, so it never holds the check's record while a restart waits for it.
fn guard_look(
    layout: Layout,
    node: core::result::Result<PathBuf, String>,
    record: Box<dyn BootRecord>,
) -> boot::Look {
    let record = Mutex::new(record);
    boot::Look(Arc::new(move || {
        let record = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if record.ownership() != Ownership::Ours {
            return boot_guard::Seen::Fine;
        }
        boot_guard::look(&layout, &node, record.as_ref())
    }))
}

impl SystemSlots {
    /// The driver over a layout and a boot record of the caller's choosing.
    pub fn new(layout: Layout, record: Box<dyn BootRecord>, keys: Vec<TrustedKey>) -> Self {
        Self::with_volumes(layout, record, keys, None)
    }

    /// The driver with its images in UBI volumes, changed through `ubi_write`, when the layout
    /// names a UBI device.
    pub fn with_volumes(
        layout: Layout,
        record: Box<dyn BootRecord>,
        keys: Vec<TrustedKey>,
        ubi_write: Option<Arc<dyn UbiWrite>>,
    ) -> Self {
        let layout_has_ubi = layout.ubi.is_some();
        let config_refusal = record.config_refusal();
        let owner = record.ownership();
        let mut slots = Self {
            layout,
            record,
            keys,
            partuuid: String::new(),
            boot_args: String::new(),
            boot_sources: Vec::new(),
            boot_node: core::result::Result::Err("the board file's boot_sources is not set".into()),
            board_id: String::new(),
            booted_override: None,
            running: None,
            reserve: DEFAULT_RESERVE,
            keep: DEFAULT_KEEP,
            calling: CallerId::LOCAL,
            install: None,
            next_job: 0,
            indicator: None,
            shown: None,
            read_buf: Vec::new(),
            clock: budget::monotonic(),
            tick_limit_ns: budget::limit_ns(0),
            ubi_write: ubi_write.filter(|_| layout_has_ubi),
            policy: Policy::default(),
            sources: Vec::new(),
            probes: Vec::new(),
            giving_up: None,
            record_error: None,
            boot: String::new(),
            started_ns: None,
            started_wall: SystemTime::now(),
            seen: Vec::new(),
            looked: false,
            snapshot_written: false,
            boots_keep: boots::DEFAULT_KEEP,
            crash: None,
            crash_bytes: crash::DEFAULT_BYTES,
            crash_note: String::new(),
            reset_reason: None,
            last_signed: None,
            attempt: None,
            check: None,
            firmware: None,
            unjudged_cleared: false,
            unjudged_note: None,
            demote_after: lemnos_slot_store::fallback::threshold_from_board(None).unwrap_or(1),
            next_check_ns: 0,
            look: None,
            looking: None,
            restarter: None,
            offline_free: None,
            uncleared: None,
            grow: None,
            trial_updates: TrialUpdates::Available,
            config_refusal,
            owner,
            state_unwritable: None,
            why: None,
            state_check_ns: 0,
            trial_watch_ns: 0,
        };
        if !slots.ours() {
            // Another system's state: nothing here is ours to clear.
            return slots;
        }
        slots.state_unwritable = state_dirs_writable(&slots.layout).err();
        slots.clear_interrupted();
        // The one thing the board's own record keeps of an install is that a restart cut it short.
        let kept = State::load(&slots.layout);
        if kept.state == "error" && kept.error == attempt::INTERRUPTED {
            slots.attempt = Some(Attempt::restart_cut_short());
        }
        // What a sender left in `.incoming` belongs to no install now.
        let _ = install::reset_incoming(&slots.layout);
        slots
    }

    /// Whether the boot records are ours, so the slots state may be written. An unknown answer is
    /// looked at again each time; `Ours` and `NotOurs` are kept.
    fn ours(&mut self) -> bool {
        if matches!(self.owner, Ownership::Unknown(_)) {
            self.owner = self.record.ownership();
        }
        self.owner == Ownership::Ours
    }

    /// `FAILED_PRECONDITION` while the boot records are not ours, or while the daemon can't write
    /// its state or image dir: a call that would write the slots state is refused before it reads
    /// or writes anything, so no boot record is switched that the state could not follow. An
    /// unwritable dir is looked at again first, so a grant given since is taken at once.
    fn writable(&mut self) -> Result<()> {
        if !self.ours() {
            let words = self.foreign_text();
            return Err(self.refuse(Status::FAILED_PRECONDITION, words));
        }
        if self.state_unwritable.is_some() {
            self.state_unwritable = state_dirs_writable(&self.layout).err();
        }
        if let Some(why) = &self.state_unwritable {
            let words = format!("{}: {why}", words::STATE_UNWRITABLE);
            return Err(self.refuse(Status::FAILED_PRECONDITION, words));
        }
        Ok(())
    }

    /// `status`, with `words` for the caller ([`Lifecycle::refusal`]): the cause the status
    /// alone can't tell apart from another.
    fn refuse(&mut self, status: Status, words: impl Into<String>) -> Status {
        self.why = Some(words.into());
        status
    }

    /// Why the slots state is not written, `""` when the boot records are ours: `list` and `info`
    /// carry it as `foreign`.
    fn foreign_text(&self) -> String {
        match &self.owner {
            Ownership::Ours => String::new(),
            Ownership::NotOurs(why) => format!("{FOREIGN}: {why}"),
            Ownership::Unknown(why) => match &self.unjudged_note {
                Some(note) => format!("{OWNER_UNKNOWN}: {why}; {note}"),
                None => format!("{OWNER_UNKNOWN}: {why}"),
            },
        }
    }

    /// An install the service's restart cut short: its work goes, and the board's state says so.
    fn clear_interrupted(&mut self) {
        if install::clear_leftovers(&self.layout, self.ubi_write.as_deref()) {
            let mut state = State::load(&self.layout);
            if state.state == "staging" {
                state.state = "error".into();
                state.error = attempt::INTERRUPTED.into();
                let _ = state.save(&self.layout);
            }
        }
    }

    fn running(&self) -> String {
        self.running
            .clone()
            .unwrap_or_else(|| read_line(&self.layout.marker("booted")))
    }

    fn state(&self) -> State {
        State::load(&self.layout)
    }

    fn save(&self, state: &State) -> Result<()> {
        state.save(&self.layout).map_err(|_| Status::IO)
    }

    /// The last failed write in the state or image dir (`store::replace` keeps it), until a later
    /// write there succeeds.
    fn write_failed(&self) -> Option<String> {
        store::write_failure(&self.layout.state_dir())
            .or_else(|| store::write_failure(&self.layout.images()))
    }

    /// The slots that keep their directory on the boot partition, and `also`.
    /// Nothing is evicted from the boot partition any more: its bays are fixed and a bay is freed by
    /// its record alone ([`remove_slot`]). Kept as the one place a switch would prune, for callers.
    fn evict(&self, _state: &State, _also: &str) -> Result<()> {
        Ok(())
    }

    fn installed(&self, id: &str) -> Result<()> {
        if self.layout.installed(id) {
            Ok(())
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    fn mark_keep_old(&self, id: &str, keep_old: bool) -> Result<()> {
        if keep_old {
            store::replace(
                &self.layout.marker("keep-old"),
                format!("{id}\n").as_bytes(),
            )
            .map_err(|_| Status::IO)?;
        }
        Ok(())
    }

    fn fail_install(&mut self, install: &mut Install, failure: Failure) -> Status {
        let status = failure.0;
        self.show("failed", NO_PROGRESS);
        self.unwind(install);
        install.discard();
        let ended = if install.refused {
            Ended::Refused
        } else {
            Ended::Failed
        };
        self.end_attempt(ended, &failure.1, Some(install));
        let mut state = State::load(&self.layout);
        state.state = "error".into();
        state.error = failure.1.clone();
        state.progress = 0;
        let _ = state.save(&self.layout);
        install.end = Some(Err(failure));
        status
    }

    /// Records how the attempt ended, with the bytes and slot of `install` when there is one.
    fn end_attempt(&mut self, ended: Ended, reason: &str, install: Option<&Install>) {
        let now = lemnos_linux_sys::time::monotonic_ns().ok();
        let Some(attempt) = self.attempt.as_mut() else {
            return;
        };
        attempt.ended = ended;
        attempt.reason.set(reason);
        attempt.ended_ns = now;
        if let Some(install) = install {
            attempt.received = Some(install.got);
            attempt.total = Some(install.size);
            attempt.slot.set(install.slot_id().unwrap_or(""));
        }
    }

    /// Starts the record of a new attempt, replacing the last one's.
    fn start_attempt(&mut self, total: u64) {
        let now = lemnos_linux_sys::time::monotonic_ns().ok();
        self.attempt = Some(Attempt::running(self.calling, total, now));
    }

    /// An attempt turned away before it started: the record says so, and the reason is the one the
    /// starter's call was given.
    fn refuse_attempt(&mut self, size: u64, reason: &str) {
        self.start_attempt(size);
        self.end_attempt(Ended::Refused, reason, None);
    }

    /// The install's next bytes. The last of them ends the bytes; the finish's steps then run from the
    /// driver's ticks (see [`SystemSlots::finish_step`]), so no call waits for the finish.
    fn feed(&mut self, install: &mut Install, offset: u64, bytes: &[u8]) -> Result<()> {
        if let Err(failure) = install.take(offset, bytes) {
            // A write out of order changes nothing: the install goes on from where it is.
            if offset != install.got {
                return Err(failure.0);
            }
            // A refusal the bundle's members made (its signature, its order) is kept for `info`.
            if let Some(signed) = install.signed.clone() {
                self.last_signed = Some((String::new(), signed));
            }
            return Err(self.fail_install(install, failure));
        }
        if !install.complete() {
            // Bytes arriving: show the progress as they do, not only on ticks.
            self.show("receiving", permille(install.got, install.size));
            return Ok(());
        }
        install.begin_finish();
        self.show("verifying", NO_PROGRESS);
        Ok(())
    }

    /// Runs the next step of an install's finish: what a tick does while one is due. A step is short,
    /// so the device answers other calls between steps. A step that fails ends the install.
    fn finish_step(&mut self, budget: &mut budget::Budget<'_>) {
        if !self.install.as_ref().is_some_and(Install::finishing) {
            return;
        }
        let Some(mut install) = self.install.take() else {
            return;
        };
        let finish = Finish {
            layout: &self.layout,
            boot_args: &self.boot_args,
            boot_sources: &self.boot_sources,
            guard: Guard {
                running: self.running(),
                default: self.state().default,
            },
            record: &*self.record,
        };
        let done = install.step(&finish, budget);
        match done {
            Ok(None) => {}
            Ok(Some(id)) => {
                if let Some(signed) = install.signed.clone() {
                    self.last_signed = Some((id.clone(), signed));
                }
                match self.stage(&id, install.keep_old) {
                    Ok(()) => {
                        // The bays are written whether or not the loader will act on them. When it won't, the reply says so.
                        let reason = match self.record.trial_updates() {
                            TrialUpdates::Available => String::new(),
                            TrialUpdates::Unavailable(why) => format!(
                                "the boot loader will not start it: {}",
                                why.reason.sentence()
                            ),
                        };
                        install.drop_source();
                        install.end = Some(Ok(id));
                        self.end_attempt(Ended::Installed, &reason, Some(&install));
                        self.show("done", NO_PROGRESS);
                    }
                    Err(status) => {
                        let reason =
                            "the slot is in place, but the install's record could not be written";
                        self.fail_install(&mut install, (status, reason.into()));
                    }
                }
            }
            Err(failure) => {
                if let Some(signed) = install.signed.clone() {
                    self.last_signed = Some((String::new(), signed));
                }
                self.fail_install(&mut install, failure);
            }
        }
        self.install = Some(install);
    }

    /// Runs every finish step now, as the ticks would, one after another. For the offline writer, which
    /// has no host to tick it.
    pub(crate) fn finish_all(&mut self) {
        // No call deadline here: each step runs as far as it goes (a flush still yields its step).
        let clock = self.clock.clone();
        while self.install.as_ref().is_some_and(Install::finishing) {
            let mut budget = budget::Budget::new(&*clock, u64::MAX, u64::MAX);
            self.finish_step(&mut budget);
        }
    }

    /// Makes an installed slot the staged one, with its keep-old marker when the install asked for it.
    fn stage(&mut self, id: &str, keep_old: bool) -> Result<()> {
        self.mark_keep_old(id, keep_old)?;
        let mut state = self.state();
        state.staged = id.to_string();
        state.state = "staged".into();
        state.error.clear();
        state.progress = 1000;
        self.save(&state)
    }

    /// Frees the bay of an install that ends before its commit (see [`Install::bay_slot`]): no
    /// pointer and no image names it, so the bay goes back.
    fn unwind(&self, install: &Install) {
        if let Some(id) = install.bay_slot() {
            let _ = self.record.free(id);
        }
    }

    /// Whether a bundle of `size` bytes fits with the reserve; the reason in the state when not.
    /// Whether slot `id` was built for this board: a slot for another is listed, never booted.
    fn fits_board(&self, id: &str) -> Result<()> {
        let built_for = fs::read_to_string(self.layout.manifest(id))
            .ok()
            .and_then(|text| json::Json::parse(&text))
            .and_then(|m| {
                m.get("board")
                    .and_then(json::Json::text)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if !self.board_id.is_empty() && built_for == self.board_id {
            return Ok(());
        }
        let mut state = self.state();
        state.error = if self.board_id.is_empty() {
            "the board file has no [board] id to match a slot's board against".to_string()
        } else if built_for.is_empty() {
            format!("slot {id} names no board it was built for, so it isn't booted here")
        } else {
            format!(
                "slot {id} is built for board {built_for}, and this is board {}",
                self.board_id
            )
        };
        let _ = self.save(&state);
        Err(Status::NOT_SUPPORTED)
    }

    /// Whether slot `id`'s `medium` is the medium the board booted from, or either is unknown. A trial
    /// of a slot for another medium is refused by name, with the reason in the state.
    fn fits_medium(&self, id: &str) -> Result<()> {
        let slot = self.manifest_text(id, "medium");
        let Some(why) = medium::mismatch(id, &slot, &self.booted_medium()) else {
            return Ok(());
        };
        let mut state = self.state();
        state.error = format!("{why}: a trial of it is refused");
        let _ = self.save(&state);
        Err(Status::NOT_SUPPORTED)
    }

    /// The medium the board booted from: [`medium::booted`], or the test stand-in. Empty when unknown.
    fn booted_medium(&self) -> String {
        match &self.booted_override {
            Some(medium) => medium.clone(),
            None => medium::booted().unwrap_or("").to_string(),
        }
    }

    /// A text key of slot `id`'s `slot.json` as written, empty when the slot records none or can't
    /// be read.
    fn manifest_text(&self, id: &str, key: &str) -> String {
        fs::read_to_string(self.layout.manifest(id))
            .ok()
            .and_then(|text| json::Json::parse(&text))
            .and_then(|m| m.path(key).and_then(json::Json::text).map(str::to_string))
            .unwrap_or_default()
    }

    /// [`Self::manifest_text`] for `list` and `info`: `not recorded` when the slot records none.
    fn recorded(&self, id: &str, key: &str) -> String {
        match self.manifest_text(id, key) {
            value if value.is_empty() => "not recorded".to_string(),
            value => value,
        }
    }

    /// With `require_signed`, a slot may be set as the default or tried only when its signature
    /// verified. The reason in the state when not.
    fn fits_signature(&self, id: &str) -> Result<()> {
        if !security::enforced() {
            return Ok(());
        }
        let meta = Meta::load(&self.layout, id);
        if signing::recorded(meta.get("SIGNATURE"), meta.get("SIGNED")) == signing::VERIFIED {
            return Ok(());
        }
        let mut state = self.state();
        state.error = format!(
            "slot {id} is not signed by a key this board trusts, and this board takes only those"
        );
        let _ = self.save(&state);
        Err(Status::NOT_PERMITTED)
    }

    /// The bytes free where images go: the data partition, or the UBI device.
    fn free(&self) -> std::io::Result<u64> {
        if let Some(free) = self.offline_free {
            return Ok(free);
        }
        match &self.layout.ubi {
            Some(device) => device.free(),
            None => lemnos_linux_sys::statfs::free_bytes(&self.layout.images()),
        }
    }

    fn room(&self, size: u64) -> Result<()> {
        fs::create_dir_all(self.layout.images()).map_err(|_| Status::IO)?;
        let free = self.free().map_err(|_| Status::IO)?;
        if free < size.saturating_add(self.reserve) {
            let mut state = self.state();
            state.error = match self
                .grow
                .as_ref()
                .filter(|grow| !grow.retry.last.is_empty())
            {
                // The grow has failed on record: the partition has not grown, so the room is the
                // partition's own, and the install needs the slot plus the board's reserve.
                Some(grow) => format!(
                    "the data partition has {} MiB free and has not grown ({}; partition 2 is {}); installs need the slot plus the board's reserve of {} MiB",
                    free >> 20,
                    grow.retry.last,
                    grow.partition_mib()
                        .map_or_else(|| "of unknown size".to_string(), |mib| format!("{mib} MiB")),
                    self.reserve >> 20
                ),
                None => format!(
                    "no room for this slot: it needs {} MiB with the reserve, the data partition has {} MiB free; remove a slot first",
                    size.saturating_add(self.reserve) >> 20,
                    free >> 20
                ),
            };
            let _ = self.save(&state);
            return Err(NO_SPACE);
        }
        Ok(())
    }

    /// Shows the install's phase on the board's indicator, when the board has one: `receiving`
    /// while bytes arrive (with their progress), then `verifying`, `done` or `failed` as the install
    /// reaches each. Only a change is sent, so a steady phase costs nothing.
    fn indicate(&mut self) {
        let Some(install) = self.install.as_ref() else {
            return;
        };
        let state = match &install.end {
            Some(Ok(_)) => ("done", NO_PROGRESS),
            Some(Err(_)) => ("failed", NO_PROGRESS),
            None if install.complete() => ("verifying", NO_PROGRESS),
            None => ("receiving", permille(install.got, install.size)),
        };
        self.show(state.0, state.1);
    }

    /// Sends `state` and its progress to the indicator, unless it is what was sent last. A refused
    /// indication changes no install: the next change is sent again.
    fn show(&mut self, state: &'static str, progress: u16) {
        if self.shown == Some((state, progress)) {
            return;
        }
        self.shown = Some((state, progress));
        if let Some(indicator) = self.indicator.as_mut() {
            let _ = indicator.set(state, progress);
        }
    }

    /// Starts an install of `size` bytes, written by its starter or read from `source`.
    fn begin(
        &mut self,
        size: u64,
        pin: bool,
        keep_old: bool,
        source: Option<install::Source>,
    ) -> Result<JobId> {
        if self.install.is_some() {
            return Err(Status::BUSY);
        }
        if security::enforced() && self.keys.is_empty() {
            self.refuse_attempt(size, attempt::NO_KEYS);
            return Err(Status::NOT_PERMITTED);
        }
        if let Err(status) = self.room(size) {
            let why = if status == NO_SPACE {
                self.state().error
            } else {
                "the data partition's free space could not be read".to_string()
            };
            self.refuse_attempt(size, &why);
            return Err(status);
        }
        let mut state = self.state();
        let guard = Guard {
            running: self.running(),
            default: state.default.clone(),
        };
        self.next_job += 1;
        let job = JobId(self.next_job);
        // The boot partition's declared firmware overlays and config digest, read before the bundle
        // comes: its boot files are checked against them as it is admitted. When they can't be read
        // (no boot partition there), the admission keeps the role-only set; the write then fails at
        // the boot partition with the boot partition's own reason.
        let (firmware, config) = self.record.declared().unwrap_or_default();
        let admit = Admit {
            keys: self.keys.clone(),
            require_signed: security::enforced(),
            board_id: self.board_id.clone(),
            booted_medium: self.booted_medium(),
            firmware,
            config,
        };
        let mut install = Install::start(
            &self.layout,
            self.calling,
            job,
            size,
            pin,
            keep_old,
            guard,
            admit,
            self.ubi_write.clone(),
        )
        .map_err(|failure| {
            self.refuse_attempt(size, &failure.1);
            failure.0
        })?;
        install.source = source;
        state.state = "staging".into();
        state.error.clear();
        state.progress = 0;
        state.started_by = "lemnos".into();
        self.save(&state)?;
        self.install = Some(install);
        self.start_attempt(size);
        self.show("receiving", 0);
        Ok(job)
    }

    /// The next piece of an install from a file: what a tick does.
    /// Reads a bundle from its file and takes it, a piece at a time until the tick's budget is spent.
    fn pull(&mut self, budget: &mut budget::Budget<'_>) {
        let Some(mut install) = self.install.take() else {
            return;
        };
        if install.end.is_none() && install.source.is_some() {
            let mut buf = core::mem::take(&mut self.read_buf);
            buf.resize(budget::PIECE, 0);
            loop {
                let want = budget.piece();
                match install.pull(&mut buf[..want]) {
                    Ok(read) => {
                        budget.spend(read);
                        let at = install.got;
                        let _ = self.feed(&mut install, at, &buf[..read]);
                        if read == 0 {
                            break;
                        }
                    }
                    Err(failure) => {
                        self.fail_install(&mut install, failure);
                        break;
                    }
                }
                if install.end.is_some()
                    || install.source.is_none()
                    || install.complete()
                    || budget.spent()
                {
                    break;
                }
            }
            self.read_buf = buf;
        }
        self.install = Some(install);
    }

    /// The clock a tick's install work is timed by: a test stands in a slow board with its own.
    #[doc(hidden)]
    pub fn set_clock(&mut self, clock: budget::Clock) {
        self.clock = clock;
    }

    /// Removes a slot's boot directory, image, manifest and state. The pointers first: the
    /// initramfs's state.json never names a slot that is going.
    fn remove_slot(&self, state: &mut State, id: &str) -> Result<()> {
        if state.previous == id {
            state.previous.clear();
        }
        if state.staged == id {
            state.staged.clear();
        }
        // The bay first, by its record: a bay a pointer names is refused, and then nothing has changed.
        self.record.free(id).map_err(|_| Status::IO)?;
        self.save(state)?;
        let io = |_| Status::IO;
        if let (Some(device), Some(writer)) = (&self.layout.ubi, &self.ubi_write) {
            let volume = ubi::image_volume(id);
            if device.volume(&volume).is_some() {
                writer.remove(&volume).map_err(io)?;
            }
        }
        for path in [
            self.layout.image(id),
            self.layout.manifest(id),
            self.layout.signature(id),
            self.layout.meta(id),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Status::IO),
            }
        }
        boot::drop_kept(&self.layout, id).map_err(io)?;
        store::sync_dir(&self.layout.images()).map_err(io)
    }

    /// Installed slots, newest first.
    fn by_age(&self) -> Vec<String> {
        let mut slots: Vec<(u64, String, String)> = self
            .layout
            .slots()
            .into_iter()
            .map(|id| {
                let meta = Meta::load(&self.layout, &id);
                (meta.seq(), meta.get("INSTALLED").to_string(), id)
            })
            .collect();
        slots.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        slots.into_iter().map(|(_, _, id)| id).collect()
    }

    /// The running, default, trial and staged slots stay and count, then the previous one, then
    /// the newest; past the number kept the rest go. Pinned slots don't count and never go. A
    /// failed slot is no fallback, so it doesn't take a good one's place.
    fn housekeep(&self, state: &mut State) -> Result<Vec<String>> {
        let keep = store::keep_count(&self.layout, self.keep);
        if keep == 0 {
            return Ok(Vec::new());
        }
        let running = self.running();
        let firm = [
            running.clone(),
            state.default.clone(),
            state.trial.clone(),
            state.staged.clone(),
        ];
        let mut order: Vec<String> = [
            running.clone(),
            state.default.clone(),
            state.trial.clone(),
            state.staged.clone(),
            state.previous.clone(),
        ]
        .into_iter()
        .filter(|id| !id.is_empty())
        .collect();
        order.extend(self.by_age());
        let (mut good, mut all) = (0u32, 0u32);
        let mut seen: Vec<String> = Vec::new();
        let mut gone = Vec::new();
        for id in order {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            if !self.layout.installed(&id) {
                continue;
            }
            let meta = Meta::load(&self.layout, &id);
            if meta.pinned() {
                continue;
            }
            if firm.contains(&id) {
                good += 1;
                all += 1;
                continue;
            }
            if meta.failed() {
                if all < keep {
                    all += 1;
                    continue;
                }
            } else if good < keep {
                good += 1;
                all += 1;
                continue;
            }
            self.remove_slot(state, &id)?;
            gone.push(id);
        }
        Ok(gone)
    }

    /// What `info` says: the policy, and the last install's signature result.
    fn info_json(&self) -> String {
        let (slot, word, signer, reason) = match &self.last_signed {
            None => (String::new(), String::new(), String::new(), String::new()),
            Some((slot, Ok(outcome))) => (
                slot.clone(),
                outcome.word().to_string(),
                outcome.signer().to_string(),
                outcome.reason(),
            ),
            Some((slot, Err(text))) => {
                (slot.clone(), "refused".into(), String::new(), text.clone())
            }
        };
        let mut out = format!(
            "{{\"require_signed\":{},\"last_install\":{{",
            security::enforced()
        );
        for (key, value) in [
            ("slot", &slot),
            ("signature", &word),
            ("signer", &signer),
            ("reason", &reason),
        ] {
            if out.ends_with('{') {
                out.push('"');
            } else {
                out.push_str(",\"");
            }
            out.push_str(key);
            out.push_str("\":");
            quoted(value, &mut out);
        }
        out.push('}');
        out.push_str(&format!(
            ",\"default_signature\":\"{}\",\"boot_unverified\":",
            signature_word(&Meta::load(&self.layout, &self.state().default))
        ));
        quoted(&signing::boot_unverified(&self.layout), &mut out);
        // The default's build and medium as its slot.json says them (`not recorded` when it says
        // neither), and the medium the board booted from (`unknown` when it can't be told).
        let default = self.state().default;
        for (key, value) in [
            ("default_build", self.recorded(&default, "build.mode")),
            ("default_medium", self.recorded(&default, "medium")),
            (
                "booted_medium",
                match self.booted_medium() {
                    medium if medium.is_empty() => "unknown".to_string(),
                    medium => medium,
                },
            ),
        ] {
            out.push_str(&format!(",\"{key}\":"));
            quoted(&value, &mut out);
        }
        // Appended last: the ids of the keys the board holds, so a client can choose the key it
        // signs with without learning the keys. Each id is the SHA-256 of the public key.
        out.push_str(",\"key_ids\":[");
        for (at, key) in self.keys.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            out.push_str("{\"id\":\"");
            out.push_str(&boot::hex(&signing::key_id(key.key.as_bytes())));
            out.push_str("\",\"owner\":");
            quoted(&key.owner, &mut out);
            out.push('}');
        }
        out.push(']');
        // Appended after `key_ids`: how the last install attempt stands or ended.
        out.push_str(",\"last_attempt\":");
        let mut shown = self.attempt;
        if let (Some(attempt), Some(install)) = (shown.as_mut(), self.install.as_ref())
            && attempt.ended == Ended::Running
        {
            // While it runs, the numbers are the job's own: `list` and `describe` show the same.
            attempt.received = Some(install.got);
            attempt.total = Some(install.size);
            attempt.slot.set(install.slot_id().unwrap_or(""));
        }
        Attempt::json(shown.as_ref(), &self.boot, &mut out);
        // Appended last: the boot count's words while the firmware refuses to clear it (empty
        // otherwise). The retry's full text, with its wait.
        out.push_str(",\"boot_count\":");
        quoted(
            &self
                .uncleared
                .as_ref()
                .map(count::Uncleared::text)
                .unwrap_or_default(),
            &mut out,
        );
        // Appended last, and only for a board that declares a crash log: what the boot's record step said.
        if self.crash.is_some() {
            out.push_str(",\"crash_log\":");
            quoted(&self.crash_note, &mut out);
        }
        // Appended last: whether trial updates are available on this board. When not, the reason's
        // fixed code, its sentence and what the driver found for it.
        match &self.trial_updates {
            TrialUpdates::Available => out.push_str(",\"trial_updates\":\"available\",\"trial_block\":\"\",\"trial_block_text\":\"\",\"trial_block_detail\":\"\""),
            TrialUpdates::Unavailable(why) => {
                out.push_str(",\"trial_updates\":\"unavailable\",\"trial_block\":");
                quoted(why.reason.code(), &mut out);
                out.push_str(",\"trial_block_text\":");
                quoted(why.reason.sentence(), &mut out);
                out.push_str(",\"trial_block_detail\":");
                quoted(&why.detail, &mut out);
            }
        }
        // Appended last: the data partition's grow while it is not finished. `grow_state` is `pending`,
        // `grown-unconfirmed` (the kernel accepted the resize and its view has not shown it after the
        // bound, with the sizes seen in bytes), or `none` (finished, or `grow_data` is false).
        grow_info(self.grow.as_ref(), &mut out);
        // Appended last: why the slots state is left untouched, `""` when the boot records are ours.
        out.push_str(",\"foreign\":");
        quoted(&self.foreign_text(), &mut out);
        out.push_str(",\"state_unwritable\":");
        quoted(self.state_unwritable.as_deref().unwrap_or(""), &mut out);
        out.push_str(",\"write_failed\":");
        quoted(&self.write_failed().unwrap_or_default(), &mut out);
        out.push('}');
        out
    }

    /// Reports, at start and in `info`, a default slot whose signature did not verify while
    /// signing is required: the state's `error` names it and what to install.
    fn report_policy(&mut self) {
        if !security::enforced() {
            return;
        }
        let state = self.state();
        if state.default.is_empty() {
            return;
        }
        if signature_word(&Meta::load(&self.layout, &state.default)) == signing::VERIFIED {
            return;
        }
        let mut state = state;
        state.error = format!(
            "signing is required, and the default slot {} is not signed by a key this board trusts: install a verified slot and set it as the default",
            state.default
        );
        let _ = self.save(&state);
    }

    fn list_json(&self) -> String {
        let state = self.state();
        let running = self.running();
        let mut out = self.list_head(&state, &running);
        for (at, id) in self.by_age().iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            out.push_str(&self.slot_json(id, &running, &state));
        }
        out.push_str("],");
        out.push_str(&self.list_rest(&state));
        out
    }

    /// `list_page`: the slots after the cursor `after`, as many as fit in `cap` bytes. `-` is the
    /// first page; any other cursor is `<seq>:<id>`, the last slot of the page before. Slots are
    /// taken in the order of their install number, newest first, ties by id; a page is the slots
    /// whose key comes after the cursor's, so no slot is repeated, and a slot installed between
    /// two pages has a larger number and is never in a later one.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a cursor that is not one, `BUFFER_TOO_SMALL` when the page's
    /// fixed fields and one slot do not fit in `cap`.
    fn list_page_json(&self, after: &str, cap: usize) -> Result<String> {
        let cursor = match after {
            "-" => None,
            _ => {
                let (seq, id) = after.split_once(':').ok_or(Status::INVALID_ARGUMENT)?;
                let seq = seq.parse::<u64>().map_err(|_| Status::INVALID_ARGUMENT)?;
                Some((seq, id))
            }
        };
        let state = self.state();
        let running = self.running();
        let pending: Vec<(u64, String)> = self
            .by_seq()
            .into_iter()
            .filter(|(seq, id)| match cursor {
                None => true,
                Some((at, last)) => *seq < at || (*seq == at && id.as_str() < last),
            })
            .collect();
        let head = self.list_head(&state, &running);
        let rest = self.list_rest(&state);
        let tail = |more: bool, next: &str| format!("],\"more\":{more},\"next\":{next},{rest}");
        let mut body = String::new();
        let mut last = String::new();
        let mut count = 0usize;
        for (seq, id) in &pending {
            let mut key = String::new();
            quoted(&format!("{seq}:{id}"), &mut key);
            let entry = self.slot_json(id, &running, &state);
            let sep = if count == 0 { "" } else { "," };
            // Sized for the longest tail this entry could leave (`more` false).
            let need = head.len() + body.len() + sep.len() + entry.len() + tail(false, &key).len();
            if need > cap {
                break;
            }
            body.push_str(sep);
            body.push_str(&entry);
            count += 1;
            last = key;
        }
        if count == 0 && (!pending.is_empty() || head.len() + tail(false, "\"\"").len() > cap) {
            return Err(Status::BUFFER_TOO_SMALL);
        }
        let more = count < pending.len();
        let next = if more { last } else { "\"\"".to_string() };
        Ok(format!("{head}{body}{}", tail(more, &next)))
    }

    /// The fields of `list` before its slots: the pointers, the free space and the settings. It
    /// ends where the slots' array begins.
    fn list_head(&self, state: &State, running: &str) -> String {
        let mut out = String::from("{");
        for (key, value) in [
            ("slot_active", running),
            ("slot_default", state.default.as_str()),
            ("slot_previous", state.previous.as_str()),
            ("slot_trial", state.trial.as_str()),
            ("slot_staged", state.staged.as_str()),
            ("state", state.state.as_str()),
            ("error", state.error.as_str()),
            ("config_refusal", self.config_refusal.unwrap_or("")),
            ("foreign", &self.foreign_text()),
            (
                "state_unwritable",
                self.state_unwritable.as_deref().unwrap_or(""),
            ),
            ("write_failed", &self.write_failed().unwrap_or_default()),
        ] {
            quoted(key, &mut out);
            out.push(':');
            quoted(value, &mut out);
            out.push(',');
        }
        let free = self.free().unwrap_or(0);
        out.push_str(&format!(
            "\"free_bytes\":{free},\"reserve_bytes\":{},\"keep\":{},\"slots\":[",
            self.reserve,
            store::keep_count(&self.layout, self.keep)
        ));
        out
    }

    /// The fields of `list` after its slots: the trial and the resolved boot, and the object's end.
    fn list_rest(&self, state: &State) -> String {
        let mut out = String::from("\"trial\":");
        out.push_str(&self.trial_json(state));
        out.push_str(",\"resolved\":");
        match Resolved::load(&self.layout) {
            Some(resolved) => out.push_str(&resolved.json()),
            None => out.push_str("null"),
        }
        out.push('}');
        out
    }

    /// One slot as `list` shows it: an object.
    fn slot_json(&self, id: &str, running: &str, state: &State) -> String {
        let manifest = fs::read_to_string(self.layout.manifest(id))
            .ok()
            .and_then(|text| json::Json::parse(&text));
        let text = |key: &str| {
            manifest
                .as_ref()
                .and_then(|m| m.path(key))
                .and_then(json::Json::text)
                .unwrap_or("")
                .to_string()
        };
        let meta = Meta::load(&self.layout, id);
        let size = self.layout.image_size(id).unwrap_or(0);
        let want = manifest
            .as_ref()
            .and_then(|m| m.path("image.size"))
            .and_then(json::Json::u64);
        let mut flags = Vec::new();
        for (flag, slot) in [
            ("running", running),
            ("default", state.default.as_str()),
            ("previous", state.previous.as_str()),
            ("trial", state.trial.as_str()),
            ("staged", state.staged.as_str()),
        ] {
            if slot == id {
                flags.push(format!("\"{flag}\""));
            }
        }
        let mut out = String::from("{");
        for (key, value) in [
            ("id", id.to_string()),
            ("product", text("product")),
            ("board", text("board")),
            ("version", text("version")),
            ("variant", text("variant")),
            ("kernel", text("kernel")),
            ("display_name", text("display_name")),
            ("good", meta.get("GOOD").to_string()),
            (
                "signed",
                signing::legacy_label(signature_word(&meta)).to_string(),
            ),
            ("signer", meta.get("SIGNER").to_string()),
            ("installed", meta.get("INSTALLED").to_string()),
            ("last_boot", meta.get("LAST_BOOT").to_string()),
        ] {
            quoted(key, &mut out);
            out.push(':');
            quoted(&value, &mut out);
            out.push(',');
        }
        out.push_str(&format!(
            "\"size\":{size},\"verity\":{},\"intact\":{},\"flags\":[{}],\"pinned\":{},\"failures\":{},\"failed\":{},\"signature\":\"{}\",",
            !text("verity.root_hash").is_empty(),
            want == Some(size),
            flags.join(","),
            meta.pinned(),
            meta.get("FAILURES").parse::<u32>().unwrap_or(0),
            meta.failed(),
            signature_word(&meta),
        ));
        // Beside the signature: how the builder made the slot (`clean`, `dev-reuse`, or the text a
        // builder wrote) and the medium its root is for. `not recorded` when the slot says neither.
        for (key, path) in [("build", "build.mode"), ("medium", "medium")] {
            let value = text(path);
            out.push_str(&format!("\"{key}\":"));
            quoted(
                if value.is_empty() {
                    "not recorded"
                } else {
                    &value
                },
                &mut out,
            );
            out.push(',');
        }
        // Whether the bundle's slot.sig is kept: `kept`, `unsigned`, or the sentence of a signed slot
        // installed before signatures were kept.
        out.push_str("\"slot_sig\":");
        quoted(
            &match kept_signature(&self.layout, id) {
                Ok(KeptSignature::Kept(_)) => "kept".to_string(),
                Ok(KeptSignature::Unsigned) => "unsigned".to_string(),
                Ok(KeptSignature::NotKept(sentence)) => sentence,
                Err(error) => format!("slot.sig could not be read: {error}"),
            },
            &mut out,
        );
        out.push('}');
        out
    }

    /// Every slot by its install number, newest first, ties by id: the order `list_page` pages.
    fn by_seq(&self) -> Vec<(u64, String)> {
        let mut slots: Vec<(u64, String)> = self
            .layout
            .slots()
            .into_iter()
            .map(|id| (Meta::load(&self.layout, &id).seq(), id))
            .collect();
        slots.sort_by(|a, b| b.cmp(a));
        slots
    }

    /// The `trial` field of `list`: null unless a trial is pending across boots.
    fn trial_json(&self, state: &State) -> String {
        if state.trial.is_empty() {
            return "null".to_string();
        }
        let left = match self.record.pending() {
            Ok(Some((id, left))) if id == state.trial => left,
            _ => 0,
        };
        let record_error = self.record_error.as_deref();
        match Log::load(&self.layout).filter(|log| log.slot == state.trial) {
            Some(log) => log.json(left, record_error),
            None => Log {
                slot: state.trial.clone(),
                seen: left,
                ..Log::default()
            }
            .json(left, record_error),
        }
    }
}

/// The settings every `system-slots` board file may give, as the driver reads them.
const KEYS: &[Key] = &[
    Key::optional("data", Kind::Str),
    Key::optional("boot", Kind::Str),
    Key::optional("ubi", Kind::Str),
    Key::optional("record", Kind::Str),
    Key::optional("restart_socket", Kind::Str),
    Key::optional("keys", Kind::Str),
    Key::optional("partuuid", Kind::Str),
    Key::optional("running", Kind::Str),
    Key::optional("boot_args", Kind::Str),
    Key::optional("require_signed", Kind::Bool),
    // The offline writer's policy (`lemnos-ctl data-partition`): "required" (the default) or "not-required".
    Key::optional("signature", Kind::Str),
    Key::optional("reserve_mb", Kind::Int),
    Key::optional("keep", Kind::Int),
    Key::optional("boots_keep", Kind::Int),
    Key::optional("crash_log", Kind::Str),
    Key::optional("crash_bytes", Kind::Int),
    Key::optional("reset_reason", Kind::Str),
    Key::optional("auto_confirm", Kind::Bool),
    Key::optional("reboot_at_deadline", Kind::Bool),
    Key::optional("confirm_uptime_s", Kind::Int),
    Key::optional("confirm_deadline_s", Kind::Int),
    Key::optional("trial_attempts", Kind::Int),
    Key::optional("health", Kind::List),
    Key::optional("boot_sources", Kind::List),
    Key::optional("device_tree", Kind::Str),
    Key::optional("vcio", Kind::Str),
    Key::optional("demote_after_episodes", Kind::Int),
    Key::optional("uboot_env_size", Kind::Int),
    Key::optional("uboot_env_device", Kind::Str),
    Key::optional("uboot_env_offset", Kind::Int),
    Key::optional("uboot_env_offset_redund", Kind::Int),
    // Read by `lemnos-ctl tree --uboot-config` and `--boot-dir` for the image's U-Boot, not by the
    // driver: the MMC index and the fastboot USB controller (U-Boot's numbering), the download-mode
    // LED's label, the fastboot gadget's VID and PID, and the watchdog's timeout in ms.
    Key::optional("uboot_env_mmc_index", Kind::Int),
    Key::optional("uboot_fastboot_usb", Kind::Int),
    Key::optional("uboot_led", Kind::Str),
    Key::optional("uboot_reset_word", Kind::Str),
    Key::optional("uboot_usb_vendor", Kind::Int),
    Key::optional("uboot_usb_product", Kind::Int),
    Key::optional("uboot_watchdog_ms", Kind::Int),
    // Listed only so that giving it is refused by name (see `uboot_env_place`).
    Key::optional("uboot_env_partuuid", Kind::Str),
    // The good-boot deadline in seconds: read by `lemnos-ctl tree --good-boot-unit` (the unit's drop-in) and
    // checked here, so a bad value is refused at the driver too (see `lemnos_slot_store::goodboot`).
    Key::optional("good_boot_s", Kind::Int),
    // Grow the data partition to the disk's end at each start until it is done, on unless `false` (see [`grow`]).
    Key::optional("grow_data", Kind::Bool),
];

/// The settings `SystemReboot` reads. The boot-file check before its restart reads the boot files
/// raw from the boot disk, so it takes the same boot facts as the slot driver, and clears the Pi
/// firmware's boot count through the same mailbox.
const REBOOT_KEYS: &[Key] = &[
    Key::optional("restart_socket", Kind::Str),
    // The board file's way back (`[boot] loop_return`), given by the host, never named in the device's own config.
    Key::optional("loop_return", Kind::Str),
    Key::optional("data", Kind::Str),
    Key::optional("boot", Kind::Str),
    Key::optional("ubi", Kind::Str),
    Key::optional("record", Kind::Str),
    Key::optional("boot_sources", Kind::List),
    Key::optional("device_tree", Kind::Str),
    Key::optional("vcio", Kind::Str),
];

/// The `record` setting: which boot record the board uses. `ubi` needs a `ubi` device.
fn record_kind<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
    match cfg.str("record").unwrap_or("tryboot") {
        "tryboot" | "file" | "uboot" => Ok(()),
        "ubi" if cfg.str("ubi").is_some() => Ok(()),
        "ubi" => Err(ConfigError::new("ubi", "is required for record = ubi")),
        _ => Err(ConfigError::new(
            "record",
            "must be tryboot, file, uboot or ubi",
        )),
    }
}

/// The `health` list, each entry a source the trial waits on.
fn health_sources(texts: &[String]) -> core::result::Result<Vec<Source>, ConfigError<'static>> {
    texts
        .iter()
        .map(|text| {
            Source::parse(text).map_err(|refused| ConfigError::new("health", refused.reason))
        })
        .collect()
}

/// The trial settings the board file gives the `system-slots` driver.
fn trial_policy<'a>(cfg: &Config<'a>) -> core::result::Result<Policy, ConfigError<'a>> {
    let mut policy = Policy::default();
    if let Some(on) = cfg.bool("auto_confirm") {
        policy.auto_confirm = on;
    }
    if let Some(on) = cfg.bool("reboot_at_deadline") {
        policy.reboot_at_deadline = on;
    }
    let seconds = |key: &'static str| -> core::result::Result<Option<Duration>, ConfigError<'a>> {
        match cfg.int(key) {
            None => Ok(None),
            Some(s) => u64::try_from(s)
                .map(|s| Some(Duration::from_secs(s)))
                .map_err(|_| ConfigError::new(key, "must not be negative")),
        }
    };
    if let Some(uptime) = seconds("confirm_uptime_s")? {
        policy.uptime = uptime;
    }
    if let Some(deadline) = seconds("confirm_deadline_s")? {
        policy.deadline = deadline;
    }
    if policy.deadline < policy.uptime {
        return Err(ConfigError::new(
            "confirm_deadline_s",
            "must not be less than confirm_uptime_s",
        ));
    }
    if let Some(attempts) = cfg.int("trial_attempts") {
        policy.attempts = u8::try_from(attempts)
            .ok()
            .filter(|n| (1..=lemnos_slot_store::records::MAX_ATTEMPTS).contains(n))
            .ok_or(ConfigError::new("trial_attempts", "must be 1 to 3"))?;
    }
    policy.health = cfg
        .list("health")
        .unwrap_or(&[])
        .iter()
        .map(|item| item.to_string())
        .collect();
    Ok(policy)
}

/// This boot's id, from the kernel. Where the kernel doesn't say, a value unique to this start:
/// then a boot may give its attempt back more than once, never fewer than once.
fn read_boot_id() -> String {
    match fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => format!(
            "start-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ),
    }
}

/// How a slot was signed, as `list` shows it. A slot from before the board recorded it is
/// `unsigned`.
/// `done` of `total` in per mille, at most 1000; the whole of it when `total` is 0.
fn permille(done: u64, total: u64) -> u16 {
    if total == 0 {
        return 1000;
    }
    (done.saturating_mul(1000) / total).min(1000) as u16
}

/// A slot's signature word: `verified`, `not_checked_unknown_key` or `not_checked_unsigned`.
fn signature_word(meta: &Meta) -> &'static str {
    signing::recorded(meta.get("SIGNATURE"), meta.get("SIGNED"))
}

/// What the board kept of an installed slot's `slot.sig` (`<data>/slots/<id>.sig`).
#[derive(Debug, PartialEq, Eq)]
pub enum KeptSignature {
    /// The record as the bundle carried it: with `<id>.json` it rebuilds the bundle's signed part.
    Kept(Vec<u8>),
    /// The slot was installed from an unsigned bundle: there is nothing to keep.
    Unsigned,
    /// A signed slot with no record on the board: it was installed before signatures were kept. The
    /// sentence says so and what to do; it is never taken as unsigned or as forged.
    NotKept(String),
}

/// The slot's kept `slot.sig`, read from `layout` for slot `id` (`docs/slots.md`, "The kept signature").
pub fn kept_signature(layout: &Layout, id: &str) -> std::io::Result<KeptSignature> {
    match fs::read(layout.signature(id)) {
        Ok(record) => Ok(KeptSignature::Kept(record)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if signature_word(&Meta::load(layout, id)) == signing::NOT_CHECKED_UNSIGNED {
                Ok(KeptSignature::Unsigned)
            } else {
                Ok(KeptSignature::NotKept(format!(
                    "slot {id} was installed before signatures were kept: no slot.sig on the board; install it again from a signed bundle, or sign it"
                )))
            }
        }
        Err(error) => Err(error),
    }
}

/// The data partition's PARTUUID: the device mounted at `data`, found among
/// `/dev/disk/by-partuuid`.
fn find_partuuid(data: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let data = data.to_str()?;
    // Fields: id parent major:minor root mount-point ...; the last mount there wins.
    let device = mounts
        .lines()
        .rev()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            (fields.get(4) == Some(&data)).then(|| fields.get(2).map(|d| d.to_string()))?
        })
        .next()?;
    let (major, minor) = device.split_once(':')?;
    let (major, minor): (u64, u64) = (major.parse().ok()?, minor.parse().ok()?);
    for entry in fs::read_dir("/dev/disk/by-partuuid").ok()?.flatten() {
        let Ok(meta) = fs::metadata(entry.path()) else {
            continue;
        };
        let rdev = meta.rdev();
        // The kernel's device number split, as glibc's major() and minor() do it.
        let dev_major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
        let dev_minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
        if dev_major == major && dev_minor == minor {
            return entry.file_name().into_string().ok();
        }
    }
    None
}

impl Lifecycle for SystemSlots {
    type Needs = SystemSlotsNeeds;

    fn refusal(&mut self, _status: Status, words: &mut Reply<'_>) {
        if let Some(why) = self.why.take() {
            lemnos_core::say(words, &why);
        }
    }
    const TRACKS_CALLERS: bool = true;
    const JOBS: bool = true;
    const TICKS: bool = true;
    const WATCHES_BOARD: bool = true;

    fn board(&mut self, view: &dyn BoardView) {
        self.watch(view);
    }

    /// Down while the firmware refuses to clear the boot count: the words name the wait, and the
    /// driver keeps retrying (see [`count::Uncleared`]). Degraded, not failed: restarts still work.
    fn health(&self) -> Health {
        // Before all else: while the boot records are another system's, nothing here is written.
        match self.owner {
            Ownership::Ours => {}
            Ownership::NotOurs(_) => return Health::Degraded(FOREIGN),
            Ownership::Unknown(_) => return Health::Degraded(OWNER_UNKNOWN),
        }
        if self.state_unwritable.is_some() {
            return Health::Degraded(STATE_UNWRITABLE);
        }
        if self.write_failed().is_some() {
            return Health::Degraded(WRITE_FAILED);
        }
        // The first word: a config.txt that cannot select the default is the one thing the boot needs.
        if let Some(why) = self.config_refusal {
            return Health::Degraded(why);
        }
        if let Some(pending) = &self.uncleared {
            return Health::Degraded(count::health_text(pending.wait_ns));
        }
        if self
            .grow
            .as_ref()
            .and_then(|grow| grow.progress())
            .is_some_and(|progress| progress.overrun)
        {
            // The elapsed time and the bound are in `info` (grow_elapsed_ms, grow_bound_ms).
            return Health::Degraded(
                "resize-overrun: the data partition's resize has run past its bound; it is not abandoned and its result is taken when it arrives",
            );
        }
        if let Some(unconfirmed) = self.grow.as_ref().and_then(|grow| grow.unconfirmed()) {
            // The words and the sizes are in `info` (grow_reason, grow_baseline_bytes, grow_seen_bytes).
            let _ = unconfirmed;
            return Health::Degraded(
                "grown-unconfirmed: the kernel accepted the data partition's grow but its view has not confirmed it; the grow is retried",
            );
        }
        match &self.trial_updates {
            TrialUpdates::Available => Health::Ok,
            TrialUpdates::Unavailable(why) => Health::Degraded(why.reason.sentence()),
        }
    }

    /// The refused clear's attempts so far, when the next one is due, and why it was refused.
    fn retrying(&self) -> Option<Retry> {
        self.uncleared.as_ref().map(|pending| Retry {
            attempts: pending.failed,
            next_ns: pending.due_ns.unwrap_or(0),
            reason: count::RETRY_REASON,
        })
    }

    /// An install from a file reads a piece each tick, at once again while there is more. A trial
    /// that waits is looked at again each second, until it is confirmed or falls back. The
    /// indicator is brought up to date on each tick too.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if !self.ours() {
            // Another system's state, or one that can't be read yet: nothing is written. An unknown
            // answer is looked at again at the failure rule's cap.
            self.indicate();
            if !matches!(self.owner, Ownership::Unknown(_)) {
                return None;
            }
            // Records that can't be read to say whose they are: the board still came up. Once the
            // daemon has run its uptime, the firmware's count is cleared (the mailbox only, nothing
            // on the boot partition), so a board that only can't judge its records never counts a
            // run of restarts toward the initramfs's way back.
            let started = *self.started_ns.get_or_insert(now_ns);
            if !self.unjudged_cleared
                && Duration::from_nanos(now_ns.saturating_sub(started)) >= self.policy.uptime
                && let Some(firmware) = self.firmware.clone()
            {
                let done = count::clear_unjudged(&firmware);
                self.unjudged_cleared = done.is_ok();
                self.unjudged_note = Some(done.unwrap_or_else(|why| why));
            }
            return Some(now_ns.saturating_add(OWNER_RETRY_NS));
        }
        if self.state_unwritable.is_some() && now_ns >= self.state_check_ns {
            self.state_unwritable = state_dirs_writable(&self.layout).err();
            self.state_check_ns = now_ns.saturating_add(OWNER_RETRY_NS);
        }
        let clock = self.clock.clone();
        let mut budget =
            budget::Budget::new(&*clock, self.tick_limit_ns, install::READ_EVERY_TICK as u64);
        self.pull(&mut budget);
        self.finish_step(&mut budget);
        self.indicate();
        let install = self
            .install
            .as_ref()
            .filter(|install| {
                install.end.is_none() && (install.source.is_some() || install.finishing())
            })
            .map(|_| now_ns);
        let settle = self.settle(now_ns);
        let slow = self.slow_check(now_ns);
        let count = self.count_tick(now_ns);
        let grow = self.grow_tick(now_ns);
        let trial = self.trial_watch(now_ns);
        let restart = self.restart_watch(now_ns);
        [install, settle, slow, count, grow, trial, restart]
            .into_iter()
            .flatten()
            .min()
    }

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        record_kind(cfg)?;
        let policy = trial_policy(cfg)?;
        health_sources(&policy.health)?;
        if let Some(mb) = cfg.int("reserve_mb") {
            u64::try_from(mb)
                .map_err(|_| ConfigError::new("reserve_mb", "must not be negative"))?;
        }
        if let Some(keep) = cfg.int("keep") {
            u32::try_from(keep).map_err(|_| ConfigError::new("keep", "must be 0 to 4294967295"))?;
        }
        if let Some(keep) = cfg.int("boots_keep") {
            usize::try_from(keep)
                .map_err(|_| ConfigError::new("boots_keep", "must not be negative"))?;
        }
        if let Some(dir) = cfg.str("crash_log")
            && !dir.starts_with('/')
        {
            return Err(ConfigError::new("crash_log", "must be an absolute path"));
        }
        if let Some(bytes) = cfg.int("crash_bytes")
            && !usize::try_from(bytes).is_ok_and(|bytes| (1..=crash::MAX_BYTES).contains(&bytes))
        {
            return Err(ConfigError::new("crash_bytes", "must be from 1 to 65536"));
        }
        boot_table(cfg)?;
        lemnos_slot_store::goodboot::deadline_from_board(cfg.int(lemnos_slot_store::goodboot::KEY))
            .map_err(|_| {
                ConfigError::new(
                    lemnos_slot_store::goodboot::KEY,
                    lemnos_slot_store::goodboot::REASON,
                )
            })?;
        if cfg.str("record") == Some("uboot") {
            uboot_env_place(cfg)?;
            if cfg
                .int("uboot_env_mmc_index")
                .is_some_and(|n| n > uboot::MAX_MMC_INDEX)
            {
                return Err(ConfigError::new(
                    "uboot_env_mmc_index",
                    "is at most 99: the image writes it as lemnos_envdev, which the budget allows two digits for",
                ));
            }
            if cfg
                .int("uboot_fastboot_usb")
                .is_some_and(|n| n > uboot::MAX_USB_INDEX)
            {
                return Err(ConfigError::new(
                    "uboot_fastboot_usb",
                    "is at most 99: the image stores it as lemnos_usb, which the environment's budget allows two digits for",
                ));
            }
            if cfg
                .str("uboot_led")
                .is_some_and(|label| label.len() > uboot::MAX_LED_BYTES)
            {
                return Err(ConfigError::new(
                    "uboot_led",
                    "is at most 32 bytes: the image stores it as lemnos_led, which the environment's budget allows for",
                ));
            }
            if cfg
                .str("uboot_reset_word")
                .is_some_and(|word| word.len() > uboot::MAX_RESET_BYTES)
            {
                return Err(ConfigError::new(
                    "uboot_reset_word",
                    "is 0x and at most eight hex digits: the image stores it as lemnos_rst, which the environment's budget allows for",
                ));
            }
        }
        for key in [
            "uboot_env_mmc_index",
            "uboot_fastboot_usb",
            "uboot_usb_vendor",
            "uboot_usb_product",
            "uboot_watchdog_ms",
        ] {
            if cfg.int(key).is_some_and(|n| n < 0) {
                return Err(ConfigError::new(key, "must not be negative"));
            }
        }
        Ok(())
    }

    fn start(needs: SystemSlotsNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (layout, ubi_write) = layout_of(cfg);
        // Two records: the check holds one (to point the default at the previous slot), and the
        // driver's own is built with the check in its restarter, so every restart is checked.
        let node = boot_node_of(cfg);
        let firmware = firmware_of(cfg);
        let base = record_for(cfg, &layout, &ubi_write, None)?;
        let check = guard_check(layout.clone(), node.clone(), base, firmware.clone());
        let look = guard_look(
            layout.clone(),
            node.clone(),
            record_for(cfg, &layout, &ubi_write, None)?,
        );
        let restarter = restarter(cfg, Some(check.clone()));
        let record = record_with(cfg, &layout, &ubi_write, restarter.clone())?;
        // A key file that can't be read leaves the board trusting no bundle, which every
        // install then says; it is read again when the driver is bound again.
        let keys = cfg
            .str("keys")
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| parse_keys(&text).ok())
            .unwrap_or_default();
        let trial_updates = record.trial_updates();
        let mut slots = Self::with_volumes(layout, record, keys, ubi_write);
        slots.trial_updates = trial_updates;
        slots.check = Some(check);
        slots.look = Some(look);
        slots.restarter = Some(restarter);
        slots.boot_node = node;
        slots.firmware = firmware;
        slots.demote_after =
            lemnos_slot_store::fallback::threshold_from_board(cfg.int("demote_after_episodes"))
                .map_err(|_| Status::INVALID_ARGUMENT)?;
        slots.policy = trial_policy(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        slots.sources =
            health_sources(&slots.policy.health).map_err(|_| Status::INVALID_ARGUMENT)?;
        slots.probes = slots
            .sources
            .iter()
            .map(|_| trial::Probe::default())
            .collect();
        slots.boot = read_boot_id();
        slots.indicator = needs.indicator;
        slots.partuuid = cfg
            .str("partuuid")
            .map(str::to_string)
            .or_else(|| find_partuuid(&slots.layout.data))
            .unwrap_or_default();
        slots.running = cfg.str("running").map(str::to_string);
        slots.boot_args = cfg.str("boot_args").unwrap_or("").to_string();
        slots.boot_sources = cfg
            .list("boot_sources")
            .unwrap_or(&[])
            .iter()
            .map(|entry| entry.to_string())
            .collect();
        slots.board_id = cfg.board().to_string();
        if cfg.get("require_signed").is_some() || cfg.get("security_view").is_some() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let ours = slots.ours();
        if ours {
            slots.report_policy();
        }
        if let Some(mb) = cfg.int("reserve_mb") {
            slots.reserve = u64::try_from(mb).map_err(|_| Status::INVALID_ARGUMENT)? << 20;
        }
        if let Some(keep) = cfg.int("keep") {
            slots.keep = u32::try_from(keep).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
        // The attempts a fallback trial gets are the board's, so the default slot's loader can
        // take them from its own settings.
        if ours {
            let _ = store::write_setting(
                &slots.layout,
                "TRIAL_ATTEMPTS",
                &slots.policy.attempts.to_string(),
            );
            slots.resolve_at_start();
        }
        if let Some(keep) = cfg.int("boots_keep") {
            slots.boots_keep = usize::try_from(keep).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
        slots.reset_reason = cfg.str("reset_reason").map(PathBuf::from);
        slots.tick_limit_ns = budget::limit_ns(cfg.call_deadline_ms());
        if let Some(dir) = cfg.str("crash_log") {
            slots.crash = Some(Box::new(crash::FsPstore::new(dir)));
        }
        if let Some(bytes) = cfg.int("crash_bytes") {
            slots.crash_bytes = usize::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
        if grow::enabled(cfg.bool("grow_data")) {
            slots.grow = Some(grow::Task::new(
                boot_disk_of(cfg),
                slots.layout.data.clone(),
            ));
        }
        if ours {
            slots.record_boot();
            slots.arrive_at_start();
        }
        Ok(slots)
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    /// The host cancels the jobs a gone caller started (`job_cancel`) and then tells the driver
    /// it went: a cancel that was the starter's own connection ending says so.
    fn caller_gone(&mut self, caller: CallerId) {
        if let Some(attempt) = self.attempt.as_mut()
            && attempt.ended == Ended::Cancelled
            && attempt.owner == caller
            && attempt.reason.as_str() == attempt::CANCELLED
        {
            attempt.reason.set(attempt::GONE);
        }
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(install) = self.install.as_ref().filter(|i| i.job == job) else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        let state = match &install.end {
            None => {
                let (done, total) = install.progress();
                return JobState::Running { done, total };
            }
            Some(Ok(id)) => JobState::Finished(match result.write(id.as_bytes()) {
                Ok(()) => Status::OK,
                Err(status) => status,
            }),
            // The failure's cause in words: the same reason `info`'s last attempt keeps.
            Some(Err((status, words))) => {
                lemnos_core::say(result, words);
                JobState::Finished(*status)
            }
        };
        self.install = None;
        state
    }

    fn job_cancel(&mut self, job: JobId) {
        if let Some(mut install) = self.install.take_if(|i| i.job == job && i.end.is_none()) {
            self.unwind(&install);
            install.discard();
            install.end = Some(Err((Status::CANCELLED, String::new())));
            self.end_attempt(Ended::Cancelled, attempt::CANCELLED, Some(&install));
            self.install = Some(install);
            let mut state = State::load(&self.layout);
            state.state = "cancelled".into();
            state.error.clear();
            state.progress = 0;
            let _ = state.save(&self.layout);
        }
    }
}

/// What stops the security switch changing (`docs/slots.md`, "The security switch"): an install or a
/// trial in progress, in either direction; and to on, a trust root with the default and the
/// running slot verified under it.
impl SecurityPrecondition for SystemSlots {
    fn apply(&mut self, on: bool) -> Result<()> {
        self.writable()?;
        // The boot side's mark: written durably (the marker is a file and its directory is synced).
        signing::set_security(&self.layout, on).map_err(|_| Status::IO)
    }

    fn precondition(&mut self, on: bool, into: &mut [u8]) -> Result<usize> {
        let mut lines = Vec::new();
        if !self.ours() {
            lines.push(format!("busy: {}", self.foreign_text()));
        }
        let state = self.state();
        if self.install.is_some() {
            lines.push("busy: an install is running".to_string());
        }
        if valid_id(&state.trial) {
            lines.push(format!(
                "busy: slot '{}' is on trial: confirm it, or let it resolve",
                state.trial
            ));
        }
        if on {
            if self.keys.is_empty() {
                lines.push("missing: no trust root: the keys list (`keys` in the board file's [security] table) has no valid line".to_string());
            }
            for (role, id) in [
                ("default", state.default.clone()),
                ("running", self.running()),
            ] {
                if !valid_id(&id) {
                    lines.push(format!("missing: no {role} slot is set"));
                } else if Meta::load(&self.layout, &id).get("SIGNATURE") != "verified" {
                    lines.push(format!(
                        "missing: {role} slot '{id}' is not verified under the board's keys: install a bundle a listed key signed"
                    ));
                }
            }
        }
        let mut text = lines.join("\n");
        text.push('\n');
        let bytes = text.as_bytes();
        into.get_mut(..bytes.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

impl Slots for SystemSlots {
    fn list(&mut self, into: &mut [u8]) -> Result<usize> {
        let json = self.list_json();
        let out = into.get_mut(..json.len()).ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(json.as_bytes());
        Ok(json.len())
    }

    fn list_page(&mut self, after: &str, into: &mut [u8]) -> Result<usize> {
        let json = self.list_page_json(after, into.len())?;
        let out = into.get_mut(..json.len()).ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(json.as_bytes());
        Ok(json.len())
    }

    fn info(&mut self, into: &mut [u8]) -> Result<usize> {
        let json = self.info_json();
        let out = into.get_mut(..json.len()).ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(json.as_bytes());
        Ok(json.len())
    }

    fn install(&mut self, size: u64, pin: bool, keep_old: bool) -> Result<JobId> {
        self.writable()?;
        self.begin(size, pin, keep_old, None)
    }

    fn accept(&mut self, size: u64) -> Result<()> {
        self.writable()?;
        if self.install.is_some() {
            return Err(Status::BUSY);
        }
        self.room(size)
    }

    fn boots(&mut self, into: &mut [u8]) -> Result<usize> {
        let text = boots::render(&boots::load(&self.layout));
        let out = into.get_mut(..text.len()).ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(text.as_bytes());
        Ok(text.len())
    }

    fn install_file(&mut self, name: &str, pin: bool, keep_old: bool) -> Result<JobId> {
        self.writable()?;
        if self.install.is_some() {
            return Err(self.refuse(Status::BUSY, words::INSTALL_RUNNING));
        }
        let incoming = self.layout.images().join(INCOMING);
        let (source, size) = install::open_incoming(&incoming, name)
            .map_err(|(status, words)| self.refuse(status, words))?;
        // A file that can't be installed is the driver's to delete too.
        let job = self.begin(size, pin, keep_old, Some(source));
        if job.is_err() {
            let _ = fs::remove_file(incoming.join(name));
        }
        job
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.writable()?;
        let Some(mut install) = self.install.take() else {
            return Err(Status::NOT_FOUND);
        };
        let result = if install.end.is_some() {
            Err(Status::NOT_FOUND)
        } else if install.owner != self.calling || install.source.is_some() {
            Err(Status::NOT_PERMITTED)
        } else {
            self.feed(&mut install, offset, bytes)
        };
        self.install = Some(install);
        result
    }

    fn set_default(&mut self, id: &str, force: bool) -> Result<()> {
        self.writable()?;
        self.installed(id)?;
        self.fits_board(id)?;
        self.fits_signature(id)?;
        let meta = Meta::load(&self.layout, id);
        if meta.failed() && !force {
            return Err(self.refuse(Status::NOT_PERMITTED, words::FAILED_TRIAL));
        }
        if meta.get("GOOD") != "yes" {
            return Err(self.refuse(Status::NOT_PERMITTED, words::NEVER_GOOD));
        }
        let mut state = self.state();
        // A switch the boot loader can't act on is refused before anything is written.
        crate::boot::loader_reads_record(self.record.as_ref())
            .map_err(|e| self.record_refused(e))?;
        self.record
            .set_default(id)
            .map_err(|e| self.record_refused(e))?;
        self.record
            .fallback_copy(&self.layout, id)
            .map_err(|_| Status::IO)?;
        if state.default != id {
            state.previous = core::mem::replace(&mut state.default, id.to_string());
        }
        self.save(&state)?;
        self.evict(&state, "")
    }

    fn trial(&mut self, id: &str, keep_default: bool, keep_old: bool, force: bool) -> Result<()> {
        self.writable()?;
        self.installed(id)?;
        self.fits_board(id)?;
        self.fits_medium(id)?;
        self.fits_signature(id)?;
        // Before anything is written: a board whose loader can't start a trial refuses it, by its reason.
        if let TrialUpdates::Unavailable(why) = self.look_at_trial_updates() {
            let sentence = why.reason.sentence().to_string();
            self.note(sentence.clone());
            let words = format!("{}: {sentence}", words::NO_TRIALS);
            return Err(self.refuse(Status::NOT_PERMITTED, words));
        }
        if id == self.running() {
            return Err(self.refuse(Status::BUSY, words::TRIAL_RUNNING));
        }
        let mut meta = Meta::load(&self.layout, id);
        if meta.failed() && !force {
            return Err(self.refuse(Status::NOT_PERMITTED, words::FAILED_TRIAL));
        }
        // Before anything is written: a trial is not armed when the restart it needs is refused.
        self.restart_check()?;
        let mut state = self.state();
        // Room for its boot files first: only the resident slots' stay.
        self.mark_keep_old(id, keep_old)?;
        boot::clear_reason(&self.boot_node);
        self.record
            .set_trial(id, self.policy.attempts)
            .map_err(|_| Status::IO)?;
        meta.set("RETRIED", "no");
        meta.save(&self.layout, id).map_err(|_| Status::IO)?;
        let marker = self.layout.marker("trial-keeps-default");
        if keep_default {
            store::replace(&marker, format!("{id}\n").as_bytes()).map_err(|_| Status::IO)?;
        } else {
            let _ = fs::remove_file(&marker);
        }
        state.trial = id.to_string();
        state.state = "rebooting".into();
        state.error.clear();
        self.save(&state)?;
        // The loader takes one attempt before the trial boots, so the record now says how many
        // there are: the trial starts on a plain restart.
        let _ = Log {
            slot: id.to_string(),
            seen: self.policy.attempts,
            ..Log::default()
        }
        .save(&self.layout);
        self.record.restart().map_err(|error| {
            self.restart_failed(error.to_string());
            Status::IO
        })
    }

    fn confirm(&mut self) -> Result<()> {
        self.writable()?;
        self.confirm_as(resolve::By::Call(self.calling))
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        self.writable()?;
        if !valid_id(id) {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.installed(id)?;
        let mut state = self.state();
        let busy = if id == self.running() {
            Some(words::REMOVE_RUNNING)
        } else if id == state.default {
            Some(words::REMOVE_DEFAULT)
        } else if id == state.trial {
            Some(words::REMOVE_TRIAL)
        } else {
            None
        };
        if let Some(words) = busy {
            return Err(self.refuse(Status::BUSY, words));
        }
        if Meta::load(&self.layout, id).pinned() {
            return Err(self.refuse(Status::NOT_PERMITTED, words::REMOVE_PINNED));
        }
        // Never the only slot that can boot.
        if !self.layout.slots().iter().any(|other| other != id) {
            return Err(self.refuse(Status::BUSY, words::REMOVE_ONLY));
        }
        self.remove_slot(&mut state, id)
    }

    fn pin(&mut self, id: &str, pinned: bool) -> Result<()> {
        self.writable()?;
        self.installed(id)?;
        let mut meta = Meta::load(&self.layout, id);
        meta.set("PINNED", if pinned { "yes" } else { "no" });
        meta.save(&self.layout, id).map_err(|_| Status::IO)
    }

    fn set_keep(&mut self, keep: u32) -> Result<()> {
        self.writable()?;
        store::set_keep(&self.layout, keep).map_err(|_| Status::IO)
    }
}

/// Restarts the board, through the restart helper; and into the board's way back (`reboot_to`).
///
/// Settings: `restart_socket`, as the slot driver reads it; `loop_return`, the board file's
/// `[boot] loop_return`, which the host gives (`"rpiboot"`, asked through `vcio`).
#[driver("system-reboot", provides = [Reboot])]
pub struct SystemReboot {
    restarter: Restarter,
    /// The state directory's parent: the data partition, where the boot history reads the request.
    data: PathBuf,
    calling: CallerId,
    /// The board's way back (`loop_return`), when the board file declares one.
    way_back: Option<String>,
    /// Asks the firmware for the way back at the next boot: the mailbox the root phase holds
    /// (`lemnos_driver_rpi_firmware_sys::hold`), or `None` when the board's record has no firmware.
    arm: Option<Arm>,
}

/// Asks the firmware for the next boot's way back. The real one is the mailbox; the tests record.
pub(crate) type Arm = Box<dyn Fn() -> std::io::Result<()> + Send>;

/// The mailbox's request for the firmware's flashing mode at the next boot (`request_rpiboot`).
fn rpiboot_arm(firmware: count::Firmware) -> Arm {
    Box::new(move || {
        lemnos_driver_rpi_firmware_sys::Mailbox::open(&firmware.vcio)?.request_rpiboot()
    })
}

impl Lifecycle for SystemReboot {
    type Needs = SystemRebootNeeds;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(REBOOT_KEYS)
    }

    fn start(_: SystemRebootNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (layout, ubi_write) = layout_of(cfg);
        let node = boot_node_of(cfg);
        let base = record_for(cfg, &layout, &ubi_write, None)?;
        let check = guard_check(layout, node, base, firmware_of(cfg));
        Ok(Self {
            restarter: restarter(cfg, Some(check)),
            data: PathBuf::from(cfg.str("data").unwrap_or("/data")),
            calling: CallerId::LOCAL,
            way_back: cfg.str("loop_return").map(str::to_string),
            arm: firmware_of(cfg).map(rpiboot_arm),
        })
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }
}

impl Reboot for SystemReboot {
    fn reboot(&mut self) -> Result<()> {
        // The boot history names who asked; the restart goes on either way.
        let state_dir = self.data.join("board/slots");
        boot::note_request(&state_dir, self.calling.0);
        self.restarter.run().map_err(|_| Status::IO)
    }

    fn reboot_to(&mut self, target: &str) -> Result<()> {
        // What the board offers, then the switch, then the firmware: each refusal asks and restarts nothing.
        let Some(way_back) = self.way_back.as_deref() else {
            return Err(Status::NOT_SUPPORTED);
        };
        if target != way_back {
            return Err(Status::INVALID_ARGUMENT);
        }
        let arm = match (way_back, &self.arm) {
            ("rpiboot", Some(arm)) => arm,
            _ => return Err(Status::NOT_SUPPORTED),
        };
        if security::enforced() {
            return Err(Status::NOT_PERMITTED);
        }
        // The helper is asked whether a restart would be taken before the firmware is: its check writes
        // nothing, and a helper that refuses leaves the firmware's order unset.
        self.restarter.ready().map_err(|_| Status::IO)?;
        arm().map_err(|_| Status::IO)?;
        // The firmware's order is for the next boot only, so a restart that fails after it leaves it set:
        // the next restart, whoever asks for it, goes to the way back.
        let state_dir = self.data.join("board/slots");
        boot::note_request(&state_dir, self.calling.0);
        self.restarter.run().map_err(|_| Status::IO)
    }
}

/// The data partition's grow for `info`: its state and the words or sizes behind it (see [`grow`]).
pub(crate) fn grow_info(grow: Option<&grow::Task>, out: &mut String) {
    match grow {
        None => out.push_str(",\"grow_state\":\"none\""),
        Some(grow) if grow.refused().is_some() => {
            let refusal = grow.refused().unwrap_or_else(|| unreachable!());
            out.push_str(&format!(",\"grow_state\":\"{}\"", refusal.state));
            if let Some((target, max)) = refusal.sizes {
                out.push_str(&format!(
                    ",\"grow_target_bytes\":{target},\"grow_max_bytes\":{max}"
                ));
            }
            out.push_str(",\"grow_reason\":");
            quoted(&refusal.reason, out);
        }
        Some(grow) if grow.done().is_some() => {
            out.push_str(",\"grow_state\":\"not-needed\",\"grow_reason\":");
            quoted(grow.done().unwrap_or_default(), out);
        }
        Some(grow) if grow.progress().is_some() => {
            let progress = grow.progress().unwrap_or_else(|| unreachable!());
            let state = if progress.overrun {
                "resize-overrun"
            } else {
                "resizing"
            };
            out.push_str(&format!(
                ",\"grow_state\":\"{state}\",\"grow_started_ms\":{},\"grow_elapsed_ms\":{},\"grow_bound_ms\":{},\"grow_target_bytes\":{}",
                progress.started_ns / 1_000_000,
                progress.elapsed_ns / 1_000_000,
                progress.bound_ns / 1_000_000,
                progress.target_bytes
            ));
        }
        Some(grow) => match grow.unconfirmed() {
            None => out.push_str(",\"grow_state\":\"pending\""),
            Some(unconfirmed) => {
                out.push_str(",\"grow_state\":\"grown-unconfirmed\",\"grow_baseline_bytes\":");
                match unconfirmed.baseline {
                    Some(bytes) => out.push_str(&bytes.to_string()),
                    None => out.push_str("null"),
                }
                out.push_str(&format!(",\"grow_seen_bytes\":{}", unconfirmed.seen));
                out.push_str(",\"grow_reason\":");
                quoted(&unconfirmed.reason, out);
            }
        },
    }
}

impl SystemSlots {
    /// One try of the data partition's grow, when it is due (see [`grow`]). A failed pass is noted
    /// in words once per reason and tried again after its backoff, never given up; a finished one
    /// is dropped. It never holds up the driver.
    fn grow_tick(&mut self, now_ns: u64) -> Option<u64> {
        let task = self.grow.as_mut()?;
        match task.tick_real(now_ns) {
            grow::Tick::At(next) => Some(next),
            grow::Tick::Refused { why, fresh } => {
                if fresh {
                    self.note(why);
                }
                None
            }
            grow::Tick::Failed { why, fresh, at } => {
                if fresh {
                    self.note(format!(
                        "the data partition is not grown yet: {why}; it is tried again"
                    ));
                }
                Some(at)
            }
            grow::Tick::Finished => {
                self.grow = None;
                None
            }
        }
    }
}

impl SystemSlots {
    /// Records this boot in the boot history, once per boot: its slot (on trial, when a trial is
    /// running), the reset reason, and the restart `system.reboot` requested, which is then
    /// taken away. Called once at start, after the trial's accounting.
    fn record_boot(&mut self) {
        let state = self.state();
        let running = self.running();
        let slot = if valid_id(&running) {
            running
        } else {
            String::new()
        };
        let outcome = if !slot.is_empty() && slot == state.trial {
            boots::ON_TRIAL
        } else {
            "unknown"
        };
        let requested = boot::read_request(&self.layout.state_dir());
        let reset = boot::reset_reason(self.reset_reason.as_deref());
        let start = crash::Start {
            boot: &self.boot,
            slot: &slot,
            outcome,
            reset: &reset,
            requested: &requested,
            keep: self.boots_keep,
        };
        let pstore = self
            .crash
            .as_deref()
            .map(|source| (source, self.crash_bytes));
        let recorded = crash::record(&self.layout, start, pstore);
        self.crash_note = recorded.note;
        if recorded.appended {
            let _ = fs::remove_file(self.layout.marker(boot::REQUEST_FILE));
        }
    }
}

impl SystemSlots {
    /// The slow-period boot-file check: at once, then every ten minutes. It repoints the default
    /// when the default's files no longer match, and otherwise only reports.
    fn slow_check(&mut self, now_ns: u64) -> Option<u64> {
        const PERIOD_NS: u64 = 600 * 1_000_000_000;
        /// How soon a tick looks again for the answer of a look that is running.
        const ANSWER_NS: u64 = 100_000_000;
        let check = self.check.clone()?;
        let look = self.look.clone()?;
        if let Some(answer) = &self.looking {
            let seen = match answer.try_recv() {
                Ok(seen) => seen,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    return Some(now_ns.saturating_add(ANSWER_NS));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => boot_guard::Seen::Neither(
                    "the boot-file look ended without an answer; it runs again at its next period"
                        .into(),
                ),
            };
            self.looking = None;
            match seen {
                boot_guard::Seen::Fine => {}
                boot_guard::Seen::Neither(text) => self.note(text),
                // The default's files changed under the board: the check reads both slots again
                // and moves the default, here on the tick, since it writes the records.
                boot_guard::Seen::Repoint => match (check.0)(false) {
                    Ok(Some(note)) => self.note(note),
                    Ok(None) => {}
                    Err(why) => self.note(why),
                },
            }
        }
        if now_ns >= self.next_check_ns {
            let (tell, answer) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .name("slots-look".into())
                .spawn(move || {
                    let _ = tell.send((look.0)());
                });
            match started {
                Ok(_) => {
                    self.looking = Some(answer);
                    self.next_check_ns = now_ns.saturating_add(PERIOD_NS);
                    return Some(now_ns.saturating_add(ANSWER_NS));
                }
                // No thread now: tried again at the failure rule's cap.
                Err(e) => {
                    self.note(format!(
                        "the boot-file look couldn't start its thread ({e}); it is tried again"
                    ));
                    self.next_check_ns = now_ns.saturating_add(OWNER_RETRY_NS);
                }
            }
        }
        Some(self.next_check_ns)
    }

    /// Whether trial updates are available on this board, found now (the record is read; nothing is
    /// written). `info` and `health` show the last look, which `trial` and a periodic look refresh.
    pub fn trial_updates(&self) -> TrialUpdates {
        self.record.trial_updates()
    }

    /// Finds whether trial updates are available now, and keeps it for `info` and `health`.
    fn look_at_trial_updates(&mut self) -> TrialUpdates {
        let found = self.record.trial_updates();
        self.trial_updates = found.clone();
        found
    }

    /// Looks at the trial updates again every ten seconds, while the record can say no (a partition
    /// added, or U-Boot's marker saved, changes the answer without a request). Returns the next look.
    fn trial_watch(&mut self, now_ns: u64) -> Option<u64> {
        const EVERY_NS: u64 = 10_000_000_000;
        if now_ns >= self.trial_watch_ns {
            self.look_at_trial_updates();
            self.trial_watch_ns = now_ns.saturating_add(EVERY_NS);
        }
        Some(self.trial_watch_ns)
    }

    /// A record write's error as a status, with its text as the state's error (what `info` shows): a
    /// refusal by the loader's state is `NOT_PERMITTED` (the write was not made); any other failure, such as
    /// an environment that does not fit, is `IO`.
    pub(crate) fn record_refused(&self, error: std::io::Error) -> Status {
        let refused = error.kind() == std::io::ErrorKind::Unsupported;
        self.note(error.to_string());
        if refused {
            Status::NOT_PERMITTED
        } else {
            Status::IO
        }
    }

    /// Writes `text` as the state's error (what `info` and `list` show).
    fn note(&self, text: String) {
        let mut state = self.state();
        state.error = text;
        let _ = self.save(&state);
    }

    /// Refuses a restart whose boot files don't match (see [`boot_guard`]); the reason is the
    /// state's error.
    fn restart_check(&mut self) -> Result<()> {
        let mut refused = None;
        if let Some(check) = &self.check
            && let Err(why) = (check.0)(true)
        {
            refused = Some(why);
        }
        if refused.is_none()
            && let Some(restarter) = &self.restarter
            && let Err(why) = restarter.ready()
        {
            refused = Some(why);
        }
        match refused {
            None => Ok(()),
            Some(why) => {
                self.note(why.clone());
                Err(self.refuse(Status::IO, format!("{}: {why}", words::RESTART_REFUSED)))
            }
        }
    }

    /// A trial's restart that failed: the trial is disarmed, so the next restart, whoever makes
    /// it, boots the default, and the state says the restart failed and why.
    fn restart_failed(&mut self, why: String) {
        let _ = self.record.clear_trial();
        let _ = fs::remove_file(self.layout.marker("trial-keeps-default"));
        let mut state = self.state();
        state.trial.clear();
        state.state = RESTART_FAILED.into();
        state.error = format!("the restart failed: {why}");
        let _ = self.save(&state);
    }

    /// Reads what came of a restart the helper accepted after its call returned: one that failed
    /// after all is handled as [`Self::restart_failed`]. Looked at every 100 ms while it waits.
    fn restart_watch(&mut self, now_ns: u64) -> Option<u64> {
        const EVERY_NS: u64 = 100_000_000;
        let restarter = self.restarter.as_ref()?;
        if let Some(Err(why)) = restarter.watch.outcome() {
            if self.state().state == "rebooting" {
                self.restart_failed(why);
            } else {
                self.note(format!("the restart failed: {why}"));
            }
        }
        let restarter = self.restarter.as_ref()?;
        restarter
            .watch
            .waiting()
            .then(|| now_ns.saturating_add(EVERY_NS))
    }
}
