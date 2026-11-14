//! `lemnos-ctl bundle`: writes a slot bundle and verifies one.
//!
//! A format is written by the program that reads it. Everything about the bundle's bytes is
//! `lemnos_slot_store::bundle` (the member order, the manifest and its bytes, the signed message and
//! the record, the tar); this module reads the board file and the inputs, signs, and prints.
//!
//! ```text
//! lemnos-ctl bundle --layout-of BOARD [--socs DIR] --slot ID --payload DIR --rootfs FILE
//!                   --verity-table FILE --product P --version V [--variant TEXT] [--kernel TEXT]
//!                   [--display-name TEXT] [--key-file FILE [--expect-key-id HEX]]
//!                   [--payload-out DIR] --out FILE
//! lemnos-ctl bundle --verify FILE [--layout-of BOARD [--socs DIR]] [--pubkey FILE]
//! ```
//!
//! `--payload DIR` is the directory `boot-partition --payload` takes (the five roles by name).
//! `--rootfs FILE` is the erofs image with its dm-verity hash tree appended, and `--verity-table`
//! the builder's `key value` facts about it; the tool holds one to the other and refuses a mismatch
//! by name. `--key-file` is a raw Ed25519 seed (32 bytes, or 64 hex digits); without it the bundle
//! is unsigned, and the output says so.

use crate::boot_dir;
use crate::boot_partition::{board_parts, load_board, read_payloads};
use ed25519_dalek::{Signer as _, SigningKey};
use lemnos_driver_system_slots::{TrustedKey, parse_keys, signature_check};
use lemnos_slot_store::bays::ROLES;
use lemnos_slot_store::bundle::{self, Build, Context, Finding, Rules, Signer, State, hex};
use lemnos_slot_store::manifest::Overlay;
use lemnos_slot_store::signing::{self, Check};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

const USAGE: &str = "usage: lemnos-ctl bundle --layout-of BOARD [--socs DIR] --slot ID --payload DIR --rootfs FILE --verity-table FILE --product P --version V [--variant TEXT] [--kernel TEXT] [--display-name TEXT] [--key-file FILE [--expect-key-id HEX]] [--payload-out DIR] --out FILE | --verify FILE [--layout-of BOARD [--socs DIR]] [--pubkey FILE]";

#[derive(Default)]
struct Options {
    layout_of: Option<String>,
    socs: Option<String>,
    slot: Option<String>,
    payload: Option<String>,
    rootfs: Option<String>,
    product: Option<String>,
    version: Option<String>,
    variant: Option<String>,
    kernel: Option<String>,
    display_name: Option<String>,
    verity_table: Option<String>,
    key_file: Option<String>,
    expect_key_id: Option<String>,
    payload_out: Option<String>,
    out: Option<String>,
    verify: Option<String>,
    pubkey: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = args.get(i + 1).cloned().ok_or_else(|| USAGE.to_string())?;
        let slot = match flag {
            "--layout-of" => &mut o.layout_of,
            "--socs" => &mut o.socs,
            "--slot" => &mut o.slot,
            "--payload" => &mut o.payload,
            "--rootfs" => &mut o.rootfs,
            "--product" => &mut o.product,
            "--version" => &mut o.version,
            "--variant" => &mut o.variant,
            "--kernel" => &mut o.kernel,
            "--display-name" => &mut o.display_name,
            "--verity-table" => &mut o.verity_table,
            "--expect-key-id" => &mut o.expect_key_id,
            "--key-file" => &mut o.key_file,
            "--payload-out" => &mut o.payload_out,
            "--out" => &mut o.out,
            "--verify" => &mut o.verify,
            "--pubkey" => &mut o.pubkey,
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

/// The board's rules for a slot, from its board file: the record's form and bays, the command
/// line's prefix and sources, the board id and the overlay marker.
fn rules_of(
    path: &str,
    socs: Option<&str>,
) -> Result<(Rules, lemnos_host::board::BoardFile), String> {
    let (board, generated) = load_board(path, socs)?;
    let parts = board_parts(&board, &generated.boot).map_err(|e| format!("{path}: {e}"))?;
    let (prefix, boot_sources) =
        boot_dir::slot_words(&board).map_err(|e| format!("{path}: {e}"))?;
    let overlay = boot_dir::overlay_marker(&board.board.id, generated.overlay_sha256.as_deref())
        .map_err(|e| format!("{path}: {e}"))?
        .map(|(board, sha256)| Overlay { board, sha256 });
    Ok((
        Rules {
            board: board.board.id.clone(),
            prefix,
            boot_sources,
            family: parts.family,
            layout: parts.layout,
            overlay,
        },
        board,
    ))
}

/// Runs `bundle` with `args` (after the subcommand's name), printing to `out`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let o = parse(args)?;
    if o.verify.is_some() {
        return verify(&o, out);
    }
    write_bundle(&o, out)
}

fn need<'a>(value: &'a Option<String>, flag: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .ok_or_else(|| format!("bundle needs {flag}: {USAGE}"))
}

