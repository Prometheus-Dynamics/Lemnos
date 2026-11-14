//! `lemnos-ctl data-partition`: the data partition's tree for a board's first slot, written offline,
//! and verified read-only.
//!
//! ```text
//! lemnos-ctl data-partition --layout-of BOARD [--socs DIR] --install BUNDLE --boot-image FILE
//!                           --keys FILE --time UTC --data-size BYTES --out DIR [--switch off]
//! lemnos-ctl data-partition --verify DIR [--layout-of BOARD [--socs DIR]] [--boot-image FILE] [--deep]
//! ```
//!
//! The writer holds no logic of its own: `lemnos_driver_system_slots::offline` runs the daemon's
//! install, trial and confirm over `--out`. This module reads the board file and the files it is
//! given, passes them on, and prints the result. `--verify` reads the tree and checks what a boot
//! and the driver rely on; it writes nothing.

use crate::boot_dir;
use crate::boot_partition::{board_parts, load_board};
use lemnos_driver_system_slots::offline::{self, FirstSlot, OfflineBoard};
use lemnos_driver_system_slots::{TrustedKey, parse_keys};
use lemnos_slot_store::json::Json;
use lemnos_slot_store::manifest::{self, Manifest};
use lemnos_slot_store::store::Layout;
use lemnos_slot_store::bootimage::Mem;
use lemnos_slot_store::boot_raw::Boot;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: lemnos-ctl data-partition --layout-of BOARD [--socs DIR] --install BUNDLE --boot-image FILE --keys FILE --time UTC --data-size BYTES --out DIR [--switch off] | --verify DIR [--layout-of BOARD [--socs DIR]] [--boot-image FILE] [--deep]";

#[derive(Default)]
struct Options {
    layout_of: Option<String>,
    socs: Option<String>,
    install: Option<String>,
    boot_image: Option<String>,
    keys: Option<String>,
    time: Option<String>,
    data_size: Option<String>,
    out: Option<String>,
    switch: Option<String>,
    verify: Option<String>,
    deep: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--deep" {
            o.deep = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).cloned().ok_or_else(|| USAGE.to_string())?;
        let slot = match flag {
            "--layout-of" => &mut o.layout_of,
            "--socs" => &mut o.socs,
            "--install" => &mut o.install,
            "--boot-image" => &mut o.boot_image,
            "--keys" => &mut o.keys,
            "--time" => &mut o.time,
            "--data-size" => &mut o.data_size,
            "--out" => &mut o.out,
            "--switch" => &mut o.switch,
            "--verify" => &mut o.verify,
            _ => return Err(USAGE.to_string()),
        };
        if slot.is_some() {
            return Err(format!("{flag} is given twice"));
        }
        *slot = Some(value);
        i += 2;
    }
    Ok(o)
}

/// Runs `data-partition` with `args` (after the subcommand's name), printing to `out`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let o = parse(args)?;
    if o.verify.is_some() {
        return verify(&o, out);
    }
    write(&o, out)
}

fn need<'a>(value: &'a Option<String>, flag: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .ok_or_else(|| format!("data-partition needs {flag}: {USAGE}"))
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the shape every stamp of the output carries.
fn valid_time(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// The board's facts the writer checks the bundle against, from the board file.
fn board_facts(path: &str, socs: Option<&str>) -> Result<OfflineBoard, String> {
    let (board, generated) = load_board(path, socs)?;
    let parts = board_parts(&board, &generated.boot).map_err(|e| format!("{path}: {e}"))?;
    let (boot_args, boot_sources) =
        boot_dir::slot_words(&board).map_err(|e| format!("{path}: {e}"))?;
    Ok(OfflineBoard {
        id: board.board.id.clone(),
        boot_args,
        boot_sources,
        layout: parts.layout,
    })
}

/// The board's trust root from `--keys FILE`: a key list, or one bare 64-hex public key.
fn keys_of(path: &str) -> Result<Vec<TrustedKey>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!("the key file {path} can't be read here ({e}); pass --keys FILE")
    })?;
    let line = text.trim();
    let text = if manifest::is_hex(line, 64) {
        format!("{line} pubkey")
    } else {
        text
    };
    parse_keys(&text).map_err(|why| format!("{path}: {why}"))
}

