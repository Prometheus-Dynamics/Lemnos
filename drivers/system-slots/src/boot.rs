//! The boot side: which slot the firmware starts, and each slot's files on the boot partition.
//!
//! What tells the boot loader which slot to start is behind [`BootRecord`]. [`Tryboot`] is the
//! Raspberry Pi firmware's form, which the board's boot chain uses: `config.txt` includes
//! `default.txt` and `tryboot.txt` includes `trial.txt`, each one line `os_prefix=slots/<id>/`,
//! and a restart with the argument `0 tryboot` reads `tryboot.txt` once. [`FileRecord`] is one
//! file, `boot-record`, with a `default=` and a `trial=` line, for a boot loader that reads it.
//!
//! A slot's boot directory (`slots/<id>/` on the boot partition) is kept on the data partition
//! too, by content ([`put`]). Only the slots that may be booted next keep a directory on the
//! boot partition: the running, default, previous and trial ones ([`evict`]). A directory goes
//! onto the boot partition ([`stage`]) before a pointer names it, and others come off only after
//! the pointers are written, so a power cut at any point leaves the old pointers with their
//! directories or the new ones with theirs.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use lemnos_slot_store::boot_raw::{BootRaw, Pointer};
use lemnos_slot_store::records;

use crate::store::{Layout, replace, sync_dir, valid_id};

/// Tells the boot loader which slot to start.
///
/// A trial is a slot the loader starts for the boots its attempts allow ([`BootRecord::set_trial`]).
/// Each trial boot takes one attempt before the trial starts (the loader does it: the script, or
/// `lemnos-slot-init` for the other forms), so a boot that never reaches the daemon uses one up.
pub trait BootRecord: Send {
    /// The slot every ordinary boot starts.
    fn set_default(&mut self, id: &str) -> io::Result<()>;
    /// Makes `id` the trial, with `attempts` boots allowed to start it, from the next boot.
    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()>;
    /// The pending trial and its attempts left, `None` when the loader holds no trial.
    fn pending(&self) -> io::Result<Option<(String, u8)>>;
    /// Sets the pending trial's attempts left (nothing when none is pending).
    fn set_attempts(&mut self, attempts: u8) -> io::Result<()>;
    /// Takes the trial out: the next boot is the default's.
    fn clear_trial(&mut self) -> io::Result<()>;
    /// Restarts the board, into whatever the loader starts first.
    fn restart(&mut self) -> io::Result<()>;
    /// Adds what this boot loader needs to a slot's boot directory `dir` at install, before it
    /// is kept: `cmdline` is the slot's kernel command line.
    fn prepare(&self, _dir: &Path, _id: &str, _cmdline: &str) -> io::Result<()> {
        Ok(())
    }
    /// Keeps the default slot's boot files where the boot loader looks when it can't read the
    /// record: at the boot partition's root.
    fn fallback_copy(&self, layout: &Layout, id: &str) -> io::Result<()> {
        root_copy(layout, id)
    }
}

/// The boot partition's copy of a trial's reason: the pre-created reason record (`reason.rec`),
/// one raw sector write on the partition's node. The driver writes it when the state directory
/// can't hold the reason at a deadline restart, and reads it at the next start, when the state has
/// no reason of its own. The loader never reads it, so it can't change what boots. Nothing mounts
/// the partition for it, and a missing record refuses the write by name.
pub fn write_reason(node: &Result<PathBuf, String>, reason: &str) -> io::Result<()> {
    let node = node.as_ref().map_err(|why| io::Error::other(why.clone()))?;
    let text = reason.replace(['\n', '\r'], " ");
    lemnos_slot_store::boot_raw::Boot::open(node)
        .and_then(|mut raw| raw.set_reason(&text))
        .map_err(io::Error::other)
}

