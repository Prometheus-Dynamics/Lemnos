//! The boot partition's image (layout version 6): the whole FAT16 volume a board boots from,
//! made by one function that the tests and `lemnos-ctl boot-partition` both call.
//!
//! The volume holds, at its root: the generated firmware text, the layout copy and the bay member
//! list (the caller's [`Parts::root`]), the pointers `default.txt`, `previous.txt` and `trial.txt`
//! (two sectors each, naming bay set 0), and the record files. Under `slots/` are the three bay
//! sets `bay0` to `bay2`: each a directory sized for its files' entries, with its record
//! (`bayinfo.rec`), its subdirectories, and its region, held whole by `reserve` until an install
//! ([`crate::bays`]). The first slot is installed into `bay0` by [`bays::install`] (the
//! installer's own code path), so the image's default is a committed set whose files sit at their
//! exact sizes.
//!
//! The U-Boot form (`Family::UbootBlock`) has no pointers and no records on the volume but
//! `reason.rec`: its default is the environment's, and each set's `extlinux/extlinux.conf` is
//! composed from the command line ([`extlinux_text`]). A compressed kernel is refused by name there.

use crate::bayfat::{self, Region};
use crate::bays::{self, Done, Form, INFO_BYTES, INFO_NAME, Info, Layout, POINTER_BYTES, SETS};
use crate::bootfat::{self, ReadAt};
use crate::fatimage::{self, Node};
use crate::firmware;
use crate::twocopy::{SECTOR, Sectors};

pub use crate::bays::{EXTLINUX, EXTLINUX_BYTES};

/// The pointers and records at the root of the Pi firmware's volume.
pub const POINTERS: [&str; 3] = ["default.txt", "previous.txt", "trial.txt"];
/// The bay set the first slot is installed into.
pub const FIRST_SET: u8 = 0;
/// The three record files at the root of the Pi firmware's volume, empty.
pub const RECORDS: [&str; 3] = ["trial.rec", "reason.rec", "episodes.rec"];
/// The restart count's baseline record at the boot partition's root (`twocopy::Kind::BASELINE`).
pub const BASELINE_FILE: &str = "count.rec";

/// The switch record a new image holds: one two-copy record, off, at generation 0.
fn switch_off_record() -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * SECTOR);
    for (_, bytes) in
        crate::twocopy::precreate(&[(crate::twocopy::Kind::SECURITY, b"off")]).unwrap_or_default()
    {
        out.extend_from_slice(&bytes);
    }
    out
}

/// The two firmware families the image is made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Raspberry Pi firmware: pointers, records and the firmware's text at the volume's root.
    Tryboot,
    /// U-Boot on block storage: no pointers or records on the volume; `extlinux/extlinux.conf`
    /// in each set.
    UbootBlock,
}

impl Family {
    /// The bay form of the family.
    pub fn form(self) -> Form {
        match self {
            Family::Tryboot => Form::Firmware,
            Family::UbootBlock => Form::Uboot,
        }
    }
}

/// What the image is made of, apart from the payloads and the size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// The family.
    pub family: Family,
    /// The payload bays (its form is the family's).
    pub layout: Layout,
    /// The root files the caller generates: `(name, bytes)`, names without a directory. The bay
    /// member list ([`bays::FIRMWARE_LIST`]) is added by the builder.
    pub root: Vec<(&'static str, Vec<u8>)>,
}

/// One line of a verification: whether it holds, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// True when the check holds.
    pub ok: bool,
    /// The line.
    pub text: String,
}

/// A whole image in memory, read and written one sector at a time.
pub struct Mem(pub Vec<u8>);

impl ReadAt for Mem {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let at = offset as usize;
        buf.copy_from_slice(&self.0[at..at + buf.len()]);
        Ok(())
    }
}

