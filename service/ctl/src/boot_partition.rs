//! `lemnos-ctl boot-partition`: the boot partition image of a board, with its payload bays.
//!
//! `--layout-of BOARD --slot ID --payload DIR --size BYTES --out FILE` writes the complete image: the
//! firmware's text (the fixed text of `config.txt` and `tryboot.txt` with the board's `gpio=` lines,
//! from the board file and its SoC map alone), the pointers and records, and the three bay sets
//! pre-created at the board's bay sizes. The first slot's payloads are written into bay set 0 by the
//! installer's own code path ([`lemnos_slot_store::bays`]). The image is deterministic: the same
//! inputs give the same bytes.
//!
//! `--payload DIR` takes the five build roles by name: `DIR/kernel`, `DIR/initramfs`,
//! `DIR/device-tree`, `DIR/cmdline` and `DIR/board-overlay`, nothing else. `--min-size` prints the
//! smallest `--size` (bytes) for the board, and writes nothing; with `--payload DIR` it prints one
//! line per role, `role stored_bytes bay_bytes`, and exits non-zero naming any role whose payload is
//! bigger than its bay. `--verify FILE` checks an image, or a dump of a real partition, read-only:
//! one line per finding, and a non-zero exit when any finding fails. `--add-to DUMP` adds the board's
//! files to a dump of a partition another system boots from, and `--undo` takes them out
//! ([`crate::boot_add`]).

use crate::boot_dir::{self, with_fragment};
use lemnos_host::board::{BoardFile, BootSpec};
use lemnos_slot_store::bays::{self, Layout, ROLES};
use lemnos_slot_store::bootimage::{self, Committed, Family, Finding, Fixed, Parts};
use lemnos_slot_store::firmware::{TRYBOOT_TXT, config_for};
use lemnos_slot_store::layout::OVERLAY_README;
use std::io::Write;
use std::path::Path;

/// The firmware overlay names of a layout (`overlays/<name>.dtbo` to `<name>`), in the layout's order.
pub(crate) fn overlay_names(layout: &Layout) -> Vec<String> {
    layout
        .firmware
        .iter()
        .map(|(path, _)| {
            path.trim_start_matches("overlays/")
                .trim_end_matches(".dtbo")
                .to_string()
        })
        .collect()
}

/// The board's `[boot] config_lines` and `firmware_overlays`, checked by name before any image is
/// made: each line is one line; each name is a firmware overlay name and listed once; a `dtoverlay=`
/// line names an overlay the list gives; and each listed overlay is loaded by a `dtoverlay=` line.
pub(crate) fn check_boot_keys(boot: &BootSpec) -> Result<(), String> {
    for (i, line) in boot.config_lines.iter().enumerate() {
        if line.is_empty() || line.contains(['\n', '\r']) {
            return Err(format!(
                "[boot] config_lines entry {} is not one firmware line: {line:?}",
                i + 1
            ));
        }
    }
    for (i, name) in boot.firmware_overlays.iter().enumerate() {
        if !bays::valid_firmware_name(name) {
            return Err(format!(
                "[boot] firmware_overlays {name:?} is not an overlay name (letters, digits, '-' and '_', at most 40)"
            ));
        }
        if boot.firmware_overlays[..i].contains(name) {
            return Err(format!("[boot] firmware_overlays lists {name:?} twice"));
        }
    }
    let mut loaded: Vec<&str> = Vec::new();
    for line in &boot.config_lines {
        let Some(rest) = line.trim_start().strip_prefix("dtoverlay=") else {
            continue;
        };
        let name = rest.split(',').next().unwrap_or("").trim();
        if !boot.firmware_overlays.iter().any(|n| n == name) {
            return Err(format!(
                "[boot] config_lines has {:?}, which loads the overlay {name:?}, and firmware_overlays does not list {name:?}",
                line.trim()
            ));
        }
        if loaded.contains(&name) {
            return Err(format!(
                "[boot] config_lines loads the overlay {name:?} on more than one line: its parameters come from one line"
            ));
        }
        loaded.push(name);
    }
    for name in &boot.firmware_overlays {
        if !loaded.contains(&name.as_str()) {
            return Err(format!(
                "[boot] firmware_overlays lists {name:?}, and no config_lines entry loads it (dtoverlay={name})"
            ));
        }
    }
    crate::boot_merge::check_expect(boot)
}

