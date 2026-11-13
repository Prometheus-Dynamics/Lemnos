//! The `uboot` boot record, for boards that boot through U-Boot's standard boot flow.
//!
//! Each slot keeps its own kernel, device trees and initramfs, so the loader chooses before any
//! kernel runs. At install the driver writes `slots/<id>/extlinux/extlinux.conf` from the slot's
//! files and its command line. The record is `boot-record.env` on the boot partition, in the text
//! form U-Boot's `env import -t` reads: `default=<id>`, `trial=<id>` (empty when none) and
//! `trial_armed=0|1`. A static script, `packaging/uboot/boot.cmd`, loads the record and, for an
//! armed trial, first writes the record back disarmed and then starts the trial; when that
//! returns, or there is no armed trial, it starts the default; when the record can't be read, the
//! fixed `extlinux/extlinux.conf` at the boot partition's root, a copy of the default's this
//! driver keeps current. [`steps`] is that decision, which the script mirrors.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};

use crate::boot::{BootRecord, Restarter};
use crate::store::{Layout, replace, sync_dir, valid_id};

pub use lemnos_slot_store::records::uboot::{KEYS, RECORD_FILE, Record};

/// Where a slot's loader configuration is, under its boot directory.
pub const EXTLINUX: &str = "extlinux/extlinux.conf";

/// One step of the loader script, in the order it takes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Write the record back with `trial_armed=0`. When that write fails, the trial is skipped.
    Disarm,
    /// `sysboot` this slot's configuration. Returns only when that slot couldn't be started.
    Boot(String),
    /// `sysboot` the fixed configuration at the boot partition's root.
    Root,
}

/// What the loader script does with `record` (`None`: it couldn't be read): each step is tried
/// when the one before it returned.
pub fn steps(record: Option<&Record>) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(record) = record {
        if record.armed && !record.trial.is_empty() {
            steps.push(Step::Disarm);
            steps.push(Step::Boot(record.trial.clone()));
        }
        if !record.default.is_empty() {
            steps.push(Step::Boot(record.default.clone()));
        }
    }
    steps.push(Step::Root);
    steps
}

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
/// device tree the one `.dtb` at the top, else every `.dtb` there or in `dtbs/` through
/// `fdtdir`, which U-Boot picks from by the board's `fdtfile`.
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
    let dtbs: Vec<&str> = top
        .iter()
        .copied()
        .filter(|f| f.ends_with(".dtb"))
        .collect();
    let dir = format!("/slots/{id}");
    let fdt = match dtbs.as_slice() {
        [one] => format!("  fdt {dir}/{one}\n"),
        [] if files
            .iter()
            .any(|f| f.starts_with("dtbs/") && f.ends_with(".dtb")) =>
        {
            format!("  fdtdir {dir}/dtbs/\n")
        }
        [] => String::new(),
        _ => format!("  fdtdir {dir}/\n"),
    };
    for name in [kernel, initrd].into_iter().chain(dtbs.iter().copied()) {
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
         {fdt}\
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
            None => replace(&self.path(), record.text().as_bytes()),
        }
    }
}

impl BootRecord for Uboot {
    fn set_default(&mut self, id: &str) -> io::Result<()> {
        let mut record = self.read();
        record.default = id.to_string();
        self.write(&record)
    }

    fn set_trial(&mut self, id: &str) -> io::Result<()> {
        let mut record = self.read();
        record.trial = id.to_string();
        record.armed = true;
        self.write(&record)
    }

    fn restart(&mut self, _trial: bool) -> io::Result<()> {
        // The script starts an armed trial on any restart.
        self.restarter.run(None)
    }

    fn prepare(&self, dir: &Path, id: &str, cmdline: &str) -> io::Result<()> {
        let files = files_under(dir)?;
        let text = extlinux(id, &files, cmdline).map_err(io::Error::other)?;
        fs::create_dir_all(dir.join("extlinux"))?;
        fs::write(dir.join(EXTLINUX), text)
    }

    fn fallback_copy(&self, layout: &Layout, id: &str) -> io::Result<()> {
        let from = layout.boot_dir(id).join(EXTLINUX);
        let text = fs::read(&from)?;
        let dir = self.boot.join("extlinux");
        fs::create_dir_all(&dir)?;
        replace(&dir.join("extlinux.conf"), &text)?;
        sync_dir(&self.boot)
    }
}

#[cfg(test)]
mod tests;