impl Sectors for Mem {
    fn read(&mut self, lba: u64, buf: &mut crate::twocopy::Sector) -> std::io::Result<()> {
        let at = lba as usize * SECTOR;
        buf.copy_from_slice(&self.0[at..at + SECTOR]);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &crate::twocopy::Sector) -> std::io::Result<()> {
        let at = lba as usize * SECTOR;
        self.0[at..at + SECTOR].copy_from_slice(buf);
        Ok(())
    }
}

/// The compression magics a kernel can start with. Any of them is refused on the U-Boot form.
const COMPRESSED: [(&str, &[u8]); 6] = [
    ("gzip", &[0x1F, 0x8B]),
    ("bzip2", b"BZh"),
    ("xz", &[0xFD, b'7', b'z', b'X', b'Z', 0x00]),
    ("lzma", &[0x5D, 0x00, 0x00]),
    ("lz4", &[0x02, 0x21, 0x4C, 0x18]),
    ("zstd", &[0x28, 0xB5, 0x2F, 0xFD]),
];

/// Refuses a kernel that is compressed, by name, for the U-Boot form. `booti` decompresses only
/// when `kernel_comp_addr_r` and `kernel_comp_size` are set beforehand, and the size is per slot.
pub fn refuse_compressed_kernel(kernel: &[u8]) -> Result<(), String> {
    for (name, magic) in COMPRESSED {
        if kernel.starts_with(magic) {
            return Err(format!(
                "the kernel is {name}-compressed: the U-Boot block form takes an uncompressed kernel in \
                 I5i8, because booti needs kernel_comp_addr_r and kernel_comp_size set per slot (requirements \
                 section 8.6, item 4); a gzip kernel is for the Pi firmware form only"
            ));
        }
    }
    Ok(())
}

/// The composed `extlinux.conf` of a U-Boot set, from its command line (one line; the trailing
/// newline is not part of the command line, the way the bundle hashes it).
pub fn extlinux_text(cmdline: &[u8]) -> Result<Vec<u8>, String> {
    let line = cmdline.strip_suffix(b"\n").unwrap_or(cmdline);
    if line.contains(&b'\n') || line.contains(&b'\r') {
        return Err("the command line has more than one line".into());
    }
    let mut out = "# Generated by lemnos-ctl boot-partition from the command line of this set. Do not edit.\n\
         DEFAULT lemnos\nTIMEOUT 0\nLABEL lemnos\n    KERNEL ../kernel8.img\n    INITRD ../initramfs.cpio\n    APPEND "
        .as_bytes()
        .to_vec();
    out.extend_from_slice(line);
    out.push(b'\n');
    if out.len() as u64 > EXTLINUX_BYTES {
        return Err(format!(
            "the composed extlinux.conf is {} bytes, more than its bay",
            out.len()
        ));
    }
    Ok(out)
}

/// The volume's tree for `parts`: the root files, and each set's directory with its record, its
/// subdirectories (empty) and its region.
pub(crate) fn tree(parts: &Parts) -> Vec<Node> {
    let mut root: Vec<Node> = parts
        .root
        .iter()
        .map(|(name, bytes)| Node::File {
            name,
            data: bytes.clone(),
        })
        .collect();
    root.push(Node::File {
        name: bays::FIRMWARE_LIST,
        data: bays::firmware_list_text(&parts.layout).into_bytes(),
    });
    if parts.family == Family::Tryboot {
        for name in POINTERS {
            root.push(Node::File {
                name,
                data: firmware::pointer_file(&bays::set_name(FIRST_SET))
                    .unwrap()
                    .to_vec(),
            });
        }
        for name in RECORDS {
            root.push(Node::File {
                name,
                data: vec![0; INFO_BYTES as usize],
            });
        }
        root.push(Node::File {
            name: BASELINE_FILE,
            data: vec![0; INFO_BYTES as usize],
        });
        root.push(Node::File {
            name: crate::security::BOOT_RECORD,
            data: switch_off_record(),
        });
    }
    if parts.family == Family::UbootBlock {
        root.push(Node::File {
            name: "reason.rec",
            data: vec![0; INFO_BYTES as usize],
        });
    }
    let entries = parts.layout.dir_entries();
    let caps = parts.layout.caps();
    let mut sets = Vec::new();
    for set in 0..SETS {
        let mut children = vec![Node::File {
            name: INFO_NAME,
            data: vec![0; INFO_BYTES as usize],
        }];
        for (dir, n) in entries.iter().skip(1) {
            children.push(Node::DirOf {
                name: leak(dir.clone()),
                entries: *n,
                children: Vec::new(),
            });
        }
        children.push(Node::Region {
            name: bayfat::RESERVE,
            caps: caps.clone(),
        });
        sets.push(Node::DirOf {
            name: ["bay0", "bay1", "bay2"][set as usize],
            entries: entries[0].1,
            children,
        });
    }
    root.push(Node::Dir {
        name: "slots",
        children: sets,
    });
    root
}