/// The firmware overlay files a board declares, read from `dir` (`<name>.dtbo`, in the layout's
/// order). Refused by name when a declared overlay is missing, or when the board declares some and no
/// directory is given.
pub(crate) fn read_firmware(dir: Option<&str>, names: &[String]) -> Result<Vec<Vec<u8>>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let dir = dir.ok_or_else(|| {
        format!(
            "this board declares the firmware overlays {}: give --firmware DIR, the directory holding their .dtbo files",
            names.join(", ")
        )
    })?;
    names
        .iter()
        .map(|name| {
            let path = Path::new(dir).join(format!("{name}.dtbo"));
            std::fs::read(&path).map_err(|e| {
                format!(
                    "{}: the firmware overlay {name} is missing from {dir} ({e}): it needs {name}.dtbo there",
                    path.display()
                )
            })
        })
        .collect()
}

/// The board's family and its payload bays, each refused by name when a key is missing.
pub(crate) fn board_parts(board: &BoardFile, fragment: &str) -> Result<Parts, String> {
    // The contract the image carries is checked against the code that writes the volume first.
    boot_dir::check_contract()?;
    let record = boot_dir::record_of(board)?;
    let family = match record.as_str() {
        "tryboot" => Family::Tryboot,
        "uboot" => Family::UbootBlock,
        other => {
            return Err(format!(
                "boot-partition writes the \"tryboot\" and \"uboot\" records, and this board's record is {other:?}"
            ));
        }
    };
    let boot = &board.boot;
    check_boot_keys(boot)?;
    if family == Family::UbootBlock && !boot.config_lines.is_empty() {
        return Err(
            "[boot] config_lines are firmware lines in config.txt: this board's record is \"uboot\", which has no config.txt"
                .to_string(),
        );
    }
    // The firmware config digest and the firmware text: the board's lines follow the fixed text of
    // both config files, inside `[all]`, so they apply to every boot of either pointer.
    // The count fallback (`[boot_count>3]`) is the Raspberry Pi 5 family's: `boot_count` exists there and
    // nowhere else. A board that asks for it elsewhere is refused by name, and no other Pi gets the filter.
    let soc = board.board.soc.clone().unwrap_or_default();
    let pi5 = soc.starts_with("bcm2712");
    let count_fallback = boot.count_fallback.unwrap_or(pi5);
    if count_fallback && !pi5 {
        return Err(format!(
            "[boot] count_fallback = true: the firmware's boot_count exists on the Raspberry Pi 5 family only, and this board's soc is {soc:?}: remove count_fallback, or set it to false"
        ));
    }
    if family == Family::UbootBlock && boot.count_fallback.is_some() {
        return Err(
            "[boot] count_fallback is a Raspberry Pi firmware setting: this board's record is \"uboot\""
                .to_string(),
        );
    }
    let config = bays::config_digest(&boot.config_lines);
    let mut lines = fragment.to_string();
    for line in &boot.config_lines {
        lines.push_str(line);
        lines.push('\n');
    }
    let bay = |key: &str, value: Option<u64>| {
        value.ok_or_else(|| {
            format!(
                "[boot] {key} is not set: a bay size has no default; state it in the board file"
            )
        })
    };
    let bytes = [
        bay("bay_kernel_bytes", boot.bay_kernel_bytes)?,
        bay("bay_initramfs_bytes", boot.bay_initramfs_bytes)?,
        bay("bay_device_tree_bytes", boot.bay_device_tree_bytes)?,
        bay("bay_cmdline_bytes", boot.bay_cmdline_bytes)?,
        bay("bay_overlay_bytes", boot.bay_overlay_bytes)?,
    ];
    // The bay order of ROLES is kernel, initramfs, device-tree, cmdline, board-overlay.
    let names: Vec<String> = boot.firmware_overlays.clone();
    let layout = Layout::new([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]])?
        .with_firmware(&names, config.clone());
    let layout_file = boot_dir::LAYOUT.as_bytes().to_vec();
    // The root list of the declared set: written only when the board declares overlays or lines.
    let mut list_file = Vec::new();
    if let Some(text) = bays::firmware_list_text(&names, config.as_deref()) {
        list_file.push((bays::FIRMWARE_LIST, text.into_bytes()));
    }
    match family {
        Family::Tryboot => Ok(Parts {
            family,
            layout,
            root: [
                vec![
                    ("slot-layout.toml", layout_file),
                    (
                        "config.txt",
                        with_fragment(boot_dir::HEADER, config_for(count_fallback), &lines),
                    ),
                    (
                        "tryboot.txt",
                        with_fragment(boot_dir::HEADER, TRYBOOT_TXT, &lines),
                    ),
                ],
                list_file,
            ]
            .concat(),
            fixed: vec![Fixed {
                path: OVERLAY_README,
                bytes: boot_dir::OVERLAY_README_TEXT.as_bytes().to_vec(),
                size: boot_dir::OVERLAY_README_TEXT.len() as u64,
            }],
        }),
        Family::UbootBlock => Ok(Parts {
            family,
            layout,
            root: [
                vec![
                    ("slot-layout.toml", layout_file),
                    (
                        lemnos_slot_store::uboot_env::LOCATOR,
                        boot_dir::uboot_place(board)?.encode().into_bytes(),
                    ),
                    ("boot.cmd", boot_dir::uboot_script(board)?.into_bytes()),
                ],
                list_file,
            ]
            .concat(),
            fixed: vec![Fixed {
                path: bootimage::EXTLINUX,
                bytes: Vec::new(),
                size: bootimage::EXTLINUX_BYTES,
            }],
        }),
    }
}

