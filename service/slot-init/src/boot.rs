//! The boot: from the kernel command line to `switch_root` into the slot, and what happens when
//! the slot can't come up.
//!
//! 1. Mount `/proc`, `/sys` and `/dev`, and read the slot arguments ([`crate::cmdline`]).
//! 2. Find the data partition ([`crate::partition`]), waiting for it a while, and mount it. An
//!    ext4 filesystem whose superblock records errors is mounted read-only: there is no fsck
//!    here, and the kernel replays the journal itself.
//! 3. Read the boot partition beside it, read-only. With a `boot-record` file there (the `file`
//!    record), the record picks the slot: its trial, which is cleared before it starts, else its
//!    default. The root hash is then the one on that slot's own `cmdline.txt`.
//! 4. Tell a trial: the firmware's tryboot flag, the `file` record's trial, or a slot that isn't
//!    the board's default. A trial writes `trial-init`.
//! 5. Read `slots/<id>.json` and refuse it when its id isn't the slot, its root hash isn't the
//!    command line's, the image isn't `image.size` bytes, or `kernel` names another kernel. Then
//!    every boot file of the slot (kernel, DTBs, overlays, `cmdline.txt`, initramfs) is read as
//!    it sits on the boot partition, raw ([`lemnos_slot_store::bootcheck`], no mount), and hashed
//!    against the manifest. A mismatch goes to [`Boot::fail`].
//! 6. Load the device-mapper modules, open the image through dm-verity, mount it read-only.
//!
//! The data partition is found by a fixed rule, not from the command line (the command line is
//! the bundle's, and it is the same on every board): the ext4 partition labelled [`DATA_LABEL`],
//! which the image builder sets, and exactly one must carry it. On raw NAND, when the UBI device
//! [`UBI_DEVICE`] is attached, the data and boot partitions are the UBIFS volumes `data` and
//! `boot` on that device, the image is the static volume `slot-<id>` opened through a `ubiblock`
//! device, and the U-Boot record is the volume `boot-record` ([`lemnos_slot_store::ubi`]).
//! 7. Move `/data` into it, write `booted` (and `trial-started` for a trial), and
//!    `switch_root` into `/sbin/init`.
//!
//! When any of that fails, [`Boot::fail`] keeps the kernel log, records the failure in the
//! slot's `.meta` (`FAILED`, `FAILURES`, `FAIL_REASON`) and asks [`crate::decide`] what next. It
//! never writes the default: the worst it does is start another slot once, on trial, so the
//! next plain restart is the default again.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_slot_store::bootcheck;
use lemnos_slot_store::manifest::Manifest;
use lemnos_slot_store::records::{
    KERNEL_GAP, MAX_ATTEMPTS, REASON_FILE, attempts, file, tryboot, uboot,
};
use lemnos_slot_store::signing::{self, BOOT_UNVERIFIED};
use lemnos_slot_store::store::{self, Layout, Meta, State, read_line, valid_id};
use lemnos_slot_store::ubi::{self, Ubi};

use crate::cmdline::Args;
use crate::decide::{
    Candidate, Failure, FilePick, Next, TryPass, after_failure, file_pick, tryboot_pass,
};
use crate::kernel::{self, Route};
use crate::ops::Ops;
use crate::partition::{self, Part};

/// The program the slot's own system starts with.
pub const INIT: &str = "/sbin/init";
/// The ext4 label of the data partition the image builder sets (it is not on the command line).
pub const DATA_LABEL: &str = "lemnos-data";
/// The UBI device of a raw NAND board, when attached: its volume `data` is the data partition.
pub const UBI_DEVICE: &str = "ubi0";
/// The kernel log kept on the boot partition after a failure.
pub const BOOT_LOG: &str = "slot-init.log";
/// The kernel log kept on the data partition after a failure.
pub const DATA_LOG: &str = "last-failure.log";

/// Where things are: `/` and its usual places on a board, a temporary directory in the tests.
#[derive(Debug, Clone)]
pub struct Env {
    pub root: PathBuf,
    pub sys: PathBuf,
    pub dev: PathBuf,
    /// How long to wait for the data partition to show up.
    pub wait: Duration,
    /// How long the reason stays on the console before a restart when no slot can start.
    pub rescue_wait: Duration,
}

impl Env {
    /// A board's.
    pub fn board() -> Self {
        Self {
            root: PathBuf::from("/"),
            sys: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
            wait: Duration::from_secs(10),
            rescue_wait: Duration::from_secs(30),
        }
    }

