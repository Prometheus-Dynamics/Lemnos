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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};
use lemnos_slot_store::uboot_env::{self, FileEnv, Geometry, Place};

use crate::boot::{BootRecord, Restarter};
use crate::store::{Layout, replace, sync_dir, valid_id};

pub use lemnos_slot_store::records::MAX_ATTEMPTS;
pub use lemnos_slot_store::records::uboot::{KEYS, RECORD_FILE, Record};

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

/// The loader configuration the install writes for slot `id` (see [`extlinux`]), into the slot's
/// boot directory `dir`, which holds the slot's files.
fn write_extlinux(dir: &Path, id: &str, cmdline: &str) -> io::Result<()> {
    let files = files_under(dir)?;
    let text = extlinux(id, &files, cmdline).map_err(io::Error::other)?;
    fs::create_dir_all(dir.join("extlinux"))?;
    fs::write(dir.join(EXTLINUX), text)
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
}

impl UbootEnv {
    /// The environment opened: its device and the geometry the place gives on it. The boot disk is
    /// only needed for the GPT form; the device form names its device.
    fn open(&self) -> io::Result<(FileEnv, Geometry)> {
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
        uboot_env::set(&mut dev, &geometry, updates).map_err(io::Error::other)
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
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        self.set(&[("default", Some(id))])
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> io::Result<()> {
        self.set(&[
            ("trial", Some(id)),
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
        self.set(&[("trial", None), ("trial_attempts", None)])
    }

    fn restart(&mut self) -> io::Result<()> {
        // The script starts a trial that has attempts left on any restart.
        self.restarter.run(None)
    }

    fn prepare(&self, dir: &Path, id: &str, cmdline: &str) -> io::Result<()> {
        write_extlinux(dir, id, cmdline)
    }

    /// The root copy is the install-time fallback (see [`Uboot`]): nothing is rewritten here.
    fn fallback_copy(&self, _layout: &Layout, _id: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