/// A name the volume's tree holds for the life of the build (the tree's names are `'static`).
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// The smallest partition, in bytes, that holds `parts`' tree: the first size, in whole sectors,
/// whose FAT16 volume has the clusters the tree needs. The tree's sizes do not depend on the
/// payloads, so this is the same for every build of a board.
pub fn min_size(parts: &Parts) -> Result<u64, String> {
    let root = tree(parts);
    let mut total = 4096u64;
    loop {
        if let Some(spc) = fatimage::try_cluster_sectors(total) {
            let (_, clusters, _) = fatimage::geometry(total, spc);
            if fatimage::clusters_needed(&root, spc) <= clusters {
                return Ok(total * SECTOR as u64);
            }
        }
        total += 1;
        if total > 1 << 26 {
            return Err("no FAT16 volume holds this tree".into());
        }
    }
}

/// Builds the image of `size` bytes, with the first slot `slot` installed into bay set 0 from
/// `payloads` (in payload order, [`Layout::paths`]). Refused by name, with nothing built, when the
/// size is not whole sectors, is below [`min_size`], a payload is larger than its bay, the kernel is
/// compressed on the U-Boot form, or the composed image does not verify.
pub fn build(parts: &Parts, size: u64, slot: &str, payloads: &[&[u8]]) -> Result<Vec<u8>, String> {
    if !size.is_multiple_of(SECTOR as u64) {
        return Err(format!(
            "--size {size} is not a whole number of {SECTOR}-byte sectors"
        ));
    }
    if parts.layout.form != parts.family.form() {
        return Err("the layout's form is not the family's".into());
    }
    let min = min_size(parts)?;
    if size < min {
        return Err(format!(
            "--size {size} is below the minimum of {min} bytes for this board's bays; \
             the minimum is printed by --min-size"
        ));
    }
    bays::check_install(&parts.layout, 0, slot, payloads).map_err(|e| e.to_string())?;
    if parts.family == Family::UbootBlock {
        refuse_compressed_kernel(payloads[0])?;
    }
    let root = tree(parts);
    let mut mem = Mem(fatimage::volume(&root, size / SECTOR as u64));
    bays::write_empty_records(&mut mem)?;
    bays::lay_out_empty_sets(&mut mem, &parts.layout)?;
    install_first(&mut mem, parts, slot, payloads)?;
    let installed = bays::set_of_slot(&mut mem, slot).map_err(|e| e.to_string())?;
    if installed != Some(FIRST_SET) {
        return Err(format!(
            "the first slot {slot} is in {installed:?}, not the bay the pointers name ({})",
            bays::set_name(FIRST_SET)
        ));
    }
    let findings = verify(&mut mem, parts);
    if let Some(bad) = findings.iter().find(|f| !f.ok) {
        return Err(format!("the built image does not verify: {}", bad.text));
    }
    Ok(mem.0)
}