    pub fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

/// Which boot record the board holds: what a fallback writes its trial into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record {
    Tryboot,
    File,
    Uboot,
    /// The U-Boot record as a UBI volume, on raw NAND.
    UbiVolume,
}

impl Record {
    fn on(layout: &Layout) -> Self {
        let boot = &layout.boot;
        if layout
            .ubi
            .as_ref()
            .is_some_and(|ubi| ubi.volume(ubi::RECORD_VOLUME).is_some())
        {
            Self::UbiVolume
        } else if boot.join(uboot::RECORD_FILE).is_file() {
            Self::Uboot
        } else if boot.join(file::FILE).is_file() {
            Self::File
        } else {
            Self::Tryboot
        }
    }
}

struct Boot<'a> {
    ops: &'a dyn Ops,
    env: &'a Env,
    layout: Layout,
    slot: String,
    trial: bool,
    data: Option<Part>,
    data_mounted: Cell<bool>,
    data_writable: Cell<bool>,
    boot_mounted: Cell<bool>,
    log_saved: Cell<bool>,
}

/// Boots the slot. On a board it returns only when it couldn't (the reason, after a restart
/// that didn't happen); with a fake [`Ops`], `""` when it got to `switch_root`.
pub fn boot(ops: &dyn Ops, env: &Env) -> String {
    for dir in ["proc", "data", "boot", "newroot"] {
        let _ = fs::create_dir_all(env.at(dir));
    }
    let _ = fs::create_dir_all(&env.sys);
    let _ = fs::create_dir_all(&env.dev);
    for (fstype, target) in [
        ("proc", env.at("proc")),
        ("sysfs", env.sys.clone()),
        ("devtmpfs", env.dev.clone()),
    ] {
        if let Err(error) = ops.mount(fstype, "", Path::new(fstype), &target) {
            ops.log(&format!("{fstype} at {}: {error}", target.display()));
        }
    }
    let mut boot = Boot {
        ops,
        env,
        layout: Layout::new(env.at("data"), env.at("boot")),
        slot: String::new(),
        trial: false,
        data: None,
        data_mounted: Cell::new(false),
        data_writable: Cell::new(false),
        boot_mounted: Cell::new(false),
        log_saved: Cell::new(false),
    };
    let cmdline = fs::read_to_string(env.at("proc/cmdline")).unwrap_or_default();
    boot.run(&cmdline)
}