fn write_bundle(o: &Options, out: &mut dyn Write) -> Result<(), String> {
    if o.pubkey.is_some() {
        return Err("--pubkey is for --verify: a bundle is signed with --key-file".into());
    }
    let board_path = need(&o.layout_of, "--layout-of BOARD")?;
    let slot = need(&o.slot, "--slot ID")?;
    let payload_dir = need(&o.payload, "--payload DIR")?;
    let rootfs_path = need(&o.rootfs, "--rootfs FILE")?;
    let product = need(&o.product, "--product P")?;
    let version = need(&o.version, "--version V")?;
    let table_path = need(&o.verity_table, "--verity-table FILE")?;
    let file = need(&o.out, "--out FILE")?;
    if o.expect_key_id.is_some() && o.key_file.is_none() {
        return Err(
            "--expect-key-id names the key a --key-file must have: it needs --key-file".into(),
        );
    }
    if Path::new(file).exists() {
        return Err(format!(
            "{file} exists: the bundle is written to a new file only"
        ));
    }
    if let Some(dir) = &o.payload_out
        && Path::new(dir).exists()
    {
        return Err(format!(
            "{dir} exists: the resolved boot files are written to a new directory only"
        ));
    }
    let (rules, _) = rules_of(board_path, o.socs.as_deref())?;
    let key = match &o.key_file {
        Some(path) => Some(read_key(path)?),
        None => None,
    };
    if let (Some(key), Some(want)) = (&key, &o.expect_key_id) {
        let got = hex(&signing::key_id(key.verifying_key().as_bytes()));
        if !want.eq_ignore_ascii_case(&got) {
            return Err(format!(
                "the key file's key id is {got}; the build expects {}",
                want.to_ascii_lowercase()
            ));
        }
    }
    let table_text =
        std::fs::read_to_string(table_path).map_err(|e| format!("{table_path}: {e}"))?;
    let table = lemnos_slot_store::verity::Table::parse(&table_text)
        .map_err(|e| format!("{table_path}: {e}"))?;
    let payloads = read_payloads(Path::new(payload_dir))?;
    let mut rootfs = std::fs::File::open(rootfs_path).map_err(|e| format!("{rootfs_path}: {e}"))?;
    let key_id = key
        .as_ref()
        .map(|k| signing::key_id(k.verifying_key().as_bytes()));
    let sign = |message: &[u8]| k_sign(key.as_ref(), message);
    let built = bundle::build(
        &Build {
            id: slot,
            product,
            version,
            variant: o.variant.as_deref().unwrap_or(""),
            kernel: o.kernel.as_deref().unwrap_or(""),
            display_name: o.display_name.as_deref().unwrap_or(""),
            table: &table,
            rules: &rules,
            signer: key_id.map(|key_id| Signer {
                key_id,
                sign: &sign,
            }),
        },
        payloads,
        &mut rootfs,
    )?;
    // Nothing is written until every refusal has been made.
    let mut sink = Hashing::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)
            .map_err(|e| format!("{file}: {e}"))?,
    );
    let written =
        bundle::write_to(&built, &mut rootfs, &mut sink).and_then(|()| sink.inner.sync_all());
    if let Err(e) = written {
        // Only the file this run just created.
        let _ = std::fs::remove_file(file);
        return Err(format!("{file}: {e}"));
    }
    let manifest = &built.manifest;
    let say =
        |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(|e| e.to_string());
    say(
        out,
        format!(
            "slot {}: product {}, version {}, board {}",
            manifest.id, manifest.product, manifest.version, manifest.board
        ),
    )?;
    say(out, format!("root hash {}", manifest.verity.root_hash))?;
    match key_id {
        Some(id) => say(out, format!("signed with key id {}", hex(&id)))?,
        None => say(out, "unsigned".to_string())?,
    }
    if let Some(dir) = &o.payload_out {
        write_payload_out(dir, &built.payloads)?;
        say(out, format!("payload written to {dir}"))?;
    }
    say(
        out,
        format!(
            "wrote {file}: {} bytes, SHA-256 {}",
            sink.bytes,
            hex(&sink.sha.finalize())
        ),
    )
}

