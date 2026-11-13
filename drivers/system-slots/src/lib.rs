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
//!   boot flow: `boot-record.env`, a per-slot `extlinux.conf` and the static script in
//!   `packaging/uboot`; see [`uboot`]), `"ubi"` (the same on raw NAND, the record being the UBI
//!   volume `boot-record` and the script `packaging/uboot/boot-ubi.cmd`; needs `ubi`) or
//!   `"file"` (one `boot-record` file, for a loader with one fixed kernel; see [`boot`]).
//!   `docs/slots.md` describes each.
//! - `ubi`: on raw NAND, the UBI device (`"ubi0"`) whose static volumes hold the images, as
//!   `slot-<id>` ([`lemnos_slot_store::ubi`]). `data` and `boot` are then where its UBIFS
//!   volumes `data` and `boot` are mounted, and a slot's command line names the data volume as
//!   `<boot_args>.data=UBI=ubi0:data`. Unset, the images are files on the data partition.
//! - `systemctl`: the program that restarts the board, `"systemctl"` by default; it is run as
//!   `systemctl reboot`, with `--reboot-argument=0 tryboot` for a trial.
//! - `keys`: a file of the keys the board trusts, one per line: 64 hex digits, a space, and the
//!   key's owner. A signed bundle's `slot.sig` is the 108-byte signature record, which names its
//!   key by id; the signature is checked against that key when the board holds it. The policy for each case is
//!   the table in [`lemnos_slot_store::signing`], shared with first flash. `list` shows each slot
//!   as `trusted` (with the owner as `signer`), `untrusted` or `unsigned`, and its `signature`
//!   word: `verified`, `not_checked_unknown_key` or `not_checked_unsigned`.
//! - `require_signed`: `true` refuses every bundle the table refuses (unsigned, signed by a key
//!   the board does not trust, or a bad signature). Off by default: an unsigned bundle or one
//!   under an unknown key installs and is reported as not checked. With it on, a slot whose
//!   signature did not verify is never set as the default nor tried, and the boot side does not
//!   start it either (see `docs/slots.md`).
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
//! - `reset_reason`: a file holding the board's reset reason, read at each boot for the history;
//!   unset, the first watchdog's `bootstatus` is read.
//!   The `system-reboot` driver's `data` (`/data` by default) is where it leaves the request
//!   the next boot's history names as `requested`.
//! - The trial settings, which [`trial`] and [`resolve`] apply: `auto_confirm` (true: the board
//!   confirms a healthy trial itself), `confirm_uptime_s` (30), `confirm_deadline_s` (600),
//!   `reboot_at_deadline` (true), `trial_attempts` (3, from 1 to 3) and `health`, a list of
//!   `device:<id>`, `file:<path>` and `socket:<path>` sources the trial must see good. The keys and
//!   defaults are in `docs/boards.md`; `docs/slots.md`, "How a trial resolves", says how they work.
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

pub mod boot;
mod boot_guard;
#[cfg(test)]
mod boot_guard_tests;
mod install;
mod resolve;
pub use lemnos_slot_store::{json, manifest, store};
pub mod tar;
pub mod trial;
pub mod uboot;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lemnos_contract_indicator::{Indicator as IndicatorContract, NO_PROGRESS};
use lemnos_contract_system_reboot::Reboot;
use lemnos_contract_system_slots::Slots;
use lemnos_core::{
    BoardView, CallerId, Config, ConfigError, Dev, JobId, JobState, Key, Kind, Lifecycle, Reply,
    Result, Status, driver,
};

use boot::{BootRecord, Check, FileRecord, Restarter, Tryboot};
use install::{Failure, Finish, Guard, INCOMING, Install, NO_SPACE};
pub use install::{SIGNATURE_LEN, TrustedKey, parse_keys};
use json::quoted;
use lemnos_slot_store::boots;
use lemnos_slot_store::signing::{self, Outcome};
use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};
use store::{DEFAULT_KEEP, Layout, Meta, State, read_line, valid_id};
use trial::{Log, Policy, Resolved, Source};
use uboot::Uboot;

