//! `lemnos-ctl boot-partition --add-to DUMP`: this system's boot files added to a dump of a boot
//! partition another system boots from, and `--undo UNDO --of FILE`, which takes them out again.
//!
//! `--add-to DUMP --layout-of BOARD [--socs DIR] --slot ID --payload DIR [--firmware DIR] [--set N]
//! [--replace-trial] --out FILE --changed-out LIST --undo-out UNDO` reads DUMP, a regular file
//! holding a whole FAT16 partition, and writes three new files: FILE (the dump with the files
//! added), LIST (every changed range of sectors) and UNDO (what those ranges held before). DUMP is
//! never written, and a block device is refused: writing the listed ranges back to a device is the
//! caller's step. The work is [`lemnos_slot_store::bootadd`]'s; the same inputs give the same three
//! files.

use lemnos_slot_store::bootadd::{self, Mode, Request};
use lemnos_slot_store::bootimage::Parts;
use lemnos_slot_store::bundle::hex;
use lemnos_slot_store::{bayfat, fatadd};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

/// What `--add-to` was given, apart from the board.
pub(crate) struct Args<'a> {
    pub dump: &'a str,
    pub slot: &'a str,
    pub payload: &'a str,
    pub firmware: Option<&'a str>,
    pub set: Option<&'a str>,
    pub replace_trial: bool,
    pub out: &'a str,
    pub changed_out: &'a str,
    pub undo_out: &'a str,
}

/// Reads a dump: a regular file only. A block device is refused by name, and so is anything else
/// that is not a regular file.
fn read_dump(file: &str) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(file).map_err(|e| format!("{file}: {e}"))?;
    let kind = meta.file_type();
    if kind.is_block_device() {
        return Err(format!(
            "{file} is a block device: --add-to reads a dump of the partition (a regular file) and never a device; dump the partition to a file first: nothing was written"
        ));
    }
    if !kind.is_file() {
        return Err(format!(
            "{file} is not a regular file: --add-to reads a dump of the partition: nothing was written"
        ));
    }
    std::fs::read(file).map_err(|e| format!("{file}: {e}"))
}

/// Refuses an output that exists: every output is a new file.
fn new_file(file: &str) -> Result<(), String> {
    if Path::new(file).exists() {
        return Err(format!(
            "{file} exists: each output is written to a new file only: nothing was written"
        ));
    }
    Ok(())
}

/// The changed-sector list as text: a comment line, then one line per range.
fn changed_text(changes: &[fatadd::Change]) -> String {
    let mut text = String::from(
        "# first-sector sector-count class sha256-before sha256-after (512-byte sectors from the partition's start)\n",
    );
    for c in changes {
        text.push_str(&format!(
            "{} {} {} {} {}\n",
            c.lba,
            c.count,
            c.class,
            hex(&c.old),
            hex(&c.new)
        ));
    }
    text
}