fn write(o: &Options, out: &mut dyn Write) -> Result<(), String> {
    let board_path = need(&o.layout_of, "--layout-of BOARD")?;
    let bundle_path = need(&o.install, "--install BUNDLE")?;
    let image_path = need(&o.boot_image, "--boot-image FILE")?;
    let keys_path = need(&o.keys, "--keys FILE")?;
    let time = need(&o.time, "--time UTC")?;
    let data_size = need(&o.data_size, "--data-size BYTES")?;
    let dir = need(&o.out, "--out DIR")?;
    match o.switch.as_deref() {
        None | Some("off") => {}
        Some("on") => {
            return Err(
                "the switch is not set by this command: set it after the first boot with lemnos-ctl security set on"
                    .into(),
            );
        }
        Some(other) => return Err(format!("--switch is off or on, not {other:?}")),
    }
    if !valid_time(time) {
        return Err(format!("--time is UTC as YYYY-MM-DDTHH:MM:SSZ, not {time:?}"));
    }
    let free: u64 = data_size
        .parse()
        .map_err(|_| format!("--data-size is a number of bytes, not {data_size:?}"))?;
    let board = board_facts(board_path, o.socs.as_deref())?;
    let keys = keys_of(keys_path)?;
    let bundle = std::fs::read(bundle_path).map_err(|e| format!("{bundle_path}: {e}"))?;
    let boot_image = std::fs::read(image_path).map_err(|e| format!("{image_path}: {e}"))?;
    let id = offline::write_first_slot(FirstSlot {
        out: Path::new(dir),
        bundle: &bundle,
        boot_image,
        keys,
        board: board.clone(),
        time,
        free,
    })?;
    let (files, bytes) = tree_size(Path::new(dir));
    let meta = std::fs::read_to_string(Path::new(dir).join("board/slots").join(format!("{id}.meta")))
        .unwrap_or_default();
    let signature = meta_value(&meta, "SIGNATURE");
    let signed = if signature == "not_checked_unsigned" {
        "unsigned".to_string()
    } else {
        format!("signature {signature} by {}", meta_value(&meta, "SIGNER"))
    };
    let root = std::fs::read_to_string(Path::new(dir).join("slots").join(format!("{id}.json")))
        .ok()
        .and_then(|text| Manifest::parse(&text).ok())
        .map(|m| m.verity.root_hash)
        .unwrap_or_default();
    writeln!(
        out,
        "wrote {dir}: {files} files, {bytes} bytes; slot {id}, board {}, root hash {root}, {signed}",
        board.id
    )
    .map_err(|e| e.to_string())
}

/// The value of `KEY` in a `KEY=value` meta text, quotes off.
fn meta_value(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
        .unwrap_or_default()
        .trim_matches('\'')
        .to_string()
}