#[driver("system-slots", provides = [Slots])]
pub struct SystemSlots {
    layout: Layout,
    record: Box<dyn BootRecord>,
    keys: Vec<TrustedKey>,
    require_signed: bool,
    partuuid: String,
    boot_args: String,
    /// The board file's `[board] id`. A slot built for another board is never booted.
    board_id: String,
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
    /// Where the board's reset reason is read from, when the board file names a file for it.
    reset_reason: Option<PathBuf>,
    /// The last install's signature result, and the slot it made (`""` when it was refused):
    /// `info` shows it.
    last_signed: Option<(String, std::result::Result<Outcome, String>)>,
    /// The boot-file check before lemnosd's own restarts and on a slow period (see
    /// [`boot_guard`]), and when the next slow-period check is due.
    check: Option<Check>,
    next_check_ns: u64,
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

fn restarter(cfg: &Config<'_>, check: Option<Check>) -> Restarter {
    Restarter {
        systemctl: PathBuf::from(cfg.str("systemctl").unwrap_or("systemctl")),
        check,
    }
}

/// The boot record the `record` setting names, with its restarter (checked when `check` is given).
fn record_for(
    cfg: &Config<'_>,
    layout: &Layout,
    ubi_write: &Option<Arc<dyn UbiWrite>>,
    check: Option<Check>,
) -> Result<Box<dyn BootRecord>> {
    let restarter = restarter(cfg, check);
    Ok(match cfg.str("record").unwrap_or("tryboot") {
        "tryboot" => Box::new(Tryboot {
            boot: layout.boot.clone(),
            restarter,
        }),
        "file" => Box::new(FileRecord {
            boot: layout.boot.clone(),
            restarter,
        }),
        "uboot" => Box::new(Uboot {
            boot: layout.boot.clone(),
            restarter,
            volume: None,
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

/// The check over a record of its own: [`boot_guard::check`], which may point the record at the
/// previous slot.
fn guard_check(layout: Layout, record: Box<dyn BootRecord>) -> Check {
    let record = Mutex::new(record);
    Check(Arc::new(move |restart| {
        let mut record = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        boot_guard::check(&layout, record.as_mut(), restart)
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
        let mut slots = Self {
            layout,
            record,
            keys,
            require_signed: false,
            partuuid: String::new(),
            boot_args: String::new(),
            board_id: String::new(),
            running: None,
            reserve: 2048 << 20,
            keep: DEFAULT_KEEP,
            calling: CallerId::LOCAL,
            install: None,
            next_job: 0,
            indicator: None,
            shown: None,
            read_buf: Vec::new(),
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
            reset_reason: None,
            last_signed: None,
            check: None,
            next_check_ns: 0,
        };
        slots.clear_interrupted();
        // What a sender left in `.incoming` belongs to no install now.
        let _ = install::reset_incoming(&slots.layout);
        slots
    }

    /// An install the service's restart cut short: its work goes, and the board's state says so.
    fn clear_interrupted(&mut self) {
        if install::clear_leftovers(&self.layout, self.ubi_write.as_deref()) {
            let mut state = State::load(&self.layout);
            if state.state == "staging" {
                state.state = "error".into();
                state.error = "an install was interrupted: the service restarted".into();
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

    /// The slots that keep their directory on the boot partition, and `also`.
    fn resident(&self, state: &State, also: &str) -> Vec<String> {
        [
            self.running(),
            state.default.clone(),
            state.previous.clone(),
            state.trial.clone(),
            also.to_string(),
        ]
        .into_iter()
        .filter(|id| !id.is_empty())
        .collect()
    }

    fn evict(&self, state: &State, also: &str) -> Result<()> {
        let resident = self.resident(state, also);
        let names: Vec<&str> = resident.iter().map(String::as_str).collect();
        boot::evict(&self.layout, &names).map_err(|_| Status::IO)
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
        install.discard();
        let mut state = State::load(&self.layout);
        state.state = "error".into();
        state.error = failure.1.clone();
        state.progress = 0;
        let _ = state.save(&self.layout);
        install.end = Some(Err(failure));
        status
    }

    /// The install's next bytes; the last of them puts the slot in place.
    fn feed(&mut self, install: &mut Install, offset: u64, bytes: &[u8]) -> Result<()> {
        if let Err(failure) = install.take(offset, bytes) {
            // A write out of order changes nothing: the install goes on from where it is.
            if offset != install.got {
                return Err(failure.0);
            }
            return Err(self.fail_install(install, failure));
        }
        if !install.complete() {
            // Bytes arriving: show the progress as they do, not only on ticks.
            self.show("receiving", permille(install.got, install.size));
            return Ok(());
        }
        // The hash, verity and signature are checked by `finish`, whichever way the bytes came.
        self.show("verifying", NO_PROGRESS);
        let mut state = self.state();
        let finish = Finish {
            layout: &self.layout,
            keys: &self.keys,
            require_signed: self.require_signed,
            boot_args: &self.boot_args,
            board_id: &self.board_id,
            guard: Guard {
                running: self.running(),
                default: state.default.clone(),
            },
            record: &*self.record,
        };
        match install.finish(&finish) {
            Ok(id) => {
                if let Some(signed) = install.signed.clone() {
                    self.last_signed = Some((id.clone(), signed));
                }
                self.mark_keep_old(&id, install.keep_old)?;
                state.staged = id.clone();
                state.state = "staged".into();
                state.error.clear();
                state.progress = 1000;
                self.save(&state)?;
                install.drop_source();
                install.end = Some(Ok(id));
                self.show("done", NO_PROGRESS);
                Ok(())
            }
            Err(failure) => {
                if let Some(signed) = install.signed.clone() {
                    self.last_signed = Some((String::new(), signed));
                }
                Err(self.fail_install(install, failure))
            }
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

    /// With `require_signed`, a slot may be set as the default or tried only when its signature
    /// verified. The reason in the state when not.
    fn fits_signature(&self, id: &str) -> Result<()> {
        if !self.require_signed {
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
            state.error = format!(
                "no room for this slot: it needs {} MiB with the reserve, the data partition has {} MiB free; remove a slot first",
                size.saturating_add(self.reserve) >> 20,
                free >> 20
            );
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
        if self.require_signed && self.keys.is_empty() {
            return Err(Status::NOT_PERMITTED);
        }
        self.room(size)?;
        let mut state = self.state();
        let guard = Guard {
            running: self.running(),
            default: state.default.clone(),
        };
        self.next_job += 1;
        let job = JobId(self.next_job);
        let mut install = Install::start(
            &self.layout,
            self.calling,
            job,
            size,
            pin,
            keep_old,
            guard,
            self.ubi_write.clone(),
        )
        .map_err(|failure| failure.0)?;
        install.source = source;
        state.state = "staging".into();
        state.error.clear();
        state.progress = 0;
        state.started_by = "lemnos".into();
        self.save(&state)?;
        self.install = Some(install);
        self.show("receiving", 0);
        Ok(job)
    }

    /// The next piece of an install from a file: what a tick does.
    fn pull(&mut self) {
        let Some(mut install) = self.install.take() else {
            return;
        };
        if install.end.is_none() && install.source.is_some() {
            let mut buf = core::mem::take(&mut self.read_buf);
            buf.resize(install::READ_EVERY_TICK, 0);
            match install.pull(&mut buf) {
                Ok(read) => {
                    let at = install.got;
                    let _ = self.feed(&mut install, at, &buf[..read]);
                }
                Err(failure) => {
                    self.fail_install(&mut install, failure);
                }
            }
            self.read_buf = buf;
        }
        self.install = Some(install);
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
        self.save(state)?;
        let io = |_| Status::IO;
        let dir = self.layout.boot_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(io)?;
            store::sync_dir(&self.layout.boot.join("slots")).map_err(io)?;
        }
        if let (Some(device), Some(writer)) = (&self.layout.ubi, &self.ubi_write) {
            let volume = ubi::image_volume(id);
            if device.volume(&volume).is_some() {
                writer.remove(&volume).map_err(io)?;
            }
        }
        for path in [
            self.layout.image(id),
            self.layout.manifest(id),
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
            self.require_signed
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
        out.push('}');
        out
    }

    /// Reports, at start and in `info`, a default slot whose signature did not verify while
    /// signing is required: the state's `error` names it and what to install.
    fn report_policy(&mut self) {
        if !self.require_signed {
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
            "\"size\":{size},\"verity\":{},\"intact\":{},\"flags\":[{}],\"pinned\":{},\"failures\":{},\"failed\":{},\"signature\":\"{}\"}}",
            !text("verity.root_hash").is_empty(),
            want == Some(size),
            flags.join(","),
            meta.pinned(),
            meta.get("FAILURES").parse::<u32>().unwrap_or(0),
            meta.failed(),
            signature_word(&meta),
        ));
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
    Key::optional("systemctl", Kind::Str),
    Key::optional("keys", Kind::Str),
    Key::optional("partuuid", Kind::Str),
    Key::optional("running", Kind::Str),
    Key::optional("boot_args", Kind::Str),
    Key::optional("require_signed", Kind::Bool),
    Key::optional("reserve_mb", Kind::Int),
    Key::optional("keep", Kind::Int),
    Key::optional("boots_keep", Kind::Int),
    Key::optional("reset_reason", Kind::Str),
    Key::optional("auto_confirm", Kind::Bool),
    Key::optional("reboot_at_deadline", Kind::Bool),
    Key::optional("confirm_uptime_s", Kind::Int),
    Key::optional("confirm_deadline_s", Kind::Int),
    Key::optional("trial_attempts", Kind::Int),
    Key::optional("health", Kind::List),
];

/// The settings `SystemReboot` reads.
const REBOOT_KEYS: &[Key] = &[
    Key::optional("systemctl", Kind::Str),
    Key::optional("data", Kind::Str),
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
            Source::parse(text).map_err(|_| {
                ConfigError::new(
                    "health",
                    "entries must be device:<id>, file:<path> or socket:<path>",
                )
            })
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
    const TRACKS_CALLERS: bool = true;
    const JOBS: bool = true;
    const TICKS: bool = true;
    const WATCHES_BOARD: bool = true;

    fn board(&mut self, view: &dyn BoardView) {
        self.watch(view);
    }

    /// An install from a file reads a piece each tick, at once again while there is more. A trial
    /// that waits is looked at again each second, until it is confirmed or falls back. The
    /// indicator is brought up to date on each tick too.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.pull();
        self.indicate();
        let install = self
            .install
            .as_ref()
            .filter(|install| install.end.is_none() && install.source.is_some())
            .map(|_| now_ns);
        let settle = self.settle(now_ns);
        let slow = self.slow_check(now_ns);
        [install, settle, slow].into_iter().flatten().min()
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
        Ok(())
    }

    fn start(needs: SystemSlotsNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (layout, ubi_write) = layout_of(cfg);
        // Two records: the check holds one (to point the default at the previous slot), and the
        // driver's own is built with the check in its restarter, so every restart is checked.
        let base = record_for(cfg, &layout, &ubi_write, None)?;
        let check = guard_check(layout.clone(), base);
        let record = record_for(cfg, &layout, &ubi_write, Some(check.clone()))?;
        // A key file that can't be read leaves the board trusting no bundle, which every
        // install then says; it is read again when the driver is bound again.
        let keys = cfg
            .str("keys")
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| parse_keys(&text).ok())
            .unwrap_or_default();
        let mut slots = Self::with_volumes(layout, record, keys, ubi_write);
        slots.check = Some(check);
        slots.policy = trial_policy(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        slots.sources =
            health_sources(&slots.policy.health).map_err(|_| Status::INVALID_ARGUMENT)?;
        slots.probes = vec![trial::Probe::default(); slots.sources.len()];
        slots.boot = read_boot_id();
        slots.indicator = needs.indicator;
        slots.partuuid = cfg
            .str("partuuid")
            .map(str::to_string)
            .or_else(|| find_partuuid(&slots.layout.data))
            .unwrap_or_default();
        slots.running = cfg.str("running").map(str::to_string);
        slots.boot_args = cfg.str("boot_args").unwrap_or("").to_string();
        slots.board_id = cfg.board().to_string();
        slots.require_signed = cfg.bool("require_signed").unwrap_or(false);
        signing::set_security(&slots.layout, slots.require_signed).map_err(|_| Status::IO)?;
        slots.report_policy();
        if let Some(mb) = cfg.int("reserve_mb") {
            slots.reserve = u64::try_from(mb).map_err(|_| Status::INVALID_ARGUMENT)? << 20;
        }
        if let Some(keep) = cfg.int("keep") {
            slots.keep = u32::try_from(keep).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
        // The attempts a fallback trial gets are the board's, so the default slot's loader can
        // take them from its own settings.
        let _ = store::write_setting(
            &slots.layout,
            "TRIAL_ATTEMPTS",
            &slots.policy.attempts.to_string(),
        );
        slots.resolve_at_start();
        if let Some(keep) = cfg.int("boots_keep") {
            slots.boots_keep = usize::try_from(keep).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
        slots.reset_reason = cfg.str("reset_reason").map(PathBuf::from);
        slots.record_boot();
        Ok(slots)
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(install) = self.install.as_ref().filter(|i| i.job == job) else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        let state = match &install.end {
            None => {
                return JobState::Running {
                    done: install.got,
                    total: install.size,
                };
            }
            Some(Ok(id)) => JobState::Finished(match result.write(id.as_bytes()) {
                Ok(()) => Status::OK,
                Err(status) => status,
            }),
            Some(Err((status, _))) => JobState::Finished(*status),
        };
        self.install = None;
        state
    }

    fn job_cancel(&mut self, job: JobId) {
        if let Some(install) = self
            .install
            .as_mut()
            .filter(|i| i.job == job && i.end.is_none())
        {
            install.discard();
            install.end = Some(Err((Status::CANCELLED, String::new())));
            let mut state = State::load(&self.layout);
            state.state = "cancelled".into();
            state.error.clear();
            state.progress = 0;
            let _ = state.save(&self.layout);
        }
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
        self.begin(size, pin, keep_old, None)
    }

    fn accept(&mut self, size: u64) -> Result<()> {
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
        if self.install.is_some() {
            return Err(Status::BUSY);
        }
        let incoming = self.layout.images().join(INCOMING);
        let (source, size) = install::open_incoming(&incoming, name)?;
        // A file that can't be installed is the driver's to delete too.
        let job = self.begin(size, pin, keep_old, Some(source));
        if job.is_err() {
            let _ = fs::remove_file(incoming.join(name));
        }
        job
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
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
        self.installed(id)?;
        self.fits_board(id)?;
        self.fits_signature(id)?;
        let meta = Meta::load(&self.layout, id);
        if meta.failed() && !force {
            return Err(Status::NOT_PERMITTED);
        }
        if meta.get("GOOD") != "yes" {
            return Err(Status::NOT_PERMITTED);
        }
        let mut state = self.state();
        self.evict(&state, id)?;
        boot::stage(&self.layout, id, false).map_err(|_| Status::IO)?;
        self.record.set_default(id).map_err(|_| Status::IO)?;
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
        self.installed(id)?;
        self.fits_board(id)?;
        self.fits_signature(id)?;
        if id == self.running() {
            return Err(Status::BUSY);
        }
        // Before anything is written: a trial is not armed when the restart it needs is refused.
        self.restart_check()?;
        let mut meta = Meta::load(&self.layout, id);
        if meta.failed() && !force {
            return Err(Status::NOT_PERMITTED);
        }
        let mut state = self.state();
        // Room for its boot files first: only the resident slots' stay.
        self.evict(&state, id)?;
        boot::stage(&self.layout, id, false).map_err(|_| Status::IO)?;
        self.mark_keep_old(id, keep_old)?;
        boot::clear_reason(&self.layout.boot);
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
        self.record.restart().map_err(|_| Status::IO)
    }

    fn confirm(&mut self) -> Result<()> {
        self.confirm_as(resolve::By::Call(self.calling))
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        if !valid_id(id) {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.installed(id)?;
        let mut state = self.state();
        if id == self.running() || id == state.default || id == state.trial {
            return Err(Status::BUSY);
        }
        if Meta::load(&self.layout, id).pinned() {
            return Err(Status::NOT_PERMITTED);
        }
        // Never the only slot that can boot.
        if !self.layout.slots().iter().any(|other| other != id) {
            return Err(Status::BUSY);
        }
        self.remove_slot(&mut state, id)
    }

    fn pin(&mut self, id: &str, pinned: bool) -> Result<()> {
        self.installed(id)?;
        let mut meta = Meta::load(&self.layout, id);
        meta.set("PINNED", if pinned { "yes" } else { "no" });
        meta.save(&self.layout, id).map_err(|_| Status::IO)
    }

    fn set_keep(&mut self, keep: u32) -> Result<()> {
        store::set_keep(&self.layout, keep).map_err(|_| Status::IO)
    }
}

/// Restarts the board: `systemctl reboot`.
///
/// Settings: `systemctl`, the program to run, `"systemctl"` by default.
#[driver("system-reboot", provides = [Reboot])]
pub struct SystemReboot {
    restarter: Restarter,
    /// The state directory's parent: the data partition, where the boot history reads the request.
    data: PathBuf,
    calling: CallerId,
}

impl Lifecycle for SystemReboot {
    type Needs = SystemRebootNeeds;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(REBOOT_KEYS)
    }

    fn start(_: SystemRebootNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (layout, ubi_write) = layout_of(cfg);
        let base = record_for(cfg, &layout, &ubi_write, None)?;
        let check = guard_check(layout, base);
        Ok(Self {
            restarter: restarter(cfg, Some(check)),
            data: PathBuf::from(cfg.str("data").unwrap_or("/data")),
            calling: CallerId::LOCAL,
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
        self.restarter.run(None).map_err(|_| Status::IO)
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
        if boots::append(
            &self.layout,
            &self.boot,
            &slot,
            outcome,
            &reset,
            &requested,
            self.boots_keep,
        )
        .is_ok()
        {
            let _ = fs::remove_file(self.layout.marker(boot::REQUEST_FILE));
        }
    }
}

impl SystemSlots {
    /// The slow-period boot-file check: at once, then every ten minutes. It repoints the default
    /// when the default's files no longer match, and otherwise only reports.
    fn slow_check(&mut self, now_ns: u64) -> Option<u64> {
        const PERIOD_NS: u64 = 600 * 1_000_000_000;
        let check = self.check.clone()?;
        if now_ns >= self.next_check_ns {
            self.next_check_ns = now_ns.saturating_add(PERIOD_NS);
            match (check.0)(false) {
                Ok(Some(note)) => self.note(note),
                Ok(None) => {}
                Err(why) => self.note(why),
            }
        }
        Some(self.next_check_ns)
    }

    /// Writes `text` as the state's error (what `info` and `list` show).
    fn note(&self, text: String) {
        let mut state = self.state();
        state.error = text;
        let _ = self.save(&state);
    }

    /// Refuses a restart whose boot files don't match (see [`boot_guard`]); the reason is the
    /// state's error.
    fn restart_check(&self) -> Result<()> {
        let Some(check) = &self.check else {
            return Ok(());
        };
        (check.0)(true).map(drop).map_err(|why| {
            self.note(why);
            Status::IO
        })
    }
}