/// The installer's code path for the first slot: [`bays::install`] for the Pi firmware form, and for
/// the U-Boot form the same commit, which has no pointers to move.
pub fn install_first<D: Sectors + ReadAt>(
    dev: &mut D,
    parts: &Parts,
    slot: &str,
    payloads: &[&[u8]],
) -> Result<(), String> {
    match parts.family {
        Family::Tryboot => match bays::install(dev, &parts.layout, FIRST_SET, slot, payloads) {
            Ok(Done::Installed) => Ok(()),
            Ok(Done::AlreadyInstalled) => {
                Err("the first install found the slot already there".into())
            }
            Err(e) => Err(e.to_string()),
        },
        Family::UbootBlock => {
            let info = bays::place_sets(dev)?[FIRST_SET as usize];
            bays::commit_set(dev, &parts.layout, FIRST_SET, info, slot, payloads)
                .map_err(|e| e.to_string())
        }
    }
}

/// The region a set's `reserve` and files must lie in, read from its record.
fn region_of<D: Sectors + ReadAt>(dev: &mut D, lba: u64) -> Result<Info, String> {
    bays::record_or_why(dev, lba)?.map_err(|why| {
        format!(
            "{why}: it names no region of layout version {}",
            bays::LAYOUT_VERSION
        )
    })
}

/// Whether the file at `path` (its chain) lies inside `region`; its size for the message.
fn inside<D: ReadAt>(dev: &mut D, path: &str, region: Region) -> Result<u64, String> {
    let size = bootfat::size_of_path(dev, path).map_err(|e| format!("{path}: {e}"))?;
    if size == 0 {
        return Ok(0);
    }
    let found = bootfat::locate_path(dev, path, size).map_err(|e| format!("{path}: {e}"))?;
    let v = bootfat::volume(dev).map_err(|e| e.to_string())?;
    let first = (found.first_lba - v.data_start) / v.spc + 2;
    let n = size.div_ceil(v.spc * SECTOR as u64);
    if first < region.start || first + n > region.start + region.clusters {
        return Err(format!(
            "{path}: clusters {first}..{} lie outside the set's region",
            first + n
        ));
    }
    Ok(size)
}

/// Verifies an image, or a dump of a real partition, read-only. Each finding is one line: each set's
/// record and region (aligned, on the volume, apart from the other sets'), its files and `reserve`
/// inside its region, each committed set's files against its record, the pointers, the records and
/// the firmware's root text.
pub fn verify<D: Sectors + ReadAt>(dev: &mut D, parts: &Parts) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |ok: bool, text: String| out.push(Finding { ok, text });
    let sets = match bays::place_sets(dev) {
        Ok(s) => s,
        Err(e) => {
            push(false, format!("locator: {e}"));
            return out;
        }
    };
    push(true, format!("locator: {} set records placed", sets.len()));
    let v = match bootfat::volume(dev) {
        Ok(v) => v,
        Err(e) => {
            push(false, format!("volume: {e}"));
            return out;
        }
    };
    let mut regions: Vec<(usize, Region)> = Vec::new();
    for (set, lba) in sets.iter().enumerate() {
        let info = match region_of(dev, *lba) {
            Ok(i) => i,
            Err(e) => {
                push(false, format!("bay{set}: {e}"));
                continue;
            }
        };
        let r = info.region;
        if let Err(e) = r.check(&v) {
            push(false, format!("bay{set}: {e}"));
            continue;
        }
        if let Some((other, _)) = regions
            .iter()
            .find(|(_, o)| r.start < o.start + o.clusters && o.start < r.start + r.clusters)
        {
            push(false, format!("bay{set}: its region overlaps bay{other}'s"));
        }
        regions.push((set, r));
        let dir = bays::set_dir(set as u8);
        if info.slot.is_none() {
            // An empty set's region is its record's; what its directory holds is the next
            // install's to lay out (a cut install leaves it half written, and that is no fault).
            push(
                true,
                format!(
                    "bay{set}: region at cluster {}, {} clusters; empty (no committed record)",
                    r.start, r.clusters
                ),
            );
            continue;
        }
        let mut names: Vec<String> = parts.layout.names();
        names.push(bayfat::RESERVE.to_string());
        let mut placed = true;
        for n in &names {
            if let Err(e) = inside(dev, &format!("{dir}/{n}"), r) {
                push(false, format!("bay{set}: {e}"));
                placed = false;
            }
        }
        if placed {
            push(
                true,
                format!(
                    "bay{set}: region at cluster {}, {} clusters, holds its files and reserve",
                    r.start, r.clusters
                ),
            );
        }
        match bays::check_set(dev, &parts.layout, set as u8, &info) {
            Ok(slot) => push(
                true,
                format!(
                    "bay{set} ({slot}): its {} files match their record",
                    info.members.len()
                ),
            ),
            Err(e) => push(false, e),
        }
    }
    if parts.family == Family::Tryboot {
        for name in POINTERS {
            match bootfat::locate_path(dev, name, POINTER_BYTES) {
                Ok(f) => match bays::read_pointer(dev, f.first_lba) {
                    Ok(Some(set)) => push(true, format!("{name}: names bay{set}")),
                    Ok(None) => push(false, format!("{name}: holds no whole line")),
                    Err(e) => push(false, format!("{name}: {e}")),
                },
                Err(e) => push(false, format!("{name}: {e}")),
            }
        }
        for name in RECORDS {
            match bootfat::locate_path(dev, name, INFO_BYTES) {
                Ok(_) => push(true, format!("{name}: placed")),
                Err(e) => push(false, format!("{name}: {e}")),
            }
        }
    }
    if parts.family == Family::UbootBlock {
        match bootfat::locate_path(dev, "reason.rec", INFO_BYTES) {
            Ok(_) => push(true, "reason.rec: placed".to_string()),
            Err(e) => push(false, format!("reason.rec: {e}")),
        }
    }
    for (name, bytes) in &parts.root {
        out.push(exact(dev, name, bytes));
    }
    let list = bays::firmware_list_text(&parts.layout).into_bytes();
    out.push(exact(dev, bays::FIRMWARE_LIST, &list));
    out
}

