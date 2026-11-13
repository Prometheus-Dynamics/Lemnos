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
//!   `packaging/uboot`; see [`uboot`]) or `"file"` (one `boot-record` file, for a loader with
//!   one fixed kernel; see [`boot`]). `docs/slots.md` describes each.
//! - `systemctl`: the program that restarts the board, `"systemctl"` by default; it is run as
//!   `systemctl reboot`, with `--reboot-argument=0 tryboot` for a trial.
//! - `keys`: a file of the keys the board trusts, one per line: 64 hex digits, a space, and the
//!   key's owner. A bundle's `slot.sig`, when it has one, is always checked against them, and
//!   `list` shows each slot as `trusted` (with the owner as `signer`), `untrusted` or `unsigned`.
//! - `require_signed`: `true` refuses every bundle not signed by a trusted key. Off by default:
//!   such a bundle installs, and shows as `untrusted` or `unsigned`.
//! - `boot_args`: the name the slot's initramfs reads its kernel arguments under: a slot's
//!   command line gets `<boot_args>.slot=<id> <boot_args>.roothash=<hex>
//!   <boot_args>.data=PARTUUID=<uuid> panic=5`. Required for an install.
//! - `partuuid`: the data partition's PARTUUID, which a slot's kernel command line names. Found
//!   from the mount table and `/dev/disk/by-partuuid` when not given.
//! - `reserve_mb`: free space an install leaves on the data partition, 2048 by default.
//! - `keep`: how many slots housekeeping keeps when the board has no setting of its own, 3 by
//!   default.
//! - `running`: the running slot, for a test; read from `board/slots/booted` otherwise.
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
mod install;
pub mod json;
pub mod store;
pub mod tar;
pub mod uboot;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_contract_system_reboot::Reboot;
use lemnos_contract_system_slots::Slots;
use lemnos_core::{CallerId, Config, JobId, JobState, Lifecycle, Reply, Result, Status, driver};

use boot::{BootRecord, FileRecord, Restarter, Tryboot};
use install::{Failure, Finish, Guard, INCOMING, Install, NO_SPACE};
pub use install::{SIGNATURE_LEN, Signed, TrustedKey, parse_keys};
use json::quoted;
use store::{DEFAULT_KEEP, Layout, Meta, State, read_line, valid_id};
use uboot::Uboot;

#[driver("system-slots", provides = [Slots])]
pub struct SystemSlots {
    layout: Layout,
    record: Box<dyn BootRecord>,
    keys: Vec<TrustedKey>,
    require_signed: bool,
    partuuid: String,
    boot_args: String,
    running: Option<String>,
    reserve: u64,
    keep: u32,
    calling: CallerId,
    install: Option<Install>,
    next_job: u64,
    /// The buffer an install from a file reads through, kept between ticks.
    read_buf: Vec<u8>,
}

fn restarter(cfg: &Config<'_>) -> Restarter {
    Restarter {
        systemctl: PathBuf::from(cfg.str("systemctl").unwrap_or("systemctl")),
    }
}

impl SystemSlots {
    /// The driver over a layout and a boot record of the caller's choosing.
    pub fn new(layout: Layout, record: Box<dyn BootRecord>, keys: Vec<TrustedKey>) -> Self {
        let mut slots = Self {
            layout,
            record,
            keys,
            require_signed: false,
            partuuid: String::new(),
            boot_args: String::new(),
            running: None,
            reserve: 2048 << 20,
            keep: DEFAULT_KEEP,
            calling: CallerId::LOCAL,
            install: None,
            next_job: 0,
            read_buf: Vec::new(),
        };
        slots.clear_interrupted();
        // What a sender left in `.incoming` belongs to no install now.
        let _ = install::reset_incoming(&slots.layout);
        slots
    }