/// A board file read, its SoC map loaded and its overlay generated: what every command that writes
/// from a board needs. Refusals name the file.
pub(crate) fn load_board(
    path: &str,
    socs: Option<&str>,
) -> Result<(BoardFile, lemnos_devtree::Output), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let map = super::load_map(&board, socs)?;
    let generated = lemnos_devtree::generate(&board, &map, &super::driver_manifests())
        .map_err(|e| format!("{path}: {e}"))?;
    Ok((board, generated))
}

/// The five payloads from `dir`, in [`ROLES`] order: each role's file named exactly as the role,
/// and nothing else in the directory. A missing role or an extra file is refused by name.
pub fn read_payloads(dir: &Path) -> Result<[Vec<u8>; 5], String> {
    let names: Vec<String> = ROLES.iter().map(|r| r.role.to_string()).collect();
    let mut present: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    present.sort();
    // The files a slot has on the boot partition are the roles: a file of any other name is refused,
    // in the words the install uses for a manifest that lists one (`bays::refuse_unstored`).
    let extra: Vec<String> = present
        .iter()
        .filter(|name| !names.contains(name))
        .map(|name| format!("{name:?}"))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "{}: the payload directory holds {}, which this board's boot partition does not store (it stores: {}): nothing was written",
            dir.display(),
            extra.join(", "),
            names.join(", ")
        ));
    }
    for name in &names {
        if !present.contains(name) {
            return Err(format!("{}: the {name} payload is missing", dir.display()));
        }
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    for name in &names {
        let path = dir.join(name);
        out.push(std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    out.try_into()
        .map_err(|_| "five payloads expected".to_string())
}

/// The options of one run.
#[derive(Default)]
struct Options {
    layout_of: Option<String>,
    socs: Option<String>,
    slot: Option<String>,
    payload: Option<String>,
    firmware: Option<String>,
    dtmerge: Option<String>,
    size: Option<String>,
    out: Option<String>,
    min_size: bool,
    verify: Option<String>,
    add_to: Option<String>,
    set: Option<String>,
    replace_trial: bool,
    changed_out: Option<String>,
    undo_out: Option<String>,
    undo: Option<String>,
    of: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let usage = "usage: lemnos-ctl boot-partition --layout-of BOARD [--socs DIR] [--slot ID --payload DIR [--firmware DIR] --size BYTES --out FILE] | --min-size [--payload DIR [--firmware DIR]] | --verify FILE [--layout-of BOARD [--socs DIR]] [--dtmerge TOOL] | --add-to DUMP --layout-of BOARD [--socs DIR] --slot ID --payload DIR [--firmware DIR] [--set N] [--replace-trial] --out FILE --changed-out LIST --undo-out UNDO | --undo UNDO --of FILE --out FILE";
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--min-size" {
            o.min_size = true;
            i += 1;
            continue;
        }
        if flag == "--replace-trial" {
            o.replace_trial = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).cloned().ok_or_else(|| usage.to_string())?;
        match flag {
            "--layout-of" => o.layout_of = Some(value),
            "--socs" => o.socs = Some(value),
            "--slot" => o.slot = Some(value),
            "--payload" => o.payload = Some(value),
            "--firmware" => o.firmware = Some(value),
            "--dtmerge" => o.dtmerge = Some(value),
            "--size" => o.size = Some(value),
            "--out" => o.out = Some(value),
            "--verify" => o.verify = Some(value),
            "--add-to" => o.add_to = Some(value),
            "--set" => o.set = Some(value),
            "--changed-out" => o.changed_out = Some(value),
            "--undo-out" => o.undo_out = Some(value),
            "--undo" => o.undo = Some(value),
            "--of" => o.of = Some(value),
            _ => return Err(usage.to_string()),
        }
        i += 2;
    }
    Ok(o)
}