/// Runs `--add-to` for a board's `parts`.
pub(crate) fn add(parts: &Parts, a: &Args<'_>, out: &mut dyn Write) -> Result<(), String> {
    let set: u8 = match a.set {
        None => 0,
        Some(text) => text
            .parse()
            .map_err(|_| "--set is a bay set's number (0, 1 or 2)".to_string())?,
    };
    for file in [a.out, a.changed_out, a.undo_out] {
        new_file(file)?;
    }
    let dump = read_dump(a.dump)?;
    let payloads = super::boot_partition::read_payloads(Path::new(a.payload))?;
    let firmware = super::boot_partition::read_firmware(a.firmware, &parts.layout)?;
    let refs: Vec<&[u8]> = payloads
        .iter()
        .chain(firmware.iter())
        .map(Vec::as_slice)
        .collect();
    let done = bootadd::add_to(
        parts,
        &dump,
        &Request {
            slot: a.slot,
            set,
            replace_trial: a.replace_trial,
            payloads: &refs,
        },
    )
    .map_err(|e| format!("{}: {e}", a.dump))?;

    let mut text = String::new();
    let mut line = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    line(match done.mode {
        Mode::Added => format!("mode: files added, slot {} installed into bay{set}", a.slot),
        Mode::InstalledOnly => format!(
            "mode: files already there, slot {} installed into bay{set}",
            a.slot
        ),
    });
    line(
        "allocation: first fit (each file one run, the lowest-numbered free run that holds it)"
            .to_string(),
    );
    line(format!(
        "free before: {} clusters in {} run(s), longest {}",
        done.free_before.clusters, done.free_before.runs, done.free_before.longest
    ));
    line(format!(
        "free after: {} clusters in {} run(s), longest {}",
        done.free_after.clusters, done.free_after.runs, done.free_after.longest
    ));
    for f in &done.added {
        // A set's region is added whole as `reserve`; the install then writes its files into the
        // region and leaves `reserve` the rest, so the line gives the region, not the file's size.
        let region = !f.is_dir && f.path.ends_with(&format!("/{}", bayfat::RESERVE));
        line(format!(
            "{} {} {} {} {}",
            if f.is_dir {
                "added-dir"
            } else if region {
                "added-region"
            } else {
                "added"
            },
            f.path,
            f.size,
            f.first,
            f.clusters
        ));
    }
    for r in &done.replaced {
        line(format!(
            "replaced {} {} {} {} (was {} bytes, sha256 {}, first cluster {}, {} cluster(s), now free)",
            r.name,
            r.size,
            r.first,
            r.clusters,
            r.old_size,
            hex(&r.old_sha256),
            r.old_first,
            r.old_clusters
        ));
    }
    for name in &done.left {
        line(format!(
            "left {name}: the existing system's (give --replace-trial to replace it)"
        ));
    }
    line(format!("expected BAD: {}", done.expected_bad.join(", ")));
    for f in &done.findings {
        line(format!("{} {}", if f.ok { "ok " } else { "BAD" }, f.text));
    }
    let sectors: u64 = done.changes.iter().map(|c| c.count).sum();
    line(format!(
        "changed: {sectors} sectors in {} range(s), listed in {}; undo file {}",
        done.changes.len(),
        a.changed_out,
        a.undo_out
    ));
    std::fs::write(a.out, &done.image).map_err(|e| format!("{}: {e}", a.out))?;
    std::fs::write(a.changed_out, changed_text(&done.changes))
        .map_err(|e| format!("{}: {e}", a.changed_out))?;
    std::fs::write(a.undo_out, &done.undo).map_err(|e| format!("{}: {e}", a.undo_out))?;
    line(format!(
        "wrote {}: {} bytes, sha256 {} (the dump: sha256 {})",
        a.out,
        done.image.len(),
        hex(&Sha256::digest(&done.image)),
        hex(&Sha256::digest(&dump))
    ));
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Runs `--undo UNDO --of FILE --out RESTORED`: the edited dump with every changed range put back.
pub(crate) fn undo(undo: &str, of: &str, to: &str, out: &mut dyn Write) -> Result<(), String> {
    new_file(to)?;
    let image = read_dump(of)?;
    let bytes = std::fs::read(undo).map_err(|e| format!("{undo}: {e}"))?;
    let restored = fatadd::apply_undo(&image, &bytes).map_err(|e| format!("{of}: {e}"))?;
    let ranges = fatadd::undo_ranges(&bytes).map_err(|e| format!("{undo}: {e}"))?;
    std::fs::write(to, &restored).map_err(|e| format!("{to}: {e}"))?;
    writeln!(
        out,
        "wrote {to}: {} bytes, sha256 {}, {} sectors in {} range(s) put back",
        restored.len(),
        hex(&Sha256::digest(&restored)),
        ranges.iter().map(|r| r.1).sum::<u64>(),
        ranges.len()
    )
    .map_err(|e| e.to_string())
}