/// Files and bytes under `root`, for the summary line.
fn tree_size(root: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

/// SHA-256 of a file, as lowercase hex.
fn sha256_of(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex(&hash.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One line per check, in the form `bundle --verify` prints: `ok`, `BAD` or `n/a`.
struct Report {
    bad: u32,
    lines: Vec<String>,
}

impl Report {
    fn ok(&mut self, text: impl Into<String>) {
        self.lines.push(format!("ok  {}", text.into()));
    }

    fn bad(&mut self, text: impl Into<String>) {
        self.bad += 1;
        self.lines.push(format!("BAD {}", text.into()));
    }

    fn check(&mut self, result: Result<(), String>, ok: &str) {
        match result {
            Ok(()) => self.ok(ok),
            Err(why) => self.bad(why),
        }
    }
}

/// `--verify DIR`: the tree's own facts, read-only. `--layout-of` adds the board's id; `--boot-image`
/// adds check 11 (the bay holds the slot).
fn verify(o: &Options, out: &mut dyn Write) -> Result<(), String> {
    for (flag, given) in [
        ("--install", &o.install),
        ("--keys", &o.keys),
        ("--time", &o.time),
        ("--data-size", &o.data_size),
        ("--out", &o.out),
        ("--switch", &o.switch),
    ] {
        if given.is_some() {
            return Err(format!("{flag} is for writing the partition, not for --verify"));
        }
    }
    let dir = PathBuf::from(o.verify.as_deref().unwrap_or_default());
    let board_id = match &o.layout_of {
        Some(path) => Some(board_facts(path, o.socs.as_deref())?.id),
        None => None,
    };
    let mut r = Report { bad: 0, lines: Vec::new() };
    let layout = Layout::new(&dir, dir.join(".boot-not-mounted"));
    let state_env = std::fs::read_to_string(dir.join("board/slots/state.env")).unwrap_or_default();
    let state_json = std::fs::read_to_string(dir.join("board/slots/state.json")).unwrap_or_default();
    let default_env = meta_value(&state_env, "DEFAULT");
    let default_json = Json::parse(&state_json)
        .and_then(|j| j.get("default").and_then(Json::text).map(str::to_string))
        .unwrap_or_default();
    if default_env.is_empty() {
        r.bad("board/slots/state.env names no default".to_string());
    } else if default_env != default_json {
        r.bad(format!(
            "state.env names default {default_env}, state.json names {default_json:?}"
        ));
    } else {
        r.ok(format!("default is {default_env} in state.env and state.json"));
    }
    if meta_value(&state_env, "STATE") != "confirmed" {
        r.bad("state is not confirmed".to_string());
    } else {
        r.ok("state is confirmed".to_string());
    }
    if !meta_value(&state_env, "TRIAL").is_empty() || !meta_value(&state_env, "STAGED").is_empty() {
        r.bad("a trial or a staged slot is still pending".to_string());
    } else {
        r.ok("no trial or staged slot pending".to_string());
    }
    let id = default_env.clone();
    if !id.is_empty() {
        verify_slot(&dir, &layout, &id, board_id.as_deref(), o.deep, &mut r);
    }
    verify_leftovers(&dir, &mut r);
    if let Some(image) = &o.boot_image {
        let bytes = std::fs::read(image).map_err(|e| format!("{image}: {e}"))?;
        let check = (|| {
            let mut boot: Boot<Mem> = offline::open_boot_image(&bytes)?;
            offline::check_bay(&mut boot, &id)
        })();
        r.check(check, &format!("the boot image's bay holds slot {id}"));
    }
    for line in &r.lines {
        writeln!(out, "{line}").map_err(|e| e.to_string())?;
    }
    if r.bad == 0 {
        writeln!(out, "ok").map_err(|e| e.to_string())
    } else {
        writeln!(out, "refused: {}", r.bad).map_err(|e| e.to_string())?;
        Err(format!("{}: {} check(s) failed", dir.display(), r.bad))
    }
}

/// The checks on one installed slot: its manifest, its image, its boot files and its metadata.
fn verify_slot(
    dir: &Path,
    layout: &Layout,
    id: &str,
    board: Option<&str>,
    deep: bool,
    r: &mut Report,
) {
    let meta = lemnos_slot_store::store::Meta::load(layout, id);
    if meta.get("GOOD") != "yes" || meta.failed() {
        r.bad(format!("slot {id} is not good, or has failed"));
    } else {
        r.ok(format!("slot {id} is good and not failed"));
    }
    let manifest_text = std::fs::read_to_string(dir.join("slots").join(format!("{id}.json")));
    let Ok(text) = manifest_text else {
        r.bad(format!("slot {id} has no manifest"));
        return;
    };
    let Ok(m) = Manifest::parse(&text) else {
        r.bad(format!("slot {id}'s manifest doesn't parse"));
        return;
    };
    if m.id != id {
        r.bad(format!("slot {id}'s manifest names {}", m.id));
    } else if let Some(board) = board
        && m.board != board
    {
        r.bad(format!("slot {id} is built for board {}, not {board}", m.board));
    } else {
        r.ok(format!("slot {id}'s manifest is for this slot and board"));
    }
    let image = dir.join("slots").join(format!("{id}.erofs"));
    match std::fs::metadata(&image) {
        Ok(meta) if meta.len() == m.image_size => r.ok(format!("slot {id}'s image is {} bytes", m.image_size)),
        Ok(meta) => r.bad(format!(
            "slot {id}'s image is {} bytes; its manifest says {}",
            meta.len(),
            m.image_size
        )),
        Err(e) => r.bad(format!("slot {id}'s image: {e}")),
    }
    if deep {
        // The image's own hash: boot does not read it (dm-verity checks the blocks it reads).
        match sha256_of(&image) {
            Ok(got) if got == m.image_sha256 => r.ok(format!("slot {id}'s image hashes to its manifest's sha256")),
            Ok(got) => r.bad(format!("slot {id}'s image hashes to {got}; its manifest says {}", m.image_sha256)),
            Err(e) => r.bad(format!("slot {id}'s image: {e}")),
        }
    }
    let listed = std::fs::read_to_string(layout.boot_list(id)).unwrap_or_default();
    let mut lines: Vec<(String, String)> = Vec::new();
    for line in listed.lines() {
        if let Some((sha, path)) = line.split_once("  ") {
            lines.push((sha.to_string(), path.to_string()));
        }
    }
    let mut boot_ok = !lines.is_empty();
    for (sha, path) in &lines {
        let stored = dir.join("slots/bootfiles").join(sha);
        match sha256_of(&stored) {
            Ok(got) if got == *sha => {}
            Ok(got) => {
                r.bad(format!("boot file {path} is stored as {sha} but hashes to {got}"));
                boot_ok = false;
            }
            Err(e) => {
                r.bad(format!("boot file {path}: {e}"));
                boot_ok = false;
            }
        }
    }
    let mut required = vec![("slot.json".to_string(), sha256_hex(text.as_bytes()))];
    required.extend(m.boot_files.iter().cloned());
    for (path, sha) in &required {
        if !lines.iter().any(|(s, p)| p == path && s == sha) {
            r.bad(format!("the boot list has no {path} with the manifest's hash"));
            boot_ok = false;
        }
    }
    if boot_ok {
        r.ok(format!("slot {id}'s boot files are stored and match its manifest"));
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Leftovers of an install that never finished: nothing of them may be in a finished partition.
fn verify_leftovers(dir: &Path, r: &mut Report) {
    let mut stray = Vec::new();
    for root in [dir.join("slots"), dir.join("board/slots")] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".install") || name.ends_with(".tmp") || name.ends_with(".new") {
                stray.push(name);
            }
        }
    }
    if stray.is_empty() {
        r.ok("no leftovers of an unfinished install".to_string());
    } else {
        r.bad(format!("leftovers of an unfinished install: {}", stray.join(", ")));
    }
}