/// Runs `boot-partition` with `args` (after the subcommand's name), printing to `out`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let o = parse(args)?;
    if o.dtmerge.is_some() && o.verify.is_none() {
        return Err(
            "--dtmerge goes with --verify: it checks the host merge of an image".to_string(),
        );
    }
    if let Some(file) = &o.verify {
        return verify_file(&o, file, out);
    }
    if let Some(undo) = &o.undo {
        let of =
            o.of.as_deref()
                .ok_or("--undo needs --of FILE, the dump the files were added to")?;
        let to = o.out.as_deref().ok_or("--undo needs --out FILE")?;
        return crate::boot_add::undo(undo, of, to, out);
    }
    if o.add_to.is_none()
        && (o.set.is_some()
            || o.replace_trial
            || o.changed_out.is_some()
            || o.undo_out.is_some()
            || o.of.is_some())
    {
        return Err(
            "--set, --replace-trial, --changed-out and --undo-out go with --add-to, and --of with --undo"
                .to_string(),
        );
    }
    let board_path = o
        .layout_of
        .as_deref()
        .ok_or("boot-partition needs --layout-of BOARD")?;
    let text = std::fs::read_to_string(board_path).map_err(|e| format!("{board_path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{board_path}: {e}"))?;
    let map = super::load_map(&board, o.socs.as_deref())?;
    let generated = lemnos_devtree::generate(&board, &map, &super::driver_manifests())
        .map_err(|e| format!("{board_path}: {e}"))?;
    let parts = board_parts(&board, &generated.boot).map_err(|e| format!("{board_path}: {e}"))?;
    if o.min_size {
        return min_size(&parts, o.payload.as_deref(), o.firmware.as_deref(), out);
    }
    if let Some(dump) = &o.add_to {
        if o.size.is_some() {
            return Err("--size goes with a new image: --add-to keeps the dump's size".to_string());
        }
        return crate::boot_add::add(
            &parts,
            &crate::boot_add::Args {
                dump,
                slot: o.slot.as_deref().ok_or("--add-to needs --slot ID")?,
                payload: o.payload.as_deref().ok_or("--add-to needs --payload DIR")?,
                firmware: o.firmware.as_deref(),
                set: o.set.as_deref(),
                replace_trial: o.replace_trial,
                out: o.out.as_deref().ok_or("--add-to needs --out FILE")?,
                changed_out: o
                    .changed_out
                    .as_deref()
                    .ok_or("--add-to needs --changed-out LIST")?,
                undo_out: o
                    .undo_out
                    .as_deref()
                    .ok_or("--add-to needs --undo-out UNDO")?,
            },
            out,
        );
    }
    let slot = o.slot.as_deref().ok_or("--out needs --slot ID")?;
    let payload_dir = o.payload.as_deref().ok_or("--out needs --payload DIR")?;
    let size: u64 = o
        .size
        .as_deref()
        .ok_or("--out needs --size BYTES")?
        .parse()
        .map_err(|_| "--size is a number of bytes".to_string())?;
    let file = o.out.as_deref().ok_or("--out needs --out FILE")?;
    if Path::new(file).exists() {
        return Err(format!(
            "{file} exists: the image is written to a new file only"
        ));
    }
    let payloads = read_payloads(Path::new(payload_dir))?;
    let firmware = read_firmware(o.firmware.as_deref(), &overlay_names(&parts.layout))?;
    let refs: Vec<&[u8]> = payloads
        .iter()
        .chain(firmware.iter())
        .map(Vec::as_slice)
        .collect();
    let image = bootimage::build(&parts, size, slot, &refs)?;
    std::fs::write(file, &image).map_err(|e| format!("{file}: {e}"))?;
    writeln!(
        out,
        "wrote {file}: {} bytes, slot {slot}, minimum {} bytes",
        image.len(),
        bootimage::min_size(&parts)?
    )
    .map_err(|e| e.to_string())
}