/// The five boot files as the bundle holds them, in the role layout `boot-partition --payload` takes.
fn write_payload_out(dir: &str, payloads: &[Vec<u8>; 5]) -> Result<(), String> {
    std::fs::create_dir(dir).map_err(|e| format!("{dir}: {e}"))?;
    for (role, bytes) in ROLES.iter().zip(payloads) {
        let path = Path::new(dir).join(role.role);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// A writer that counts and hashes what passes through it.
struct Hashing {
    inner: std::fs::File,
    sha: Sha256,
    bytes: u64,
}

impl Hashing {
    fn new(inner: std::fs::File) -> Self {
        Self {
            inner,
            sha: Sha256::new(),
            bytes: 0,
        }
    }
}

impl Write for Hashing {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.sha.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn k_sign(key: Option<&SigningKey>, message: &[u8]) -> [u8; 64] {
    // Only called when a key was given.
    key.map_or([0; 64], |k| k.sign(message).to_bytes())
}

/// The signing key of `path`: a raw Ed25519 seed, exactly 32 bytes or exactly 64 hex digits, with at
/// most one trailing newline. The file is opened once and its mode is read from the open
/// descriptor: one readable by group or others is refused. Refusals name the file and the shape
/// found, never the bytes. The buffers that held the seed are overwritten before they are dropped
/// (best effort: the workspace has no zeroizing facility, and the signing key's own copy is the
/// library's).
fn read_key(path: &str) -> Result<SigningKey, String> {
    use std::os::unix::fs::MetadataExt;
    let mut file = std::fs::File::open(path).map_err(|e| format!("--key-file {path}: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("--key-file {path}: {e}"))?;
    let mode = meta.mode() & 0o7777;
    if !meta.is_file() {
        return Err(format!("the key file {path} is not a regular file"));
    }
    if mode & 0o077 != 0 {
        return Err(format!(
            "the key file {path} is mode {mode:04o}: it must not be readable by group or others (0600 or 0400)"
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("--key-file {path}: {e}"))?;
    let result = seed_of(&bytes, path);
    bytes.fill(0);
    let mut seed = result?;
    let key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    Ok(key)
}

fn seed_of(bytes: &[u8], path: &str) -> Result<[u8; 32], String> {
    let shape = || {
        format!(
            "the key file {path} is {} bytes: it must be a raw 32-byte Ed25519 seed, or 64 hex digits, with at most one trailing newline",
            bytes.len()
        )
    };
    let body = match bytes.len() {
        32 => bytes,
        33 if bytes[32] == b'\n' => &bytes[..32],
        64 => bytes,
        65 if bytes[64] == b'\n' => &bytes[..64],
        _ => return Err(shape()),
    };
    let mut seed = [0u8; 32];
    if body.len() == 32 {
        seed.copy_from_slice(body);
        return Ok(seed);
    }
    for (at, byte) in seed.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&body[at * 2..at * 2 + 2]).ok();
        *byte = pair
            .filter(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|p| u8::from_str_radix(p, 16).ok())
            .ok_or_else(|| {
                format!("the key file {path} is 64 characters and not all of them hex digits")
            })?;
    }
    Ok(seed)
}

/// The keys a signed bundle is checked against: `--pubkey FILE` (a bare 64-hex public key, or the board's
/// key-list format: 64 hex digits, a space, the owner), else with `--layout-of` the board's `[security]
/// keys` file when it can be read. `Err` is the line to show when a named file can't be used.
fn trusted_keys(
    o: &Options,
    board: Option<&lemnos_host::board::BoardFile>,
) -> Result<Vec<TrustedKey>, String> {
    let (path, explicit) = match (
        &o.pubkey,
        board.and_then(|b| b.security.as_ref()?.keys.clone()),
    ) {
        (Some(path), _) => (path.clone(), true),
        (None, Some(path)) => (path, false),
        (None, None) => return Ok(Vec::new()),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let line = text.trim();
            let text = if lemnos_slot_store::manifest::is_hex(line, 64) {
                format!("{line} pubkey")
            } else {
                text
            };
            parse_keys(&text).map_err(|why| format!("{path}: {why}"))
        }
        Err(e) if explicit => Err(format!("--pubkey {path}: {e}")),
        Err(e) => Err(format!(
            "the board's key file {path} can't be read here ({e}); pass --pubkey FILE"
        )),
    }
}

fn verify(o: &Options, out: &mut dyn Write) -> Result<(), String> {
    for (flag, given) in [
        ("--slot", &o.slot),
        ("--payload", &o.payload),
        ("--rootfs", &o.rootfs),
        ("--product", &o.product),
        ("--version", &o.version),
        ("--out", &o.out),
        ("--key-file", &o.key_file),
        ("--verity-table", &o.verity_table),
        ("--expect-key-id", &o.expect_key_id),
        ("--payload-out", &o.payload_out),
    ] {
        if given.is_some() {
            return Err(format!("{flag} is for writing a bundle, not for --verify"));
        }
    }
    let file = o.verify.as_deref().unwrap_or_default();
    let rules = match &o.layout_of {
        Some(path) => Some(rules_of(path, o.socs.as_deref())?),
        None => None,
    };
    let keys = trusted_keys(o, rules.as_ref().map(|(_, b)| b));
    let mut input = std::fs::File::open(file).map_err(|e| format!("{file}: {e}"))?;
    // What to say when the signature's key is not one that can be checked.
    let note = match &keys {
        Err(why) => why.clone(),
        Ok(list) if list.is_empty() => {
            "no key list given (--pubkey FILE, or the board's [security] keys)".to_string()
        }
        Ok(_) => "the key is not in the key list".to_string(),
    };
    let check = |manifest: &[u8], sig: &[u8]| -> (State, String) {
        let keys = match &keys {
            Ok(keys) => keys.as_slice(),
            Err(_) => &[],
        };
        let key_id = signing::parse_record(sig).map(|r| hex(&r.key_id));
        match signature_check(keys, manifest, sig) {
            Check::Unsigned => (State::Ok, "unsigned".into()),
            Check::Malformed(why) => (State::Bad, why),
            Check::UnknownKey => {
                let id = key_id.unwrap_or_default();
                (
                    State::Skipped,
                    format!("signed by key id {id}: not checked, {note}"),
                )
            }
            Check::BadSignature { owner } => (
                State::Bad,
                format!(
                    "signed by key id {}: the signature does NOT verify with the key of {owner}",
                    key_id.unwrap_or_default()
                ),
            ),
            Check::Verified { .. } => (
                State::Ok,
                format!(
                    "signed by key id {}: signature valid",
                    key_id.unwrap_or_default()
                ),
            ),
        }
    };
    let findings = bundle::verify(
        &mut input,
        &Context {
            rules: rules.as_ref().map(|(r, _)| r),
            signature: &check,
        },
    );
    report(file, &findings, out)
}

fn report(file: &str, findings: &[Finding], out: &mut dyn Write) -> Result<(), String> {
    let mut bad = 0;
    for f in findings {
        let tag = match f.state {
            State::Ok => "ok ",
            State::Bad => {
                bad += 1;
                "BAD"
            }
            State::Skipped => "n/a",
        };
        writeln!(out, "{tag} {}", f.text).map_err(|e| e.to_string())?;
    }
    if bad == 0 {
        writeln!(out, "ok").map_err(|e| e.to_string())
    } else {
        writeln!(out, "refused: {bad}").map_err(|e| e.to_string())?;
        Err(format!("{file}: {bad} check(s) failed"))
    }
}
