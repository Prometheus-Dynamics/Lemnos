//! The boot partition's image (I5i8, layout version 5): the whole FAT16 volume a board boots from,
//! made by one function that the tests and `lemnos-ctl boot-partition` both call.
//!
//! The volume holds, at its root: the generated firmware text and the layout copy (the caller's
//! [`Parts::root`]), the two pointers `default.txt`, `previous.txt` and `trial.txt` (two sectors each,
//! naming bay set 0), and the three record files `trial.rec`, `reason.rec`, `episodes.rec` (empty,
//! 1024 bytes each). Under `slots/` are the three bay sets `bay0` to `bay2`: each holds the five
//! payload roles pre-created at their board sizes ([`bays::Layout`]), the set's `bayinfo.rec`, and
//! its fixed generated files ([`Parts::fixed`]). The first slot is installed into `bay0` by
//! [`bays::install`] (the installer's own code path), so the image's default is a committed set.
//!
//! The U-Boot form (`Family::UbootBlock`) has no pointers and no records on the volume but
//! `reason.rec`: its default is the environment's, and each set's `extlinux/extlinux.conf` is composed from the command line
//! ([`extlinux_text`]). A compressed kernel is refused by name there (requirements section 8.6).

use crate::bays::{self, Done, Error, INFO_BYTES, INFO_NAME, Layout, POINTER_BYTES, ROLES, SETS};
use crate::bootfat::{self, ReadAt};
use crate::fatimage::{self, Node};
use crate::firmware;
use crate::twocopy::{SECTOR, Sectors};

/// The pointers and records at the root of the Pi firmware's volume.
pub const POINTERS: [&str; 3] = ["default.txt", "previous.txt", "trial.txt"];
/// The three record files at the root of the Pi firmware's volume, empty.
pub const RECORDS: [&str; 3] = ["trial.rec", "reason.rec", "episodes.rec"];
/// The U-Boot form's composed extlinux file, its path under each set and its bay size.
pub const EXTLINUX: &str = "extlinux/extlinux.conf";
/// The bay size of the composed extlinux file.
pub const EXTLINUX_BYTES: u64 = 4096;

/// The two firmware families the image is made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Raspberry Pi firmware: pointers, records and the firmware's text at the volume's root.
    Tryboot,
    /// U-Boot on block storage: no pointers or records on the volume; `extlinux/extlinux.conf`
    /// in each set.
    UbootBlock,
}

/// A generated file of a bay set, other than the payload roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixed {
    /// The path under the set directory.
    pub path: &'static str,
    /// Its bytes, written at build time.
    pub bytes: Vec<u8>,
    /// Its size on the volume: its own length, or a bay size it is padded to.
    pub size: u64,
}

/// What the image is made of, apart from the payloads and the size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// The family.
    pub family: Family,
    /// The payload bays.
    pub layout: Layout,
    /// The root files the caller generates: `(name, bytes)`, names without a directory.
    pub root: Vec<(&'static str, Vec<u8>)>,
    /// The generated files of each set.
    pub fixed: Vec<Fixed>,
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

/// The volume's tree for `parts`, with the sets' payloads as zeros of their bay sizes (the
/// installer writes the real bytes into them).
fn tree(parts: &Parts) -> Vec<Node> {
    let pad_file = |name: &'static str, size: u64, pad: u8| Node::File {
        name,
        data: vec![pad; size as usize],
    };
    let mut root: Vec<Node> = parts
        .root
        .iter()
        .map(|(name, bytes)| Node::File {
            name,
            data: bytes.clone(),
        })
        .collect();
    if parts.family == Family::Tryboot {
        for name in POINTERS {
            root.push(Node::File {
                name,
                data: firmware::pointer_file("bay0").unwrap().to_vec(),
            });
        }
        for name in RECORDS {
            root.push(Node::File {
                name,
                data: vec![0; INFO_BYTES as usize],
            });
        }
    }
    if parts.family == Family::UbootBlock {
        // The U-Boot form's one record: a kernel gap's reason (its trial and default are in U-Boot's
        // environment). Pre-created, like the Pi form's, so a write is two sectors and no file is made.
        root.push(Node::File {
            name: "reason.rec",
            data: vec![0; INFO_BYTES as usize],
        });
    }
    let mut sets = Vec::new();
    for set in 0..SETS {
        let mut children = Vec::new();
        for (i, r) in ROLES.iter().enumerate() {
            let size = parts.layout.bytes[i];
            let node = pad_file(base_name(r.path), size, r.pad);
            children.push(split_dir(node, r.path));
        }
        for f in &parts.fixed {
            let data = if f.bytes.len() as u64 == f.size {
                f.bytes.clone()
            } else {
                let mut v = f.bytes.clone();
                v.resize(f.size as usize, b' ');
                v
            };
            children.push(split_dir(
                Node::File {
                    name: base_name(f.path),
                    data,
                },
                f.path,
            ));
        }
        children.push(Node::File {
            name: INFO_NAME,
            data: vec![0; INFO_BYTES as usize],
        });
        sets.push(Node::Dir {
            name: ["bay0", "bay1", "bay2"][set as usize],
            children: merge_dirs(children),
        });
    }
    root.push(Node::Dir {
        name: "slots",
        children: sets,
    });
    root
}