/// `--min-size`: the minimum in bytes, one line (decimal digits only); with `--payload`, that line
/// first, then `role stored_bytes bay_bytes` per role.
fn min_size(
    parts: &Parts,
    payload: Option<&str>,
    firmware: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let Some(dir) = payload else {
        writeln!(out, "{}", bootimage::min_size(parts)?).map_err(|e| e.to_string())?;
        return Ok(());
    };
    // The minimum first, on a line of its own (the same line as without `--payload`), then one line
    // per role and one per firmware overlay.
    writeln!(out, "{}", bootimage::min_size(parts)?).map_err(|e| e.to_string())?;
    let payloads = read_payloads(Path::new(dir))?;
    let overlays = read_firmware(firmware, &overlay_names(&parts.layout))?;
    let mut over = Vec::new();
    for (i, r) in ROLES.iter().enumerate() {
        let stored = payloads[i].len() as u64;
        let bay = parts.layout.bytes[i];
        writeln!(out, "{} {stored} {bay}", r.role).map_err(|e| e.to_string())?;
        if stored > bay {
            over.push(r.role.to_string());
        }
    }
    for (j, (path, bay)) in parts.layout.firmware.iter().enumerate() {
        let stored = overlays[j].len() as u64;
        writeln!(out, "{path} {stored} {bay}").map_err(|e| e.to_string())?;
        if stored > *bay {
            over.push(path.clone());
        }
    }
    if over.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the payload of {} is bigger than its bay: rebuild with a larger bay or a smaller payload",
            over.join(", ")
        ))
    }
}