impl Boot<'_> {
    fn log(&self, message: &str) {
        self.ops.log(&format!("slot-init: {message}"));
    }

    /// The boot partition and its filesystem: partition 1 beside the data partition, or on
    /// raw NAND the UBIFS volume `boot`.
    fn boot_dev(&self) -> Option<(&'static str, PathBuf)> {
        if let Some(ubi) = &self.layout.ubi {
            return Some((
                "ubifs",
                PathBuf::from(format!("{}:{}", ubi.device, ubi::BOOT_VOLUME)),
            ));
        }
        partition::boot_beside(&self.env.sys, &self.env.dev, self.data.as_ref()?)
            .map(|dev| ("vfat", dev))
    }

    /// Mounts the boot partition at `boot`, read-write or not.
    fn mount_boot(&self, writable: bool) -> Result<(), String> {
        if self.boot_mounted.get() {
            self.unmount_boot();
        }
        let (fstype, dev) = self
            .boot_dev()
            .ok_or("no partition 1 beside the data partition")?;
        let opts = if writable { "rw" } else { "ro" };
        self.ops
            .mount(fstype, opts, &dev, &self.layout.boot)
            .map_err(|error| format!("{}: {error}", dev.display()))?;
        self.boot_mounted.set(true);
        Ok(())
    }

    fn unmount_boot(&self) {
        if self.boot_mounted.replace(false) {
            self.ops.sync();
            self.ops.umount(&self.layout.boot);
        }
    }

    /// Writes a marker under `board/slots`; a read-only data partition just logs it.
    fn mark(&self, name: &str, text: &str) {
        if !self.data_writable.get() {
            return;
        }
        let _ = fs::create_dir_all(self.layout.state_dir());
        if let Err(error) = store::replace(&self.layout.marker(name), text.as_bytes()) {
            self.log(&format!("couldn't write {name}: {error}"));
        }
    }

    /// The kernel log onto both partitions, once.
    fn save_log(&self) {
        if self.log_saved.replace(true) {
            return;
        }
        let text = self.ops.kernel_log();
        if self.data_mounted.get() && self.data_writable.get() {
            let path = self.layout.marker(DATA_LOG);
            let _ = fs::create_dir_all(self.layout.state_dir());
            match store::replace(&path, text.as_bytes()) {
                Ok(()) => self.log(&format!("kept the kernel log in {}", path.display())),
                Err(error) => self.log(&format!("couldn't keep the kernel log: {error}")),
            }
        }
        let mounted = self.boot_mounted.get() || self.mount_boot(true).is_ok();
        if mounted {
            match store::replace(&self.layout.boot.join(BOOT_LOG), text.as_bytes()) {
                Ok(()) => self.log(&format!(
                    "kept the kernel log in {BOOT_LOG} on the boot partition"
                )),
                Err(error) => self.log(&format!(
                    "couldn't keep the kernel log on the boot partition: {error}"
                )),
            }
        }
        self.ops.sync();
    }

    /// Unmounts both partitions, syncs, and restarts.
    fn restart(&self, argument: Option<&str>) {
        self.unmount_boot();
        self.ops.sync();
        if self.data_mounted.replace(false) {
            self.ops.umount(&self.layout.data);
        }
        self.ops.sync();
        self.ops.restart(argument);
    }

    /// Nothing better can start: say why, keep the log, wait so a console shows it, restart.
    fn rescue(&self, why: &str) -> String {
        self.log(&format!("no slot can start: {why}"));
        self.save_log();
        self.log(&format!(
            "restarting in {} s; the reason is in {BOOT_LOG} on the boot partition",
            self.env.rescue_wait.as_secs()
        ));
        self.ops.sleep(self.env.rescue_wait);
        self.restart(None);
        format!("no slot can start: {why}")
    }

    /// The slot didn't come up: keep the log, record it, and fall back as [`after_failure`]
    /// says.
    fn fail(&self, why: &str) -> String {
        let slot = self.slot.as_str();
        self.log(&format!("slot {slot} can't start: {why}"));
        if !self.data_mounted.get() || !valid_id(slot) {
            return self.rescue(why);
        }
        if self.data_writable.get() {
            let mut meta = Meta::load(&self.layout, slot);
            let failures = meta.get("FAILURES").parse::<u32>().unwrap_or(0) + 1;
            meta.set("FAILED", "yes");
            meta.set("FAILURES", &failures.to_string());
            meta.set("FAIL_REASON", why);
            if let Err(error) = meta.save(&self.layout, slot) {
                self.log(&format!("couldn't record the failure: {error}"));
            }
        }
        if let Err(error) = self.mount_boot(true) {
            return self.rescue(&format!("{why}; the boot partition: {error}"));
        }
        self.save_log();
        let mut state = State::load(&self.layout);
        let fallbacks = read_line(&self.layout.marker("fallbacks"))
            .parse()
            .unwrap_or(0);
        let slots: Vec<Candidate> = self
            .layout
            .slots()
            .into_iter()
            .map(|id| {
                let meta = Meta::load(&self.layout, &id);
                Candidate {
                    good: meta.get("GOOD") == "yes",
                    failed: meta.failed(),
                    resident: self.layout.boot_dir(&id).is_dir(),
                    signed: self.may_start(&meta),
                    seq: meta.seq(),
                    id,
                }
            })
            .collect();
        let next = after_failure(&Failure {
            slot,
            trial: self.trial,
            previous: &state.previous,
            fallbacks,
            slots: &slots,
        });
        match next {
            Next::Restart => {
                self.log("it was a trial; restarting into the default slot");
                self.restart(None);
                why.to_string()
            }
            Next::Wait(reason) => self.rescue(&format!("{why}; {reason}")),
            Next::Trial(id) => {
                let record = Record::on(&self.layout);
                if let Err(error) = self.write_trial(record, &id, self.trial_attempts()) {
                    return self.rescue(&format!("{why}; couldn't write the trial: {error}"));
                }
                self.mark("fallbacks", &format!("{}\n", fallbacks + 1));
                if self.data_writable.get() {
                    state.trial = id.clone();
                    state.error = format!("slot {slot} can't start: {why}");
                    let _ = fs::remove_file(self.layout.marker("trial-keeps-default"));
                    let _ = state.save(&self.layout);
                }
                self.log(&format!("falling back: restarting into slot {id} on trial"));
                // A plain restart: the loader takes the trial's attempt, as it does for any trial.
                self.restart(None);
                why.to_string()
            }
        }
    }

    /// The attempts a trial gets: the board's `trial_attempts`, as the driver wrote it.
    /// Whether the policy lets `meta`'s slot be a fallback: always, unless signing is required and
    /// its signature did not verify. (The default may still start: see `run`.)
    fn may_start(&self, meta: &Meta) -> bool {
        !signing::security_on(&self.layout)
            || signing::recorded(meta.get("SIGNATURE"), meta.get("SIGNED")) == signing::VERIFIED
    }

    fn trial_attempts(&self) -> u8 {
        store::setting(&self.layout, "TRIAL_ATTEMPTS")
            .and_then(|text| attempts(&text))
            .filter(|n| *n >= 1)
            .unwrap_or(MAX_ATTEMPTS)
    }

    /// Points the boot partition's record at `id` for a trial with `left` attempts, which the
    /// loader takes one of before the trial boots.
    fn write_trial(&self, record: Record, id: &str, left: u8) -> std::io::Result<()> {
        let boot = &self.layout.boot;
        match record {
            Record::Tryboot => {
                store::replace(
                    &boot.join(tryboot::ATTEMPTS_FILE),
                    format!("{left}\n").as_bytes(),
                )?;
                store::replace(
                    &boot.join(tryboot::TRIAL_FILE),
                    tryboot::entry(id).as_bytes(),
                )
            }
            Record::File => {
                let (default, _, _) =
                    file::parse(&fs::read_to_string(boot.join(file::FILE)).unwrap_or_default());
                store::replace(
                    &boot.join(file::FILE),
                    file::text(&default, id, left).as_bytes(),
                )
            }
            Record::Uboot => {
                let path = boot.join(uboot::RECORD_FILE);
                let mut record = uboot::Record::parse(&fs::read_to_string(&path)?);
                record.trial = id.to_string();
                record.attempts = left;
                store::replace(&path, record.text().as_bytes())
            }
            Record::UbiVolume => {
                let Some(device) = &self.layout.ubi else {
                    return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
                };
                let text = String::from_utf8_lossy(&device.read(ubi::RECORD_VOLUME)?).into_owned();
                let mut record = uboot::Record::parse(&text);
                record.trial = id.to_string();
                record.attempts = left;
                self.ops
                    .ubi_replace(device, ubi::RECORD_VOLUME, record.text().as_bytes())
                    .map_err(std::io::Error::other)
            }
        }
    }

    /// The data partition, by `spec`, else the one labelled `data`, for up to `wait`: the card
    /// or eMMC can take a moment to show up.
    fn find_data(&self) -> Result<Part, String> {
        let tries = (self.env.wait.as_millis() / 100).max(1);
        let mut last = String::new();
        for _ in 0..tries {
            let (mut hits, seen) = partition::labelled(DATA_LABEL, &self.env.sys, &self.env.dev);
            match hits.len() {
                1 => return Ok(hits.remove(0)),
                0 => {
                    last = format!(
                        "no partition is labelled {DATA_LABEL}; saw {}",
                        seen.join("; ")
                    );
                }
                n => {
                    // Two of them is a board that can't say which is its data: refused now, not waited on.
                    let names: Vec<&str> = hits.iter().map(|p| p.name.as_str()).collect();
                    return Err(format!(
                        "{n} partitions are labelled {DATA_LABEL} ({}): refusing to guess",
                        names.join(", ")
                    ));
                }
            }
            self.ops.sleep(Duration::from_millis(100));
        }
        Err(last)
    }

    /// The every-boot check: every boot file the manifest lists, read as it sits on the boot
    /// partition. Raw FAT on a board (no mount); on raw NAND, through the boot partition's
    /// read-only mount, since its UBIFS volume has no raw reader here.
    fn verify_boot_files(&self, slot: &str, manifest: &Manifest) -> Result<(), String> {
        let found = if self.layout.ubi.is_some() {
            self.mount_boot(false)?;
            let root = self.layout.boot.clone();
            let found = bootcheck::check(slot, &manifest.boot_files, |path| {
                match fs::read(root.join(path)) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            });
            self.unmount_boot();
            found
        } else {
            let (_, dev) = self
                .boot_dev()
                .ok_or("no partition 1 beside the data partition")?;
            bootcheck::check(slot, &manifest.boot_files, |path| {
                self.ops.read_boot_file(&dev, path)
            })
        };
        if found.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = found.iter().map(|m| m.to_string()).collect();
        Err(format!(
            "its boot files don't match its manifest: {}",
            list.join("; ")
        ))
    }

    /// On raw NAND: the UBI device once its volume `volume` shows up, for up to `wait`.
    fn find_volume(&self, device: &str, volume: &str) -> Result<Ubi, String> {
        let ubi = Ubi {
            sys: self.env.sys.clone(),
            dev: self.env.dev.clone(),
            device: device.to_string(),
        };
        let tries = (self.env.wait.as_millis() / 100).max(1);
        for _ in 0..tries {
            if ubi.volume(volume).is_some() {
                return Ok(ubi);
            }
            self.ops.sleep(Duration::from_millis(100));
        }
        Err(if ubi.present() {
            let names: Vec<String> = ubi.volumes().into_iter().map(|v| v.name).collect();
            format!(
                "{device} has no volume {volume}; it has {}",
                names.join(", ")
            )
        } else {
            format!("no UBI device {device}: is ubi.mtd= on the command line?")
        })
    }

    /// With a `file` record: the slot it picks, whether that is a trial, and its root hash. A
    /// trial takes one of its attempts here, written back before the trial starts; with none left
    /// the trial is cleared and the default starts.
    fn file_record(&self, args: &Args) -> Option<(String, bool, String)> {
        let path = self.layout.boot.join(file::FILE);
        let (default, trial, left) = file::parse(&fs::read_to_string(&path).ok()?);
        let pick = if self.held() {
            FilePick::Default { clear: false }
        } else {
            file_pick(&trial, left)
        };
        let (slot, is_trial) = match pick {
            FilePick::Trial { slot, left } => {
                self.write_file_record(&path, &default, &trial, left);
                (slot, true)
            }
            FilePick::Default { clear } => {
                if clear {
                    self.write_file_record(&path, &default, "", 0);
                }
                let slot = if default.is_empty() {
                    args.slot.clone()
                } else {
                    default.clone()
                };
                (slot, false)
            }
        };
        let roothash = if slot == args.slot {
            args.roothash.clone()
        } else {
            let cmdline = self.layout.boot_dir(&slot).join("cmdline.txt");
            args.roothash_in(&fs::read_to_string(cmdline).unwrap_or_default())
        };
        Some((slot, is_trial, roothash))
    }

    /// Writes the `file` record on the boot partition, mounted read-write for it.
    fn write_file_record(&self, path: &Path, default: &str, trial: &str, left: u8) {
        let written = self.mount_boot(true).and_then(|()| {
            store::replace(path, file::text(default, trial, left).as_bytes())
                .map_err(|e| e.to_string())
        });
        if let Err(error) = written {
            self.log(&format!("couldn't write {}: {error}", file::FILE));
        }
    }

    /// The tryboot pass: a pending trial with attempts left takes one, and the board restarts with
    /// the tryboot argument so the firmware boots the trial. Only the boot partition is mounted
    /// for it. Returns the reason when the boot is over here; `None` when the boot carries on.
    fn tryboot_pass(&self) -> Option<String> {
        if self.layout.ubi.is_some() {
            return None;
        }
        if self.mount_boot(true).is_err() {
            return None;
        }
        let boot = self.layout.boot.clone();
        let form_here = boot.join(uboot::RECORD_FILE).is_file() || boot.join(file::FILE).is_file();
        let mut over = None;
        // A trial held back for a kernel gap takes no attempt until the daemon resolves it.
        if !form_here && !self.held() {
            let trial_text = fs::read_to_string(boot.join(tryboot::TRIAL_FILE)).unwrap_or_default();
            let flag = fs::read(self.env.at(tryboot::TRIAL_FLAG)).unwrap_or_default();
            let counter = fs::read_to_string(boot.join(tryboot::ATTEMPTS_FILE))
                .ok()
                .and_then(|text| attempts(&text));
            match tryboot_pass(
                tryboot::slot(&trial_text).is_some(),
                flag == [0, 0, 0, 1],
                counter,
            ) {
                TryPass::Boot => {}
                TryPass::Spend { left } => {
                    let slot = tryboot::slot(&trial_text).unwrap_or_default().to_string();
                    match store::replace(
                        &boot.join(tryboot::ATTEMPTS_FILE),
                        format!("{left}\n").as_bytes(),
                    ) {
                        Ok(()) => {
                            self.log(&format!(
                                "trial {slot} takes an attempt, {left} left; restarting into it"
                            ));
                            self.restart(Some(tryboot::RESTART_ARG));
                            over = Some(format!("restarting into trial {slot}"));
                        }
                        Err(error) => self.log(&format!(
                            "couldn't take an attempt from the trial: {error}; booting the default"
                        )),
                    }
                }
                TryPass::Out => {
                    self.log("the trial has no attempts left; booting the default");
                    let _ = fs::remove_file(boot.join(tryboot::TRIAL_FILE));
                    let _ = fs::remove_file(boot.join(tryboot::ATTEMPTS_FILE));
                }
            }
        }
        self.unmount_boot();
        over
    }

    /// The kernel's release, from `/proc`.
    fn release(&self) -> String {
        fs::read_to_string(self.env.at("proc/sys/kernel/osrelease"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Whether sysfs shows a block device that is not a loop, RAM, UBI or device-mapper one: the
    /// storage the data partition is on.
    fn storage_present(&self) -> bool {
        fs::read_dir(self.env.sys.join("class/block")).is_ok_and(|dir| {
            dir.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                !["loop", "ram", "zram", "ubiblock", "dm-", "mtd"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
            })
        })
    }

    /// What the kernel offers, read now.
    fn facts(&self, route: Route) -> kernel::Facts {
        let read = |path: &str| fs::read_to_string(self.env.at(path)).unwrap_or_default();
        let misc = kernel::misc(&read("proc/misc"));
        let dm_targets = if misc.iter().any(|name| name == "device-mapper") {
            match self.ops.dm_targets() {
                Ok(targets) => Some(targets),
                Err(error) => {
                    self.log(&format!("device-mapper's targets: {error}"));
                    None
                }
            }
        } else {
            None
        };
        kernel::Facts {
            filesystems: kernel::filesystems(&read("proc/filesystems")),
            misc,
            drivers: kernel::drivers(&read("proc/devices")),
            crypto: kernel::crypto(&read("proc/crypto")),
            dm_targets,
            ubi: route == Route::Nand && self.layout.ubi.as_ref().is_some_and(|ubi| ubi.present()),
        }
    }

    /// The kernel check, before any attempt is taken or any record written. Every absent piece
    /// is named on the console. When a required one is absent the boot can't carry on, and the
    /// reason is returned: see [`Boot::kernel_gap`].
    fn check_kernel(&self, route: Route) -> Option<String> {
        let gaps = kernel::missing(route, &self.facts(route));
        for piece in &gaps {
            self.log(&piece.message());
        }
        let why: Vec<String> = gaps
            .iter()
            .filter(|piece| piece.required)
            .map(|piece| piece.message())
            .collect();
        if why.is_empty() {
            None
        } else {
            Some(self.kernel_gap(&why.join("; ")))
        }
    }

    /// The boot can't carry on for the kernel. Nothing is spent and no slot is failed: the kernel
    /// is the board's, and the slot isn't at fault. A trial's boot gives back the attempt the
    /// boot that started it took, and holds the trial (its reason is kept on the boot partition,
    /// where the daemon reads it). The default slot is opened through the same chain, so the
    /// boot waits and restarts as it does when nothing can start.
    fn kernel_gap(&self, why: &str) -> String {
        let Some(form) = self.trial_in_effect() else {
            return self.rescue(why);
        };
        self.hold_trial(form, why);
        self.save_log();
        self.log("the trial can't start on this kernel; restarting into the default slot");
        self.restart(None);
        why.to_string()
    }

    /// The trial this boot is, and which boot record holds it, when there is one: the firmware's
    /// tryboot flag, a `file` record's trial with attempts left, or a U-Boot or UBI record's trial
    /// with attempts.
    fn trial_in_effect(&self) -> Option<Record> {
        let flag = fs::read(self.env.at(tryboot::TRIAL_FLAG)).unwrap_or_default();
        if flag == [0, 0, 0, 1] {
            return Some(Record::Tryboot);
        }
        let mounted = self.mount_boot(false).is_ok();
        let form = Record::on(&self.layout);
        let trial = if !mounted {
            None
        } else {
            match form {
                Record::File => {
                    let text = fs::read_to_string(self.layout.boot.join(file::FILE));
                    let (_, trial, left) = file::parse(&text.unwrap_or_default());
                    (!trial.is_empty() && left > 0).then_some(Record::File)
                }
                Record::Uboot | Record::UbiVolume => self
                    .record_now(form)
                    .filter(|(trial, left)| !trial.is_empty() && *left > 0)
                    .map(|_| form),
                Record::Tryboot => None,
            }
        };
        self.unmount_boot();
        trial
    }

    /// The trial and attempts a U-Boot or UBI record holds now.
    fn record_now(&self, form: Record) -> Option<(String, u8)> {
        let text = match form {
            Record::Uboot => fs::read_to_string(self.layout.boot.join(uboot::RECORD_FILE)).ok()?,
            Record::UbiVolume => {
                let device = self.layout.ubi.as_ref()?;
                String::from_utf8_lossy(&device.read(ubi::RECORD_VOLUME).ok()?).into_owned()
            }
            Record::Tryboot | Record::File => return None,
        };
        let record = uboot::Record::parse(&text);
        Some((record.trial, record.attempts))
    }

    /// Gives back the attempt a trial's boot took and keeps the reason on the boot partition. A
    /// `tryboot` or `file` trial stays in its record, held. A U-Boot or UBI record is withdrawn
    /// instead (its trial cleared): the loader takes an attempt for every trial start, and can't
    /// be told to wait.
    fn hold_trial(&self, form: Record, why: &str) {
        if let Err(error) = self.mount_boot(true) {
            self.log(&format!("couldn't hold the trial: {error}"));
            return;
        }
        match form {
            Record::Tryboot => {
                let path = self.layout.boot.join(tryboot::ATTEMPTS_FILE);
                if let Some(left) = fs::read_to_string(&path).ok().and_then(|t| attempts(&t)) {
                    let left = (left + 1).min(self.trial_attempts());
                    let _ = store::replace(&path, format!("{left}\n").as_bytes());
                }
            }
            Record::File => {}
            Record::Uboot | Record::UbiVolume => {
                if let Some((_, left)) = self.record_now(form) {
                    let left = (left + 1).min(self.trial_attempts());
                    if let Err(error) = self.write_trial(form, "", left) {
                        self.log(&format!("couldn't withdraw the trial: {error}"));
                    }
                }
            }
        }
        if let Err(error) = store::replace(
            &self.layout.boot.join(REASON_FILE),
            format!("{why}\n").as_bytes(),
        ) {
            self.log(&format!("couldn't keep the reason: {error}"));
        }
    }

    /// Whether the boot partition holds a kernel-gap reason: a held trial takes no attempt.
    fn held(&self) -> bool {
        fs::read_to_string(self.layout.boot.join(REASON_FILE))
            .is_ok_and(|text| text.trim_start().starts_with(KERNEL_GAP))
    }

    fn run(&mut self, cmdline: &str) -> String {
        let args = match Args::parse(cmdline) {
            Ok(args) => args,
            Err(error) => return self.rescue(&error),
        };
        self.slot = args.slot.clone();
        let on_nand = Ubi {
            sys: self.env.sys.clone(),
            dev: self.env.dev.clone(),
            device: UBI_DEVICE.into(),
        }
        .present();
        let (fstype, opts, data_dev) = if on_nand {
            match self.find_volume(UBI_DEVICE, ubi::DATA_VOLUME) {
                Ok(ubi) => {
                    self.layout.ubi = Some(ubi);
                    if let Some(why) = self.check_kernel(Route::Nand) {
                        return why;
                    }
                    (
                        "ubifs",
                        "noatime",
                        PathBuf::from(format!("{UBI_DEVICE}:{}", ubi::DATA_VOLUME)),
                    )
                }
                Err(seen) => {
                    self.log(&seen);
                    return self
                        .rescue(&format!("no data volume {UBI_DEVICE}:{}", ubi::DATA_VOLUME));
                }
            }
        } else {
            let data = match self.find_data() {
                Ok(part) => part,
                Err(seen) => {
                    let seen = if self.storage_present() {
                        seen
                    } else {
                        kernel::storage_message()
                    };
                    self.log(&seen);
                    return self.rescue(&format!("no data partition: {seen}"));
                }
            };
            let data_dev = self.env.dev.join(&data.name);
            self.data = Some(data);
            if let Some(why) = self.check_kernel(Route::Stock) {
                return why;
            }
            if let Some(why) = self.tryboot_pass() {
                return why;
            }
            let mut opts = "noatime,errors=remount-ro";
            if partition::head(&data_dev).is_some_and(|head| partition::ext_has_errors(&head)) {
                self.log("the data partition's filesystem records errors; mounting it read-only: repair it off the board");
                opts = "ro,noatime";
            }
            ("ext4", opts, data_dev)
        };
        if let Err(error) = self.ops.mount(fstype, opts, &data_dev, &self.layout.data) {
            return self.rescue(&format!(
                "can't mount the data partition {}: {error}",
                data_dev.display()
            ));
        }
        self.data_mounted.set(true);
        self.data_writable.set(!opts.starts_with("ro"));

        let mut roothash = args.roothash.clone();
        let mut file_trial = false;
        match self.mount_boot(false) {
            Ok(()) => {
                if let Some((slot, trial, hash)) = self.file_record(&args) {
                    self.slot = slot;
                    file_trial = trial;
                    roothash = hash;
                }
                self.unmount_boot();
            }
            Err(error) => self.log(&format!("couldn't read the boot partition: {error}")),
        }
        if !valid_id(&self.slot) {
            let slot = core::mem::take(&mut self.slot);
            return self.rescue(&format!("{slot:?} isn't a slot id"));
        }
        let slot = self.slot.clone();
        let state = State::load(&self.layout);
        let flag = fs::read(self.env.at(tryboot::TRIAL_FLAG)).unwrap_or_default();
        self.trial = flag == [0, 0, 0, 1]
            || file_trial
            || (!state.default.is_empty() && slot != state.default);
        if self.trial {
            self.mark("trial-init", &format!("{slot}\n"));
        }

        // The switch's policy, as the boot side applies it. An unverified trial is refused (the
        // default starts instead). The default always starts: a board must come up. When it is
        // unverified the start is recorded, so the driver reports it.
        let meta = Meta::load(&self.layout, &slot);
        let verified =
            signing::recorded(meta.get("SIGNATURE"), meta.get("SIGNED")) == signing::VERIFIED;
        if signing::security_on(&self.layout) && !verified {
            if self.trial {
                return self.fail(&format!(
                    "{slot} is not signed by a key this board trusts, and this board takes only those"
                ));
            }
            self.log(&format!("starting the default slot {slot} although it is not signed by a key this board trusts"));
            let _ = store::replace(
                &self.layout.marker(BOOT_UNVERIFIED),
                format!("{slot}\n").as_bytes(),
            );
        } else {
            let _ = fs::remove_file(self.layout.marker(BOOT_UNVERIFIED));
        }
        let manifest_path = self.layout.manifest(&slot);
        let manifest = match fs::read_to_string(&manifest_path) {
            Ok(text) => match Manifest::parse(&text) {
                Ok(manifest) => manifest,
                Err(error) => return self.fail(&format!("its manifest: {error}")),
            },
            Err(_) => return self.fail(&format!("no manifest {}", manifest_path.display())),
        };
        if manifest.id != slot {
            return self.fail(&format!("its manifest is for slot {}", manifest.id));
        }
        if roothash.is_empty() {
            return self.fail(&format!("no {}.roothash on its command line", args.prefix));
        }
        if manifest.verity.root_hash != roothash {
            return self.fail("its manifest's root hash isn't the one on its command line");
        }
        let image = match &self.layout.ubi {
            Some(ubi) => PathBuf::from(format!("{}:{}", ubi.device, ubi::image_volume(&slot))),
            None => self.layout.image(&slot),
        };
        match self.layout.image_size(&slot) {
            None => return self.fail(&format!("no image {}", image.display())),
            Some(size) if size != manifest.image_size => {
                return self.fail(&format!(
                    "the image is {size} bytes, its manifest says {}: damaged or cut short",
                    manifest.image_size
                ));
            }
            Some(_) => {}
        }
        if let Err(why) = self.verify_boot_files(&slot, &manifest) {
            return self.fail(&why);
        }
        let release = self.release();
        if !manifest.kernel.is_empty() && manifest.kernel != release {
            return self.fail(&format!(
                "it was built for kernel {}, and this is {release}",
                manifest.kernel
            ));
        }
        let image = match &self.layout.ubi {
            Some(ubi) => match self.ops.ubi_block(ubi, &ubi::image_volume(&slot)) {
                Ok(node) => node,
                Err(error) => {
                    return self.fail(&format!(
                        "no block device over {}: {error}",
                        image.display()
                    ));
                }
            },
            None => image,
        };
        let dm = match self.ops.open_verity(&image, "slot", &manifest.verity) {
            Ok(dm) => dm,
            Err(error) => return self.fail(&format!("dm-verity refused the image: {error}")),
        };
        let new_root = self.env.at("newroot");
        if let Err(error) = self.ops.mount("erofs", "ro", Path::new(&dm), &new_root) {
            return self.fail(&format!("can't mount the image ({dm}): {error}"));
        }
        let moved_data = new_root.join("data");
        if let Err(error) = self.ops.move_mount(&self.layout.data, &moved_data) {
            self.ops.umount(&new_root);
            return self.fail(&format!(
                "can't move the data partition into the slot (no /data in the image?): {error}"
            ));
        }
        self.data_mounted.set(false);
        // From here the data partition is inside the new root.
        self.layout.data = moved_data;
        self.mark("booted", &format!("{slot}\n"));
        if self.trial {
            self.mark("trial-started", &format!("{slot}\n"));
        }
        self.log(&format!(
            "starting slot {slot} ({} {}{})",
            manifest.product,
            manifest.version,
            if self.trial { ", on trial" } else { "" }
        ));
        match self.ops.switch_root(&new_root) {
            Ok(()) => String::new(),
            Err(error) => self.rescue(&format!("switch_root: {error}")),
        }
    }
}

#[cfg(test)]
mod tests;