/// The reason kept on the boot partition, if there is one.
pub fn read_reason(node: &Result<PathBuf, String>) -> Option<String> {
    let node = node.as_ref().ok()?;
    let mut raw = lemnos_slot_store::boot_raw::Boot::open(node).ok()?;
    raw.reason()
        .ok()
        .flatten()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// Forgets the reason kept on the boot partition. Never fails: a stale copy is only ever read
/// when the state has no reason, and it is cleared again at each resolution.
pub fn clear_reason(node: &Result<PathBuf, String>) {
    let _ = write_reason(node, "");
}

/// Runs the system's restart: `<systemctl> reboot`, with `--reboot-argument=0 tryboot` for a
/// trial when the firmware takes one.
/// The check lemnosd makes before its own restarts, and on a slow period (see
/// [`crate::boot_guard`]). Called with `true` before a restart: an `Err` refuses the restart. With
/// `false` on a slow period, its `Ok` carries a note of what it did.
#[derive(Clone)]
pub struct Check(pub Arc<dyn Fn(bool) -> Result<Option<String>, String> + Send + Sync>);

impl fmt::Debug for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Check")
    }
}

#[derive(Debug, Clone)]
pub struct Restarter {
    pub systemctl: PathBuf,
    /// Checked before the restart runs: a refused check means no restart.
    pub check: Option<Check>,
}

impl Restarter {
    pub fn run(&self, argument: Option<&str>) -> io::Result<()> {
        if let Some(check) = &self.check {
            (check.0)(true).map_err(io::Error::other)?;
        }
        let mut command = Command::new(&self.systemctl);
        command.arg("reboot");
        if let Some(argument) = argument {
            command.arg(format!("--reboot-argument={argument}"));
        }
        let status = command.status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} reboot: {status}",
                self.systemctl.display()
            )))
        }
    }
}

/// The Raspberry Pi firmware's `os_prefix` files. A trial is `trial.txt` with its attempts in
/// `trial-attempts` beside it: the firmware has no counter of its own, so the default slot's
/// `lemnos-slot-init` takes one from it before each trial boot (see `docs/slots.md`).
#[derive(Debug, Clone)]
pub struct Tryboot {
    /// Where the boot partition is mounted: its boot files are read there.
    pub boot: PathBuf,
    /// The boot partition's block node, where the pointers and the trial record are written, one
    /// raw sector at a time: found from the boot chain's source and the board's `boot_sources`
    /// (see `lemnos_slot_store::disk`). The reason it is not known is kept, and its writes are
    /// refused by that reason.
    pub node: Result<PathBuf, String>,
    pub restarter: Restarter,
}

impl Tryboot {
    /// The boot partition's raw records and pointers, opened on its node. Refused by name when the
    /// node is not known.
    fn raw(&self) -> io::Result<Box<dyn BootRaw>> {
        let node = self.node.as_ref().map_err(|why| {
            io::Error::other(format!(
                "the boot partition's device isn't known ({why}): its pointers can't be written"
            ))
        })?;
        lemnos_slot_store::boot_raw::Boot::open(node)
            .map(|boot| Box::new(boot) as Box<dyn BootRaw>)
            .map_err(io::Error::other)
    }
}

impl BootRecord for Tryboot {
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        self.raw()?
            .set_pointer(Pointer::Default, id)
            .map_err(io::Error::other)
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()> {
        // The record first, then the trial pointer: a trial never names a slot without its attempts.
        self.raw()?
            .arm_trial(id, attempts)
            .map_err(io::Error::other)
    }

    fn pending(&self) -> io::Result<Option<(String, u8)>> {
        let trial = self.raw()?.trial().map_err(io::Error::other)?;
        Ok(trial.map(|t| (t.slot, t.attempts)))
    }