/// The last component of a path: the name the file has in its directory.
fn base_name(path: &'static str) -> &'static str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A file path `a/b/c` as nested directories: the leaf named as the file, the directories around
/// it. A plain name is a file.
fn split_dir(leaf: Node, path: &'static str) -> Node {
    match path.split_once('/') {
        None => leaf,
        Some((dir, rest)) => Node::Dir {
            name: dir,
            children: vec![split_dir(leaf, rest)],
        },
    }
}

/// Joins the directories of one level that share a name (`overlays/` and `overlays/README`), and
/// the same inside each of them.
fn merge_dirs(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for node in nodes {
        match node {
            Node::Dir { name, children } => {
                match out
                    .iter()
                    .position(|n| matches!(n, Node::Dir { name: d, .. } if *d == name))
                {
                    Some(i) => {
                        if let Node::Dir { children: have, .. } = &mut out[i] {
                            have.extend(children);
                        }
                    }
                    None => out.push(Node::Dir { name, children }),
                }
            }
            file => out.push(file),
        }
    }
    out.into_iter()
        .map(|n| match n {
            Node::Dir { name, children } => Node::Dir {
                name,
                children: merge_dirs(children),
            },
            file => file,
        })
        .collect()
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
/// `payloads` (the five roles in [`ROLES`] order). Refused by name, with nothing built, when the
/// size is not whole sectors, is below [`min_size`], a payload is larger than its bay, the kernel is
/// compressed on the U-Boot form, or the composed image does not verify.
pub fn build(
    parts: &Parts,
    size: u64,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<Vec<u8>, String> {
    if !size.is_multiple_of(SECTOR as u64) {
        return Err(format!(
            "--size {size} is not a whole number of {SECTOR}-byte sectors"
        ));
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
    for f in &parts.fixed {
        if f.bytes.len() as u64 > f.size {
            return Err(format!(
                "{}: {} bytes do not fit {}",
                f.path,
                f.bytes.len(),
                f.size
            ));
        }
    }
    let root = tree(parts);
    let mut mem = Mem(fatimage::volume(&root, size / SECTOR as u64));
    install_first(&mut mem, parts, slot, payloads)?;
    let findings = verify(&mut mem, parts);
    if let Some(bad) = findings.iter().find(|f| !f.ok) {
        return Err(format!("the built image does not verify: {}", bad.text));
    }
    Ok(mem.0)
}

/// The installer's code path for the first slot: [`bays::install`] for the Pi firmware form, and for
/// the U-Boot form the same payload commit with the composed extlinux file.
pub fn install_first<D: Sectors + ReadAt>(
    dev: &mut D,
    parts: &Parts,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<(), String> {
    match parts.family {
        Family::Tryboot => match bays::install(dev, &parts.layout, 0, slot, payloads) {
            Ok(Done::Installed) => Ok(()),
            Ok(Done::AlreadyInstalled) => {
                Err("the first install found the slot already there".into())
            }
            Err(e) => Err(e.to_string()),
        },
        Family::UbootBlock => {
            let sets = bays::place_sets(dev, &parts.layout)?;
            let (files, info) = sets[0];
            let text = extlinux_text(payloads[3])?;
            let lba = bootfat::locate_path(dev, &format!("slots/bay0/{EXTLINUX}"), EXTLINUX_BYTES)
                .map_err(|e| e.to_string())?
                .first_lba;
            bays::commit_set_with(
                dev,
                &parts.layout,
                &files,
                info,
                slot,
                payloads,
                Some((lba, &text, EXTLINUX_BYTES)),
            )
            .map_err(|e: Error| e.to_string())
        }
    }
}

/// Verifies an image, or a dump of a real partition, read-only. Each finding is one line. The
/// checks are the boot chain's: the locator places every file at its size, each pointer is a whole
/// line, the default and previous sets' payloads hash to their records, every committed set's
/// composed extlinux file matches its command line (U-Boot form), and the firmware's root text is
/// there.
pub fn verify<D: Sectors + ReadAt>(dev: &mut D, parts: &Parts) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |ok: bool, text: String| out.push(Finding { ok, text });
    let sets = match bays::place_sets(dev, &parts.layout) {
        Ok(s) => s,
        Err(e) => {
            push(false, format!("locator: {e}"));
            return out;
        }
    };
    push(
        true,
        format!("locator: {} sets placed at their sizes", sets.len()),
    );
    for (set, (files, info)) in sets.iter().enumerate() {
        match bays::read_info(dev, *info) {
            Ok(None) => push(true, format!("bay{set}: empty (no committed record)")),
            Ok(Some(rec)) => {
                let mut ok = true;
                for (i, (len, hash)) in rec.files.iter().enumerate() {
                    let bytes = match bays::read_file(dev, files[i], *len) {
                        Ok(b) => b,
                        Err(e) => {
                            push(false, format!("bay{set}: {}: {e}", ROLES[i].role));
                            ok = false;
                            continue;
                        }
                    };
                    if bytes.len() as u64 != *len {
                        ok = false;
                    }
                    if <sha2::Sha256 as sha2::Digest>::digest(&bytes).as_slice() != hash {
                        push(
                            false,
                            format!(
                                "bay{set} ({}): the {} file does not match its record",
                                rec.slot, ROLES[i].role
                            ),
                        );
                        ok = false;
                    }
                }
                if ok {
                    push(
                        true,
                        format!(
                            "bay{set} ({}): the five payloads match their record",
                            rec.slot
                        ),
                    );
                }
                if parts.family == Family::UbootBlock {
                    let cmd = bays::read_file(dev, files[3], rec.files[3].0).unwrap_or_default();
                    match (
                        extlinux_text(&cmd),
                        bootfat::locate_path(
                            dev,
                            &format!("slots/bay{set}/{EXTLINUX}"),
                            EXTLINUX_BYTES,
                        ),
                    ) {
                        (Ok(text), Ok(found)) => {
                            let file = bays::read_file(dev, found.first_lba, EXTLINUX_BYTES)
                                .unwrap_or_default();
                            let same = file.starts_with(&text)
                                && file[text.len()..].iter().all(|&b| b == b' ');
                            push(
                                same,
                                format!(
                                    "bay{set}: extlinux.conf {} the composed text of its command line",
                                    if same { "matches" } else { "does not match" }
                                ),
                            );
                        }
                        (Err(e), _) => push(false, format!("bay{set}: extlinux.conf: {e}")),
                        (_, Err(e)) => push(false, format!("bay{set}: extlinux.conf: {e}")),
                    }
                }
            }
            Err(e) => push(false, format!("bay{set}: record: {e}")),
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
        out.push(exact(dev, name, bytes, bytes.len() as u64));
    }
    for set in 0..SETS {
        for f in parts.fixed.iter().filter(|f| !f.bytes.is_empty()) {
            let mut want = f.bytes.clone();
            want.resize(f.size as usize, b' ');
            let path = format!("{}/{}", bays::set_dir(set), f.path);
            out.push(exact(dev, &path, &want, f.size));
        }
    }
    out
}

/// One finding for a file that must hold exactly `want`, at exactly `size` bytes.
fn exact<D: Sectors + ReadAt>(dev: &mut D, path: &str, want: &[u8], size: u64) -> Finding {
    let found = match bootfat::locate_path(dev, path, size) {
        Ok(f) => f,
        Err(e) => {
            return Finding {
                ok: false,
                text: format!("{path}: {e}"),
            };
        }
    };
    match bays::read_file(dev, found.first_lba, size) {
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