    /// An install the service's restart cut short: its work goes, and the board's state says so.
    fn clear_interrupted(&mut self) {
        if install::clear_leftovers(&self.layout) {
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

    fn fail_install(&self, install: &mut Install, failure: Failure) -> Status {
        let status = failure.0;
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
    fn feed(&self, install: &mut Install, offset: u64, bytes: &[u8]) -> Result<()> {
        if let Err(failure) = install.take(offset, bytes) {
            // A write out of order changes nothing: the install goes on from where it is.
            if offset != install.got {
                return Err(failure.0);
            }
            return Err(self.fail_install(install, failure));
        }
        if !install.complete() {
            return Ok(());
        }
        let mut state = self.state();
        let finish = Finish {
            layout: &self.layout,
            keys: &self.keys,
            require_signed: self.require_signed,
            partuuid: &self.partuuid,
            boot_args: &self.boot_args,
            guard: Guard {
                running: self.running(),
                default: state.default.clone(),
            },
            record: &*self.record,
        };
        match install.finish(&finish) {
            Ok(id) => {
                self.mark_keep_old(&id, install.keep_old)?;
                state.staged = id.clone();
                state.state = "staged".into();
                state.error.clear();
                state.progress = 1000;
                self.save(&state)?;
                install.drop_source();
                install.end = Some(Ok(id));
                Ok(())
            }
            Err(failure) => Err(self.fail_install(install, failure)),
        }
    }

    /// Whether a bundle of `size` bytes fits with the reserve; the reason in the state when not.
    fn room(&self, size: u64) -> Result<()> {
        fs::create_dir_all(self.layout.images()).map_err(|_| Status::IO)?;
        let free =
            lemnos_linux_sys::statfs::free_bytes(&self.layout.images()).map_err(|_| Status::IO)?;
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
        let mut install =
            Install::start(&self.layout, self.calling, job, size, pin, keep_old, guard)
                .map_err(|failure| failure.0)?;
        install.source = source;
        state.state = "staging".into();
        state.error.clear();
        state.progress = 0;
        state.started_by = "lemnos".into();
        self.save(&state)?;
        self.install = Some(install);
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

    fn list_json(&self) -> String {
        let state = self.state();
        let running = self.running();
        let mut out = String::from("{");
        for (key, value) in [
            ("slot_active", &running),
            ("slot_default", &state.default),
            ("slot_previous", &state.previous),
            ("slot_trial", &state.trial),
            ("slot_staged", &state.staged),
            ("state", &state.state),
            ("error", &state.error),
        ] {
            quoted(key, &mut out);
            out.push(':');
            quoted(value, &mut out);
            out.push(',');
        }
        let free = lemnos_linux_sys::statfs::free_bytes(&self.layout.data).unwrap_or(0);
        out.push_str(&format!(
            "\"free_bytes\":{free},\"reserve_bytes\":{},\"keep\":{},\"slots\":[",
            self.reserve,
            store::keep_count(&self.layout, self.keep)
        ));
        for (at, id) in self.by_age().iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
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
            let size = fs::metadata(self.layout.image(id)).map_or(0, |m| m.len());
            let want = manifest
                .as_ref()
                .and_then(|m| m.path("image.size"))
                .and_then(json::Json::u64);
            let mut flags = Vec::new();
            for (flag, slot) in [
                ("running", &running),
                ("default", &state.default),
                ("previous", &state.previous),
                ("trial", &state.trial),
                ("staged", &state.staged),
            ] {
                if slot == id {
                    flags.push(format!("\"{flag}\""));
                }
            }
            out.push('{');
            for (key, value) in [
                ("id", id.clone()),
                ("family", text("family")),
                ("version", text("version")),
                ("variant", text("variant")),
                ("kernel", text("kernel")),
                ("display_name", text("display_name")),
                ("good", meta.get("GOOD").to_string()),
                ("signed", signed_word(&meta)),
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
                "\"size\":{size},\"verity\":{},\"intact\":{},\"flags\":[{}],\"pinned\":{},\"failures\":{},\"failed\":{}}}",
                !text("verity.root_hash").is_empty(),
                want == Some(size),
                flags.join(","),
                meta.pinned(),
                meta.get("FAILURES").parse::<u32>().unwrap_or(0),
                meta.failed(),
            ));
        }
        out.push_str("]}");
        out
    }
}

/// How a slot was signed, as `list` shows it. A slot from before the board recorded it is
/// `unsigned`.
fn signed_word(meta: &Meta) -> String {
    match meta.get("SIGNED") {
        "" => "unsigned".to_string(),
        word => word.to_string(),
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
    const TRACKS_CALLERS: bool = true;
    const JOBS: bool = true;
    const TICKS: bool = true;

    /// An install from a file reads a piece each tick, at once again while there is more.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.pull();
        self.install
            .as_ref()
            .filter(|install| install.end.is_none() && install.source.is_some())
            .map(|_| now_ns)
    }

    fn start(_: SystemSlotsNeeds, cfg: &Config<'_>) -> Result<Self> {
        let layout = Layout {
            data: PathBuf::from(cfg.str("data").unwrap_or("/data")),
            boot: PathBuf::from(cfg.str("boot").unwrap_or("/boot/firmware")),
        };
        let record: Box<dyn BootRecord> = match cfg.str("record").unwrap_or("tryboot") {
            "tryboot" => Box::new(Tryboot {
                boot: layout.boot.clone(),
                restarter: restarter(cfg),
            }),
            "file" => Box::new(FileRecord {
                boot: layout.boot.clone(),
                restarter: restarter(cfg),
            }),
            "uboot" => Box::new(Uboot {
                boot: layout.boot.clone(),
                restarter: restarter(cfg),
            }),
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        // A key file that can't be read leaves the board trusting no bundle, which every
        // install then says; it is read again when the driver is bound again.
        let keys = cfg
            .str("keys")
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| parse_keys(&text).ok())
            .unwrap_or_default();
        let mut slots = Self::new(layout, record, keys);
        slots.partuuid = cfg
            .str("partuuid")
            .map(str::to_string)
            .or_else(|| find_partuuid(&slots.layout.data))
            .unwrap_or_default();
        slots.running = cfg.str("running").map(str::to_string);
        slots.boot_args = cfg.str("boot_args").unwrap_or("").to_string();
        slots.require_signed = cfg.bool("require_signed").unwrap_or(false);
        if let Some(mb) = cfg.int("reserve_mb") {
            slots.reserve = u64::try_from(mb).map_err(|_| Status::INVALID_ARGUMENT)? << 20;
        }
        if let Some(keep) = cfg.int("keep") {
            slots.keep = u32::try_from(keep).map_err(|_| Status::INVALID_ARGUMENT)?;
        }
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

    fn install(&mut self, size: u64, pin: bool, keep_old: bool) -> Result<JobId> {
        self.begin(size, pin, keep_old, None)
    }

    fn accept(&mut self, size: u64) -> Result<()> {
        if self.install.is_some() {
            return Err(Status::BUSY);
        }
        self.room(size)
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
        if id == self.running() {
            return Err(Status::BUSY);
        }
        let mut meta = Meta::load(&self.layout, id);
        if meta.failed() && !force {
            return Err(Status::NOT_PERMITTED);
        }
        let mut state = self.state();
        // Room for its boot files first: only the resident slots' stay.
        self.evict(&state, id)?;
        boot::stage(&self.layout, id, false).map_err(|_| Status::IO)?;
        self.mark_keep_old(id, keep_old)?;
        self.record.set_trial(id).map_err(|_| Status::IO)?;
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
        self.record.restart(true).map_err(|_| Status::IO)
    }

    fn confirm(&mut self) -> Result<()> {
        let running = self.running();
        if !valid_id(&running) {
            return Err(Status::NOT_FOUND);
        }
        let mut state = self.state();
        let mut housekeep = false;
        if !state.trial.is_empty() && running == state.trial {
            let keeps = read_line(&self.layout.marker("trial-keeps-default"));
            if keeps == running {
                // A trial only: it booted well, and the default stays for the next restart.
                state.trial.clear();
                state.staged.clear();
            } else {
                self.record
                    .fallback_copy(&self.layout, &running)
                    .map_err(|_| Status::IO)?;
                self.record.set_default(&running).map_err(|_| Status::IO)?;
                if state.default != running {
                    state.previous = core::mem::replace(&mut state.default, running.clone());
                }
                state.trial.clear();
                state.staged.clear();
                housekeep = true;
            }
            state.state = "confirmed".into();
            state.error.clear();
        } else if !state.trial.is_empty() {
            // The trial didn't come up as the running slot: the board went back.
            let trial = core::mem::take(&mut state.trial);
            let reason = format!("slot {trial} didn't confirm its boot; back on {running}");
            let mut meta = Meta::load(&self.layout, &trial);
            meta.set("GOOD", "no");
            if !meta.failed() {
                let failures = meta.get("FAILURES").parse::<u32>().unwrap_or(0) + 1;
                meta.set("FAILED", "yes");
                meta.set("FAILURES", &failures.to_string());
                meta.set("FAIL_REASON", &reason);
                meta.set("FAIL_TIME", &store::now_utc());
            }
            if self.layout.installed(&trial) {
                meta.save(&self.layout, &trial).map_err(|_| Status::IO)?;
            }
            state.state = "rolled-back".into();
            state.error = reason;
        } else if state.state == "rebooting" {
            state.state = "rolled-back".into();
        }
        if state.default.is_empty() {
            state.default = running.clone();
        }
        let mut meta = Meta::load(&self.layout, &running);
        let now = store::now_utc();
        meta.set("GOOD", "yes");
        meta.set("FAILED", "no");
        meta.set("CONFIRM_TIME", &now);
        meta.set("LAST_BOOT", &now);
        if self.layout.installed(&running) {
            meta.save(&self.layout, &running).map_err(|_| Status::IO)?;
        }
        let _ = store::replace(&self.layout.marker("fallbacks"), b"0\n");
        for marker in ["trial-started", "trial-init", "trial-keeps-default"] {
            let _ = fs::remove_file(self.layout.marker(marker));
        }
        self.save(&state)?;
        if housekeep {
            let keep_old = read_line(&self.layout.marker("keep-old"));
            if keep_old != running {
                self.housekeep(&mut state)?;
            }
            let _ = fs::remove_file(self.layout.marker("keep-old"));
        }
        // Boot directories of slots no longer resident come off. Never fails the confirm.
        let _ = self.evict(&state, "");
        Ok(())
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
}

impl Lifecycle for SystemReboot {
    type Needs = SystemRebootNeeds;

    fn start(_: SystemRebootNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            restarter: restarter(cfg),
        })
    }
}

impl Reboot for SystemReboot {
    fn reboot(&mut self) -> Result<()> {
        self.restarter.run(None).map_err(|_| Status::IO)
    }
}