/// A committed bay set read back from the image: its slot, and its files by path, in written order.
/// Read only: the hashes are not checked here (that is [`verify`]'s job).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The bay set's number.
    pub set: usize,
    /// The slot the set's record names.
    pub slot: String,
    /// The files, in order: `(path, bytes)`.
    pub members: Vec<(String, Vec<u8>)>,
}

/// The committed bay sets of an image. A set with no committed slot is left out.
pub fn committed<D: Sectors + ReadAt>(
    dev: &mut D,
    parts: &Parts,
) -> Result<Vec<Committed>, String> {
    let sets = bays::place_sets(dev)?;
    let mut out = Vec::new();
    for (set, lba) in sets.iter().enumerate() {
        let Some(rec) = bays::read_info(dev, *lba).map_err(|e| format!("bay{set}: record: {e}"))?
        else {
            continue;
        };
        let Some(slot) = rec.slot.clone() else {
            continue;
        };
        let mut members = Vec::new();
        for (path, (len, _)) in parts.layout.names().into_iter().zip(&rec.members) {
            let at = format!("{}/{path}", bays::set_dir(set as u8));
            let bytes = bootfat::read_file(dev, &at, *len)
                .map_err(|e| format!("bay{set} ({slot}): {path}: {e}"))?;
            members.push((path, bytes));
        }
        out.push(Committed { set, slot, members });
    }
    Ok(out)
}

/// One finding for a file that must hold exactly `want`.
fn exact<D: Sectors + ReadAt>(dev: &mut D, path: &str, want: &[u8]) -> Finding {
    match bootfat::read_file(dev, path, want.len() as u64) {
        Ok(bytes) if bytes == want => Finding {
            ok: true,
            text: format!("{path}: holds its generated bytes"),
        },
        Ok(_) => Finding {
            ok: false,
            text: format!("{path}: does not hold its generated bytes"),
        },
        Err(e) => Finding {
            ok: false,
            text: format!("{path}: {e}"),
        },
    }
}

#[cfg(test)]
#[path = "bootimage_tests.rs"]
mod tests;
