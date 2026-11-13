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
//!    command line's, the image isn't `image.size` bytes, or `kernel` names another kernel.
//! 6. Load the device-mapper modules, open the image through dm-verity, mount it read-only.
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

use lemnos_slot_store::manifest::Manifest;
use lemnos_slot_store::records::{file, tryboot, uboot};
use lemnos_slot_store::store::{self, Layout, Meta, State, read_line, valid_id};

use crate::cmdline::Args;
use crate::decide::{Candidate, Failure, Next, after_failure};
use crate::ops::Ops;
use crate::partition::{self, Part, Spec};

/// The program the slot's own system starts with.
pub const INIT: &str = "/sbin/init";
/// The modules dm-verity needs, in load order. Built-in ones fail to load, which is fine.
const MODULES: [&str; 3] = ["dm-mod", "dm-bufio", "dm-verity"];
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

/// Which boot record the boot partition holds: what a fallback writes its trial into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record {
    Tryboot,
    File,
    Uboot,
}

impl Record {
    fn on(boot: &Path) -> Self {
        if boot.join(uboot::RECORD_FILE).is_file() {
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
        layout: Layout {
            data: env.at("data"),
            boot: env.at("boot"),
        },
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

    fn boot_dev(&self) -> Option<PathBuf> {
        partition::boot_beside(&self.env.sys, &self.env.dev, self.data.as_ref()?)
    }

    /// Mounts the boot partition at `boot`, read-write or not.
    fn mount_boot(&self, writable: bool) -> Result<(), String> {
        if self.boot_mounted.get() {
            self.unmount_boot();
        }
        let dev = self
            .boot_dev()
            .ok_or("no partition 1 beside the data partition")?;
        let opts = if writable { "rw" } else { "ro" };
        self.ops
            .mount("vfat", opts, &dev, &self.layout.boot)
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
                let record = Record::on(&self.layout.boot);
                if let Err(error) = self.write_trial(record, &id) {
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
                self.restart((record == Record::Tryboot).then_some(tryboot::RESTART_ARG));
                why.to_string()
            }
        }
    }

    /// Points the boot partition's record at `id` for one trial boot.
    fn write_trial(&self, record: Record, id: &str) -> std::io::Result<()> {
        let boot = &self.layout.boot;
        match record {
            Record::Tryboot => store::replace(
                &boot.join(tryboot::TRIAL_FILE),
                tryboot::entry(id).as_bytes(),
            ),
            Record::File => {
                let (default, _) =
                    file::parse(&fs::read_to_string(boot.join(file::FILE)).unwrap_or_default());
                store::replace(&boot.join(file::FILE), file::text(&default, id).as_bytes())
            }
            Record::Uboot => {
                let path = boot.join(uboot::RECORD_FILE);
                let mut record = uboot::Record::parse(&fs::read_to_string(&path)?);
                record.trial = id.to_string();
                record.armed = true;
                store::replace(&path, record.text().as_bytes())
            }
        }
    }

    /// The data partition, by `spec`, else the one labelled `data`, for up to `wait`: the card
    /// or eMMC can take a moment to show up.
    fn find_data(&self, spec: &str) -> Result<Part, String> {
        let spec = Spec::parse(spec)?;
        let label = Spec::Label("data".into());
        let tries = (self.env.wait.as_millis() / 100).max(1);
        let mut last = String::new();
        for _ in 0..tries {
            match partition::find(&spec, &self.env.sys, &self.env.dev) {
                Ok(part) => return Ok(part),
                Err(error) => last = error,
            }
            if let Ok(part) = partition::find(&label, &self.env.sys, &self.env.dev) {
                self.log(&format!(
                    "nothing answers to the data partition on the command line; using {}, labelled data",
                    part.name
                ));
                return Ok(part);
            }
            self.ops.sleep(Duration::from_millis(100));
        }
        Err(last)
    }

    /// With a `file` record: the slot it picks, whether that is a trial, and its root hash.
    /// The trial is cleared on the boot partition before it starts.
    fn file_record(&self, args: &Args) -> Option<(String, bool, String)> {
        let path = self.layout.boot.join(file::FILE);
        let (default, trial) = file::parse(&fs::read_to_string(&path).ok()?);
        let (slot, is_trial) = if trial.is_empty() {
            (
                if default.is_empty() {
                    args.slot.clone()
                } else {
                    default.clone()
                },
                false,
            )
        } else {
            (trial, true)
        };
        let roothash = if slot == args.slot {
            args.roothash.clone()
        } else {
            let cmdline = self.layout.boot_dir(&slot).join("cmdline.txt");
            args.roothash_in(&fs::read_to_string(cmdline).unwrap_or_default())
        };
        if is_trial {
            let cleared = self.mount_boot(true).and_then(|()| {
                store::replace(&path, file::text(&default, "").as_bytes())
                    .map_err(|e| e.to_string())
            });
            if let Err(error) = cleared {
                self.log(&format!(
                    "couldn't clear the trial in {}: {error}",
                    file::FILE
                ));
            }
        }
        Some((slot, is_trial, roothash))
    }

    fn run(&mut self, cmdline: &str) -> String {
        let args = match Args::parse(cmdline) {
            Ok(args) => args,
            Err(error) => return self.rescue(&error),
        };
        self.slot = args.slot.clone();
        let data = match self.find_data(&args.data) {
            Ok(part) => part,
            Err(seen) => {
                self.log(&seen);
                return self.rescue(&format!("no data partition {}", args.data));
            }
        };
        let data_dev = self.env.dev.join(&data.name);
        self.data = Some(data);
        let mut opts = "noatime,errors=remount-ro";
        if partition::head(&data_dev).is_some_and(|head| partition::ext_has_errors(&head)) {
            self.log("the data partition's filesystem records errors; mounting it read-only: repair it off the board");
            opts = "ro,noatime";
        }
        if let Err(error) = self.ops.mount("ext4", opts, &data_dev, &self.layout.data) {
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
        let image = self.layout.image(&slot);
        let size = fs::metadata(&image).map(|m| m.len());
        match size {
            Err(_) => return self.fail(&format!("no image {}", image.display())),
            Ok(size) if size != manifest.image_size => {
                return self.fail(&format!(
                    "the image is {size} bytes, its manifest says {}: damaged or cut short",
                    manifest.image_size
                ));
            }
            Ok(_) => {}
        }
        let release = fs::read_to_string(self.env.at("proc/sys/kernel/osrelease"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !manifest.kernel.is_empty() && manifest.kernel != release {
            return self.fail(&format!(
                "it was built for kernel {}, and this is {release}",
                manifest.kernel
            ));
        }
        for module in MODULES {
            if let Err(error) = self.ops.load_module(module, &release) {
                self.log(&format!("{module}: {error}"));
            }
        }
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