    fn set_attempts(&mut self, attempts: u8) -> io::Result<()> {
        let mut raw = self.raw()?;
        if let Some(trial) = raw.trial().map_err(io::Error::other)? {
            raw.set_trial(&trial.slot, attempts)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn clear_trial(&mut self) -> io::Result<()> {
        self.raw()?.clear_trial().map_err(io::Error::other)
    }

    fn restart(&mut self) -> io::Result<()> {
        self.restarter.run(None)
    }

    /// The Pi firmware reads its slot from the pointer files; the boot partition's root holds no
    /// copy of the slot's files, so nothing is copied there (a copy of kernels is a file write this
    /// form does not make).
    fn fallback_copy(&self, _layout: &Layout, _id: &str) -> io::Result<()> {
        Ok(())
    }
}

/// One `boot-record` file on the boot partition: `default=<id>` and `trial=<id>` lines. The boot
/// loader that reads it clears the trial line when it starts that slot.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub boot: PathBuf,
    pub restarter: Restarter,
}

impl FileRecord {
    fn path(&self) -> PathBuf {
        self.boot.join(records::file::FILE)
    }

    fn write(&self, default: &str, trial: &str, attempts: u8) -> io::Result<()> {
        replace(
            &self.path(),
            records::file::text(default, trial, attempts).as_bytes(),
        )
    }

    /// The record as it reads: default, trial and the trial's attempts.
    fn read(&self) -> (String, String, u8) {
        records::file::parse(&fs::read_to_string(self.path()).unwrap_or_default())
    }
}

impl BootRecord for FileRecord {
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        let (_, trial, attempts) = self.read();
        self.write(id, &trial, attempts)
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()> {
        let (default, _, _) = self.read();
        self.write(&default, id, attempts)
    }

    fn pending(&self) -> io::Result<Option<(String, u8)>> {
        let (_, trial, attempts) = self.read();
        Ok((!trial.is_empty()).then_some((trial, attempts)))
    }

    fn set_attempts(&mut self, attempts: u8) -> io::Result<()> {
        let (default, trial, _) = self.read();
        if trial.is_empty() {
            return Ok(());
        }
        self.write(&default, &trial, attempts)
    }

    fn clear_trial(&mut self) -> io::Result<()> {
        let (default, _, _) = self.read();
        self.write(&default, "", 0)
    }