/// The host merge of committed bays with `tool`, in a scratch directory this run makes and removes.
fn host_merge(boot: &BootSpec, tool: &str, sets: &[Committed]) -> Result<Vec<Finding>, String> {
    // One directory per call: runs in one process (the tests) must not share files.
    static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!("lemnos-host-merge-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let findings = crate::boot_merge::merge_findings(boot, tool, sets, &work);
    // Only the files this call wrote: its own directory, removed as a whole.
    let _ = std::fs::remove_dir_all(&work);
    Ok(findings)
}

/// `--verify FILE`: the image or dump is read, never written; one line per finding.
fn verify_file(o: &Options, file: &str, out: &mut dyn Write) -> Result<(), String> {
    let board_path = o
        .layout_of
        .as_deref()
        .ok_or("--verify needs --layout-of BOARD for the bay sizes")?;
    let text = std::fs::read_to_string(board_path).map_err(|e| format!("{board_path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{board_path}: {e}"))?;
    let map = super::load_map(&board, o.socs.as_deref())?;
    let generated = lemnos_devtree::generate(&board, &map, &super::driver_manifests())
        .map_err(|e| format!("{board_path}: {e}"))?;
    let parts = board_parts(&board, &generated.boot).map_err(|e| format!("{board_path}: {e}"))?;
    let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let mut mem = bootimage::Mem(bytes);
    let mut findings: Vec<Finding> = bootimage::verify(&mut mem, &parts);
    // The host merge needs overlays to merge; a board with none is verified exactly as before.
    let merging = !board.boot.firmware_overlays.is_empty();
    if let Some(tool) = o.dtmerge.as_ref().filter(|_| merging) {
        match bootimage::committed(&mut mem, &parts) {
            Ok(sets) => findings.extend(host_merge(&board.boot, tool, &sets)?),
            Err(e) => findings.push(Finding { ok: false, text: e }),
        }
    }
    let mut bad = 0;
    for f in &findings {
        if !f.ok {
            bad += 1;
        }
        writeln!(out, "{} {}", if f.ok { "ok " } else { "BAD" }, f.text)
            .map_err(|e| e.to_string())?;
    }
    if bad == 0 {
        Ok(())
    } else {
        Err(format!("{file}: {bad} finding(s) failed"))
    }
}

#[cfg(test)]
mod count_fallback_tests {
    use super::*;

    /// The reference board's `[boot]` table with `count_fallback` (if any) written, its soc replaced.
    fn board(soc: &str, count: &str) -> BoardFile {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/boards/reference.toml"),
        )
        .unwrap();
        let text = text
            .replace("soc = \"bcm2712-rp1\"", &format!("soc = \"{soc}\""))
            .replacen("[boot]\n", &format!("[boot]\n{count}\n"), 1);
        lemnos_host::board::parse(&text).unwrap()
    }

    fn config_of(parts: &Parts) -> String {
        let (_, text) = parts
            .root
            .iter()
            .find(|(name, _)| *name == "config.txt")
            .expect("a Pi board has config.txt");
        String::from_utf8(text.clone()).unwrap()
    }

    #[test]
    fn the_pi5_family_gets_the_count_fallback_and_refuses_a_fallback_anywhere_else() {
        let pi5 = board("bcm2712-rp1", "");
        let parts = board_parts(&pi5, "").unwrap();
        assert!(config_of(&parts).contains("[boot_count>3]"));
        assert!(config_of(&parts).contains("include previous.txt"));

        // Asked for elsewhere: refused by name, before any image is made.
        let rk = board("rk3566", "count_fallback = true");
        let why = board_parts(&rk, "").expect_err("refused");
        assert!(why.contains("count_fallback = true"), "{why}");
        assert!(why.contains("Raspberry Pi 5 family only"), "{why}");
    }

    #[test]
    fn a_pi_outside_the_pi5_family_gets_no_filter_and_no_previous_include() {
        // A Pi 5 with the fallback switched off writes no filter either.
        let pi5_off = board("bcm2712-rp1", "count_fallback = false");
        let config = config_of(&board_parts(&pi5_off, "").unwrap());
        // Whole lines only: the generated header comment names the filter in words.
        let lines: Vec<&str> = config.lines().collect();
        assert!(
            !lines.iter().any(|l| l.starts_with("[boot_count")),
            "{config}"
        );
        assert!(!lines.contains(&"include previous.txt"), "{config}");
        assert!(lines.contains(&"include default.txt"), "{config}");
    }
}
