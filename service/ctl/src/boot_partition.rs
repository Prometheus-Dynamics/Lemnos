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
//! one line per finding, and a non-zero exit when any finding fails.

use crate::boot_dir::{self, with_fragment};
use lemnos_host::board::BoardFile;
use lemnos_slot_store::bays::{Layout, ROLES};
use lemnos_slot_store::bootimage::{self, Family, Finding, Fixed, Parts};
use lemnos_slot_store::firmware::{CONFIG_TXT, TRYBOOT_TXT};
use lemnos_slot_store::layout::OVERLAY_README;
use std::io::Write;
use std::path::Path;

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
    let layout = Layout::new([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]])?;
    let layout_file = boot_dir::LAYOUT.as_bytes().to_vec();
    match family {
        Family::Tryboot => Ok(Parts {
            family,
            layout,
            root: vec![
                ("slot-layout.toml", layout_file),
                (
                    "config.txt",
                    with_fragment(boot_dir::HEADER, CONFIG_TXT, fragment),
                ),
                (
                    "tryboot.txt",
                    with_fragment(boot_dir::HEADER, TRYBOOT_TXT, fragment),
                ),
            ],
            fixed: vec![Fixed {
                path: OVERLAY_README,
                bytes: boot_dir::OVERLAY_README_TEXT.as_bytes().to_vec(),
                size: boot_dir::OVERLAY_README_TEXT.len() as u64,
            }],
        }),
        Family::UbootBlock => Ok(Parts {
            family,
            layout,
            root: vec![
                ("slot-layout.toml", layout_file),
                (
                    lemnos_slot_store::uboot_env::LOCATOR,
                    boot_dir::uboot_place(board)?.encode().into_bytes(),
                ),
                ("boot.cmd", boot_dir::uboot_script(board)?.into_bytes()),
            ],
            fixed: vec![Fixed {
                path: bootimage::EXTLINUX,
                bytes: Vec::new(),
                size: bootimage::EXTLINUX_BYTES,
            }],
        }),
    }
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
    size: Option<String>,
    out: Option<String>,
    min_size: bool,
    verify: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let usage = "usage: lemnos-ctl boot-partition --layout-of BOARD [--socs DIR] [--slot ID --payload DIR --size BYTES --out FILE] | --min-size [--payload DIR] | --verify FILE";
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--min-size" {
            o.min_size = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).cloned().ok_or_else(|| usage.to_string())?;
        match flag {
            "--layout-of" => o.layout_of = Some(value),
            "--socs" => o.socs = Some(value),
            "--slot" => o.slot = Some(value),
            "--payload" => o.payload = Some(value),
            "--size" => o.size = Some(value),
            "--out" => o.out = Some(value),
            "--verify" => o.verify = Some(value),
            _ => return Err(usage.to_string()),
        }
        i += 2;
    }
    Ok(o)
}

/// Runs `boot-partition` with `args` (after the subcommand's name), printing to `out`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let o = parse(args)?;
    if let Some(file) = &o.verify {
        return verify_file(&o, file, out);
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
        return min_size(&parts, o.payload.as_deref(), out);
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
    let refs: [&[u8]; 5] = std::array::from_fn(|i| payloads[i].as_slice());
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
fn min_size(parts: &Parts, payload: Option<&str>, out: &mut dyn Write) -> Result<(), String> {
    let Some(dir) = payload else {
        writeln!(out, "{}", bootimage::min_size(parts)?).map_err(|e| e.to_string())?;
        return Ok(());
    };
    // The minimum first, on a line of its own (the same line as without `--payload`), then one line
    // per role.
    writeln!(out, "{}", bootimage::min_size(parts)?).map_err(|e| e.to_string())?;
    let payloads = read_payloads(Path::new(dir))?;
    let mut over = Vec::new();
    for (i, r) in ROLES.iter().enumerate() {
        let stored = payloads[i].len() as u64;
        let bay = parts.layout.bytes[i];
        writeln!(out, "{} {stored} {bay}", r.role).map_err(|e| e.to_string())?;
        if stored > bay {
            over.push(r.role);
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
    let findings: Vec<Finding> = bootimage::verify(&mut bootimage::Mem(bytes), &parts);
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