    fn restart(&mut self) -> io::Result<()> {
        self.restarter.run(None)
    }
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let got = io::Read::read(&mut file, &mut buf)?;
        if got == 0 {
            break;
        }
        hasher.update(&buf[..got]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every file under `dir`, as sorted relative paths.
fn files_under(dir: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(at)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path.strip_prefix(root).map_err(io::Error::other)?;
                out.push(relative.to_string_lossy().into_owned());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// A slot's kept boot files: `(sha256, path)` from `<id>.boot`.
pub fn kept(layout: &Layout, id: &str) -> Option<Vec<(String, String)>> {
    let text = fs::read_to_string(layout.boot_list(id)).ok()?;
    Some(
        text.lines()
            .filter_map(|line| {
                let (sha, path) = line.split_once("  ")?;
                Some((sha.to_string(), path.to_string()))
            })
            .collect(),
    )
}

/// Keeps `dir` (a slot's boot directory as it goes on the boot partition) on the data
/// partition: each file by content, and `<id>.boot` listing them.
pub fn put(layout: &Layout, id: &str, dir: &Path) -> io::Result<()> {
    let store = layout.bootfiles();
    fs::create_dir_all(&store)?;
    let mut list = String::new();
    for name in files_under(dir)? {
        let from = dir.join(&name);
        let sha = sha256_file(&from)?;
        let to = store.join(&sha);
        if !to.exists() {
            let beside = store.join(format!(".{sha}.tmp"));
            fs::copy(&from, &beside)?;
            fs::File::open(&beside)?.sync_all()?;
            fs::rename(&beside, &to)?;
        }
        list.push_str(&format!("{sha}  {name}\n"));
    }
    sync_dir(&store)?;
    replace(&layout.boot_list(id), list.as_bytes())
}

/// Puts a slot's directory on the boot partition from its kept files, checked as read back.
/// Leaves one that is there alone unless `replace_it`.
pub fn stage(layout: &Layout, id: &str, replace_it: bool) -> Result<(), String> {
    let target = layout.boot_dir(id);
    if target.is_dir() && !replace_it {
        return Ok(());
    }
    let files = kept(layout, id).ok_or_else(|| format!("slot {id} has no boot files"))?;
    let slots = layout.boot.join("slots");
    let fresh = slots.join(format!(".{id}.new"));
    let _ = fs::remove_dir_all(&fresh);
    let io = |error: io::Error| format!("slot {id}'s boot files: {error}");
    let mut need = 1u64 << 20;
    for (sha, name) in &files {
        let from = layout.bootfiles().join(sha);
        let size = fs::metadata(&from)
            .map_err(|_| format!("slot {id}'s boot file {name} is missing on the data partition"))?
            .len();
        need += size.div_ceil(1024) * 1024;
    }
    fs::create_dir_all(&slots).map_err(io)?;
    let free = lemnos_linux_sys::statfs::free_bytes(&slots).map_err(io)?;
    if free < need {
        return Err(format!(
            "no room on the boot partition for slot {id}'s boot files: they need {} MiB, it has {} MiB free",
            need >> 20,
            free >> 20
        ));
    }
    for (sha, name) in &files {
        let to = fresh.join(name);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        fs::copy(layout.bootfiles().join(sha), &to).map_err(io)?;
        fs::File::open(&to).and_then(|f| f.sync_all()).map_err(io)?;
    }
    for (sha, name) in &files {
        if sha256_file(&fresh.join(name)).map_err(io)? != *sha {
            let _ = fs::remove_dir_all(&fresh);
            return Err(format!(
                "slot {id}'s boot files didn't read back as written"
            ));
        }
    }
    let _ = fs::remove_dir_all(&target);
    fs::rename(&fresh, &target).map_err(io)?;
    sync_dir(&slots).map_err(io)
}

/// Takes off the boot partition every slot directory that isn't `resident`, half-written ones
/// too. An installed slot's directory goes only once its files are kept on the data partition
/// (a slot put there before this driver, or by a first image, is kept first).
pub fn evict(layout: &Layout, resident: &[&str]) -> io::Result<()> {
    let slots = layout.boot.join("slots");
    let Ok(entries) = fs::read_dir(&slots) else {
        return Ok(());
    };
    let mut gone = false;
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if resident.contains(&name.as_str()) {
            if layout.installed(&name) && kept(layout, &name).is_none() {
                put(layout, &name, &entry.path())?;
            }
            continue;
        }
        if valid_id(&name) && layout.installed(&name) && kept(layout, &name).is_none() {
            put(layout, &name, &entry.path())?;
        }
        fs::remove_dir_all(entry.path())?;
        gone = true;
    }
    if gone {
        sync_dir(&slots)?;
    }
    Ok(())
}

/// A removed slot's kept files, and then the files no slot lists any more.
pub fn drop_kept(layout: &Layout, id: &str) -> io::Result<()> {
    let _ = fs::remove_file(layout.boot_list(id));
    let mut used = std::collections::BTreeSet::new();
    for entry in fs::read_dir(layout.images())?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(other) = name.strip_suffix(".boot")
            && let Some(files) = kept(layout, other)
        {
            used.extend(files.into_iter().map(|(sha, _)| sha));
        }
    }
    let Ok(entries) = fs::read_dir(layout.bootfiles()) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !used.contains(&name) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// The default slot's boot files at the boot partition's root too: what the firmware starts when
/// it can't read the pointer.
pub fn root_copy(layout: &Layout, id: &str) -> io::Result<()> {
    let from = layout.boot_dir(id);
    if !from.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&from)?.flatten() {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let beside = layout.boot.join(format!(".{name}.new"));
        fs::copy(entry.path(), &beside)?;
        fs::File::open(&beside)?.sync_all()?;
        fs::rename(&beside, layout.boot.join(&name))?;
    }
    let overlays = from.join("overlays");
    if overlays.is_dir() {
        let fresh = layout.boot.join(".overlays.new");
        let _ = fs::remove_dir_all(&fresh);
        fs::create_dir_all(&fresh)?;
        for name in files_under(&overlays)? {
            let to = fresh.join(&name);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(overlays.join(&name), &to)?;
        }
        let _ = fs::remove_dir_all(layout.boot.join("overlays"));
        fs::rename(&fresh, layout.boot.join("overlays"))?;
    }
    sync_dir(&layout.boot)
}

/// The reboot request `system.reboot` leaves in the state directory before it restarts, for the
/// next boot's history (`requested` in `lemnos/boots`), and which that boot takes and removes.
pub const REQUEST_FILE: &str = "reboot-request";

/// Records that `caller` asked for the restart through `system.reboot`. The restart goes on
/// whether or not this can be written: the history then says `-`.
pub fn note_request(state_dir: &Path, caller: u64) {
    let _ = fs::create_dir_all(state_dir);
    let _ = replace(
        &state_dir.join(REQUEST_FILE),
        format!("by:{caller}\n").as_bytes(),
    );
}

/// The request the previous run left, `by:<caller>`, or `-` when there is none.
pub fn read_request(state_dir: &Path) -> String {
    fs::read_to_string(state_dir.join(REQUEST_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

/// The reset reason, as the board reports it: the file the board file's `reset_reason` names,
/// else the first watchdog's `bootstatus`. The raw text; `none` when nothing reads.
pub fn reset_reason(path: Option<&Path>) -> String {
    if let Some(path) = path {
        return fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "none".to_string());
    }
    let Ok(dirs) = fs::read_dir("/sys/class/watchdog") else {
        return "none".to_string();
    };
    let mut files: Vec<PathBuf> = dirs
        .flatten()
        .map(|entry| entry.path().join("bootstatus"))
        .collect();
    files.sort();
    for file in files {
        if let Ok(text) = fs::read_to_string(&file) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    "none".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;
    use lemnos_slot_store::boot_raw::{Boot as RawBoot, BootRaw, Pointer, Trial};
    use lemnos_slot_store::fatimage;
    use lemnos_slot_store::firmware;
    use lemnos_slot_store::twocopy::{Kind, precreate};

    /// A boot partition image in `dir`: the record files pre-created, and the three pointer files
    /// each naming `os-1`. Returns its node.
    fn fat_boot(dir: &TempDir) -> std::path::PathBuf {
        let mut files = precreate(&[
            (Kind::TRIAL, b""),
            (Kind::REASON, b""),
            (Kind::EPISODES, b""),
        ])
        .unwrap();
        let pointer = firmware::pointer_sector("os-1").unwrap().to_vec();
        for name in [b"DEFAULT TXT", b"PREVIOUSTXT", b"TRIAL   TXT"] {
            files.push((name, pointer.clone()));
        }
        let node = dir.0.join("boot.img");
        fs::write(&node, fatimage::fat16(&files)).unwrap();
        node
    }

    fn tryboot(dir: &TempDir, node: Result<std::path::PathBuf, String>) -> Tryboot {
        Tryboot {
            boot: dir.0.clone(),
            node,
            restarter: Restarter {
                check: None,
                systemctl: "/nonexistent/systemctl".into(),
            },
        }
    }

    #[test]
    fn the_tryboot_record_writes_the_pointers_the_firmware_reads_one_sector_each() {
        let dir = TempDir::new("tryboot");
        let node = fat_boot(&dir);
        let mut record = tryboot(&dir, Ok(node.clone()));
        record.set_default("a-1").unwrap();
        record.set_trial("b-2", 3).unwrap();
        let mut raw = RawBoot::open(&node).unwrap();
        assert_eq!(
            raw.pointer(Pointer::Default).unwrap().as_deref(),
            Some("a-1")
        );
        assert_eq!(raw.pointer(Pointer::Trial).unwrap().as_deref(), Some("b-2"));
        assert_eq!(
            raw.trial().unwrap(),
            Some(Trial {
                slot: "b-2".into(),
                attempts: 3
            })
        );
        // No file is created, renamed or removed on the boot partition: the boot directory is untouched.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        assert!(record.restart().is_err());
    }

    #[test]
    fn the_tryboot_trial_has_its_attempts_beside_it_and_goes_with_it() {
        let dir = TempDir::new("tryboot-attempts");
        let node = fat_boot(&dir);
        let mut record = tryboot(&dir, Ok(node.clone()));
        assert_eq!(record.pending().unwrap(), None);
        record.set_trial("b-2", 2).unwrap();
        assert_eq!(record.pending().unwrap(), Some(("b-2".into(), 2)));
        record.set_attempts(1).unwrap();
        assert_eq!(record.pending().unwrap(), Some(("b-2".into(), 1)));
        record.clear_trial().unwrap();
        assert_eq!(record.pending().unwrap(), None);
        // Setting attempts with no trial pending writes nothing.
        record.set_attempts(2).unwrap();
        assert_eq!(record.pending().unwrap(), None);
    }

    #[test]
    fn lemnosd_never_mounts_the_boot_partition() {
        // Grep proof: this file calls no mount. The node is found from the disk's sysfs, never mounted.
        let src = include_str!("boot.rs");
        // The needles are split so that this test's own text does not match them.
        assert!(!src.contains(concat!("Command::new(\"", "mount", "\")")));
        assert!(!src.contains(concat!("rootfs", "::mount")));
        assert!(!src.contains(concat!("libc", "::mount")));
    }

    #[test]
    fn a_tryboot_record_with_no_known_device_or_no_pointer_file_refuses_by_name() {
        let dir = TempDir::new("tryboot-refused");
        let mut record = tryboot(
            &dir,
            Err("no boot_sources entry names the controller".into()),
        );
        let why = record.set_default("a-1").unwrap_err().to_string();
        assert!(why.contains("device isn't known"), "{why}");
        // A partition without its pointer files is refused by the first file it lacks.
        let files = precreate(&[
            (Kind::TRIAL, b""),
            (Kind::REASON, b""),
            (Kind::EPISODES, b""),
        ])
        .unwrap();
        let node = dir.0.join("bare.img");
        fs::write(&node, fatimage::fat16(&files)).unwrap();
        let mut record = tryboot(&dir, Ok(node));
        let why = record.set_default("a-1").unwrap_err().to_string();
        assert!(why.contains("default.txt"), "{why}");
    }

    fn layout(dir: &TempDir) -> Layout {
        let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
        fs::create_dir_all(layout.images()).unwrap();
        fs::create_dir_all(layout.boot.join("slots")).unwrap();
        layout
    }

    fn slot_dir(dir: &Path, kernel: &[u8]) {
        fs::create_dir_all(dir.join("overlays")).unwrap();
        fs::write(dir.join("kernel8.img"), kernel).unwrap();
        fs::write(dir.join("cmdline.txt"), b"console=tty1\n").unwrap();
        fs::write(dir.join("overlays/README"), b"r").unwrap();
    }

    fn install(layout: &Layout, id: &str) {
        fs::write(layout.manifest(id), "{}").unwrap();
        fs::write(layout.image(id), "img").unwrap();
    }

    #[test]
    fn the_file_record_keeps_both_lines() {
        let dir = TempDir::new("record");
        let mut record = FileRecord {
            boot: dir.0.clone(),
            restarter: Restarter {
                check: None,
                systemctl: "/nonexistent/systemctl".into(),
            },
        };
        record.set_trial("b", 2).unwrap();
        record.set_default("a").unwrap();
        assert_eq!(
            fs::read_to_string(dir.0.join("boot-record")).unwrap(),
            "default=a\ntrial=b\ntrial_attempts=2\n"
        );
        assert_eq!(record.pending().unwrap(), Some(("b".into(), 2)));
        record.set_attempts(0).unwrap();
        assert_eq!(record.pending().unwrap(), Some(("b".into(), 0)));
        record.clear_trial().unwrap();
        assert_eq!(
            fs::read_to_string(dir.0.join("boot-record")).unwrap(),
            "default=a\n"
        );
        assert_eq!(record.pending().unwrap(), None);
    }

    #[test]
    fn boot_files_are_kept_once_by_content_and_staged_back_checked() {
        let dir = TempDir::new("kept");
        let layout = layout(&dir);
        let source = dir.0.join("src");
        slot_dir(&source, b"kernel");
        install(&layout, "a");
        install(&layout, "b");
        put(&layout, "a", &source).unwrap();
        put(&layout, "b", &source).unwrap();
        // Two slots with the same files keep one copy of each.
        assert_eq!(fs::read_dir(layout.bootfiles()).unwrap().count(), 3);
        stage(&layout, "a", false).unwrap();
        assert_eq!(
            fs::read(layout.boot_dir("a").join("kernel8.img")).unwrap(),
            b"kernel"
        );
        assert_eq!(
            fs::read(layout.boot_dir("a").join("overlays/README")).unwrap(),
            b"r"
        );
        // A kept file that went bad is never put in place.
        let list = kept(&layout, "b").unwrap();
        let kernel = list.iter().find(|(_, n)| n == "kernel8.img").unwrap();
        fs::write(layout.bootfiles().join(&kernel.0), b"rotten").unwrap();
        let error = stage(&layout, "b", false).unwrap_err();
        assert!(error.contains("didn't read back"), "{error}");
        assert!(!layout.boot_dir("b").exists());
        assert!(!layout.boot.join("slots/.b.new").exists());
    }

    #[test]
    fn eviction_keeps_resident_slots_and_keeps_a_copy_before_it_removes() {
        let dir = TempDir::new("evict");
        let layout = layout(&dir);
        install(&layout, "a");
        install(&layout, "old");
        // Slots put on the boot partition by a first image, with no copy on the data partition.
        slot_dir(&layout.boot_dir("a"), b"ka");
        slot_dir(&layout.boot_dir("old"), b"ko");
        slot_dir(&layout.boot_dir("gone"), b"kg");
        fs::create_dir_all(layout.boot.join("slots/.x.new")).unwrap();
        evict(&layout, &["a"]).unwrap();
        assert!(layout.boot_dir("a").is_dir());
        assert!(!layout.boot_dir("old").exists());
        assert!(!layout.boot_dir("gone").exists());
        assert!(!layout.boot.join("slots/.x.new").exists());
        // Both installed slots' files were kept first, and come back.
        assert!(kept(&layout, "a").is_some());
        stage(&layout, "old", false).unwrap();
        assert_eq!(
            fs::read(layout.boot_dir("old").join("kernel8.img")).unwrap(),
            b"ko"
        );
        drop_kept(&layout, "old").unwrap();
        assert_eq!(fs::read_dir(layout.bootfiles()).unwrap().count(), 3);
    }

    #[test]
    fn the_default_goes_to_the_root_too() {
        let dir = TempDir::new("root");
        let layout = layout(&dir);
        slot_dir(&layout.boot_dir("a"), b"ka");
        fs::create_dir_all(layout.boot.join("overlays")).unwrap();
        fs::write(layout.boot.join("overlays/stale.dtbo"), b"s").unwrap();
        root_copy(&layout, "a").unwrap();
        assert_eq!(fs::read(layout.boot.join("kernel8.img")).unwrap(), b"ka");
        assert!(layout.boot.join("overlays/README").is_file());
        assert!(!layout.boot.join("overlays/stale.dtbo").exists());
    }
}
