//! `boot-partition --verify --dtmerge TOOL`: the firmware overlays a board declares, applied on the
//! host to each committed bay's `board.dtb` with `TOOL` (the `dtmerge` of the Raspberry Pi utilities,
//! passed in by path), then the board's `[boot] expect_nodes` checked in the merged tree.
//!
//! Everything here is host work: the merge runs on this machine and nothing is loaded by firmware,
//! so every line says "applies on host, not loaded by firmware". Each overlay is applied in the order
//! the board declares, one on the last, with the parameters of the `dtoverlay=` config line that loads
//! it, as `TOOL BASE MERGED OVERLAY name=value ...` (the dtmerge command line). A board with no
//! overlays has nothing to merge and writes no line.

use std::path::Path;
use std::process::Command;

use lemnos_host::board::{BootSpec, ExpectValue};
use lemnos_slot_store::bootimage::{Committed, Finding};

/// The flattened device tree magic.
const FDT_MAGIC: u32 = 0xd00d_feed;
/// The structure block tokens.
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

/// The name and the parameters of a `dtoverlay=NAME[,PARAM...]` config line, or `None` for another line.
fn overlay_line(line: &str) -> Option<(&str, Vec<&str>)> {
    let rest = line.trim_start().strip_prefix("dtoverlay=")?;
    let mut parts = rest.split(',').map(str::trim);
    let name = parts.next()?;
    Some((name, parts.collect()))
}

/// The parameters the config line that loads overlay `name` gives it, as dtmerge takes them.
pub(crate) fn params_of(boot: &BootSpec, name: &str) -> Vec<String> {
    boot.config_lines
        .iter()
        .find_map(|line| match overlay_line(line) {
            Some((n, params)) if n == name => Some(params.iter().map(|p| p.to_string()).collect()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The value of parameter `key`, given by `key=value` on the config lines. Exactly one config line
/// must give it: none, or two, is refused by name.
pub(crate) fn param_value(boot: &BootSpec, key: &str) -> Result<String, String> {
    let mut values = Vec::new();
    for line in &boot.config_lines {
        let Some((_, params)) = overlay_line(line) else {
            continue;
        };
        for p in params {
            let Some((k, v)) = p.split_once('=') else {
                continue;
            };
            if k.trim() == key {
                values.push(v.trim().to_string());
            }
        }
    }
    match values.len() {
        1 => Ok(values.remove(0)),
        0 => Err(format!(
            "no dtoverlay= config line gives the parameter {key:?}"
        )),
        _ => Err(format!(
            "the dtoverlay= config lines give the parameter {key:?} {} times",
            values.len()
        )),
    }
}

/// A decimal or `0x` hexadecimal integer.
fn parse_int(text: &str) -> Option<i64> {
    match text.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// One 32-bit cell, big-endian.
fn cell(n: i64) -> Result<Vec<u8>, String> {
    u32::try_from(n)
        .map(|n| n.to_be_bytes().to_vec())
        .map_err(|_| format!("{n} is not a 32-bit cell (0 to 4294967295)"))
}

/// A string property's bytes: the text and its NUL.
fn string(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.push(0);
    out
}

/// The bytes an expected value stands for. A `$NAME` takes the config-line parameter `NAME`: an
/// integer when it reads as one, else the string.
pub(crate) fn expected_bytes(boot: &BootSpec, value: &ExpectValue) -> Result<Vec<u8>, String> {
    match value {
        ExpectValue::Int(n) => cell(*n),
        ExpectValue::Str(text) => match text.strip_prefix('$') {
            None => Ok(string(text)),
            Some(key) => {
                let v = param_value(boot, key)?;
                match parse_int(&v) {
                    Some(n) => cell(n),
                    None => Ok(string(&v)),
                }
            }
        },
    }
}

/// Checks `[boot] expect_nodes` by name before any image is made: the board must declare overlays,
/// each path is absolute, and each value (including every `$NAME`) resolves.
pub(crate) fn check_expect(boot: &BootSpec) -> Result<(), String> {
    if boot.expect_nodes.is_empty() {
        return Ok(());
    }
    if boot.firmware_overlays.is_empty() {
        return Err(
            "[boot] expect_nodes checks the host merge of firmware_overlays, and this board declares none"
                .to_string(),
        );
    }
    for node in &boot.expect_nodes {
        let p = node.path.as_str();
        let absolute = p.starts_with('/') && (p == "/" || (!p.ends_with('/') && !p.contains("//")));
        if !absolute {
            return Err(format!(
                "[boot] expect_nodes path {p:?} is not an absolute node path (such as /soc/gpu)"
            ));
        }
        for (name, value) in &node.properties {
            if name.is_empty() || name.contains('\0') {
                return Err(format!(
                    "[boot] expect_nodes {p}: a property name is empty or has NUL"
                ));
            }
            expected_bytes(boot, value)
                .map_err(|e| format!("[boot] expect_nodes {p} {name}: {e}"))?;
        }
    }
    Ok(())
}

/// What the merged tree holds at a node's property.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// No node at the path.
    NoNode,
    /// The node is there, without the property.
    NoProperty,
    /// The property's bytes.
    Value(Vec<u8>),
}

/// Reads a whole slice, refusing a range that runs past the blob.
fn slice(dtb: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    at.checked_add(len)
        .and_then(|end| dtb.get(at..end))
        .ok_or_else(|| "the device tree blob is truncated".to_string())
}

fn be32(dtb: &[u8], at: usize) -> Result<u32, String> {
    let b = slice(dtb, at, 4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The bytes up to the NUL that ends a name starting at `at`.
fn cstr(dtb: &[u8], at: usize) -> Result<&[u8], String> {
    let rest = dtb
        .get(at..)
        .ok_or_else(|| "the device tree blob is truncated".to_string())?;
    let end = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| "a name in the device tree blob has no NUL".to_string())?;
    Ok(&rest[..end])
}

fn align4(at: usize) -> Result<usize, String> {
    at.checked_add(3)
        .map(|a| a & !3)
        .ok_or_else(|| "the device tree blob is truncated".to_string())
}

/// The property `name` of the node at absolute `path` in a flattened device tree, read from its
/// structure block. Only what the check needs: nodes and properties, no overlays.
pub(crate) fn find(dtb: &[u8], path: &str, name: &str) -> Result<Found, String> {
    if be32(dtb, 0)? != FDT_MAGIC {
        return Err("not a flattened device tree (no FDT magic)".to_string());
    }
    let off_struct = be32(dtb, 8)? as usize;
    let off_strings = be32(dtb, 12)? as usize;
    let wanted: Vec<&[u8]> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::as_bytes)
        .collect();
    let mut stack: Vec<&[u8]> = Vec::new();
    let mut at = off_struct;
    let mut target_depth: Option<usize> = None;
    loop {
        let token = be32(dtb, at)?;
        at += 4;
        match token {
            BEGIN_NODE => {
                let name_bytes = cstr(dtb, at)?;
                at = align4(at + name_bytes.len() + 1)?;
                stack.push(name_bytes);
                if target_depth.is_none() && stack[1..] == wanted[..] {
                    target_depth = Some(stack.len());
                }
            }
            END_NODE => {
                if target_depth == Some(stack.len()) {
                    return Ok(Found::NoProperty);
                }
                stack.pop();
            }
            PROP => {
                let len = be32(dtb, at)? as usize;
                let name_off = be32(dtb, at + 4)? as usize;
                at += 8;
                let value = slice(dtb, at, len)?;
                let prop_name = cstr(dtb, off_strings + name_off)?;
                if target_depth == Some(stack.len()) && prop_name == name.as_bytes() {
                    return Ok(Found::Value(value.to_vec()));
                }
                at = align4(at + len)?;
            }
            NOP => {}
            END => break,
            other => return Err(format!("unknown token {other} in the device tree blob")),
        }
    }
    Ok(if target_depth.is_some() {
        Found::NoProperty
    } else {
        Found::NoNode
    })
}

/// A short description of property bytes for a line: a cell, a string, or a byte count.
fn describe(bytes: &[u8]) -> String {
    if bytes.len() == 4 {
        let n = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        return format!("{n} (0x{n:x})");
    }
    let printable = bytes
        .split_last()
        .filter(|(last, text)| **last == 0 && text.iter().all(|&b| (0x20..0x7f).contains(&b)));
    if let Some((_, text)) = printable {
        return format!("{:?}", String::from_utf8_lossy(text));
    }
    format!("{} bytes", bytes.len())
}

/// The first non-empty line of a tool's stderr, for a finding.
fn first_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no message")
        .to_string()
}

/// Applies each committed bay's declared overlays on the host with `tool`, then checks the board's
/// `expect_nodes` in the merged tree. `work` is a directory this run made; the files written there
/// are the bay's base, overlays and merged trees. A bay whose members do not match the declared list
/// is reported, not merged.
pub(crate) fn merge_findings(
    boot: &BootSpec,
    tool: &str,
    sets: &[Committed],
    work: &Path,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |ok: bool, text: String| out.push(Finding { ok, text });
    let declared = boot.firmware_overlays.len();
    for c in sets {
        let set = c.set;
        let slot = &c.slot;
        if c.members.len() != 5 + declared {
            push(
                false,
                format!(
                    "bay{set} ({slot}): holds {} members and the board declares {declared} firmware overlays: no host merge",
                    c.members.len().saturating_sub(5)
                ),
            );
            continue;
        }
        let base = work.join(format!("bay{set}-base.dtb"));
        if let Err(e) = std::fs::write(&base, &c.members[2].1) {
            push(false, format!("bay{set} ({slot}): {}: {e}", base.display()));
            continue;
        }
        let mut current = base;
        let mut merged = true;
        for (j, name) in boot.firmware_overlays.iter().enumerate() {
            let overlay = work.join(format!("bay{set}-{name}.dtbo"));
            let next = work.join(format!("bay{set}-{j}.dtb"));
            let params = params_of(boot, name);
            let written = std::fs::write(&overlay, &c.members[5 + j].1);
            if let Err(e) = written {
                push(
                    false,
                    format!("bay{set} ({slot}): {}: {e}", overlay.display()),
                );
                merged = false;
                break;
            }
            let result = Command::new(tool)
                .arg(&current)
                .arg(&next)
                .arg(&overlay)
                .args(&params)
                .output();
            match result {
                Err(e) => {
                    push(
                        false,
                        format!(
                            "bay{set} ({slot}): {name} does not apply on host, not loaded by firmware: cannot run {tool}: {e}"
                        ),
                    );
                    merged = false;
                    break;
                }
                Ok(o) if !o.status.success() => {
                    let status = o
                        .status
                        .code()
                        .map_or("a signal".to_string(), |c| c.to_string());
                    push(
                        false,
                        format!(
                            "bay{set} ({slot}): {name} does not apply on host, not loaded by firmware: dtmerge exited with {status}: {}",
                            first_line(&o.stderr)
                        ),
                    );
                    merged = false;
                    break;
                }
                Ok(_) => {
                    push(
                        true,
                        format!(
                            "bay{set} ({slot}): {name} applies on host, not loaded by firmware ({} parameter(s))",
                            params.len()
                        ),
                    );
                    current = next;
                }
            }
        }
        if !merged {
            continue;
        }
        let tree = match std::fs::read(&current) {
            Ok(t) => t,
            Err(e) => {
                push(
                    false,
                    format!("bay{set} ({slot}): {}: {e}", current.display()),
                );
                continue;
            }
        };
        for node in &boot.expect_nodes {
            for (prop, value) in &node.properties {
                let want = match expected_bytes(boot, value) {
                    Ok(w) => w,
                    Err(e) => {
                        push(
                            false,
                            format!("bay{set} ({slot}): {} {prop}: {e}", node.path),
                        );
                        continue;
                    }
                };
                let at = format!("bay{set} ({slot}): {} {prop}", node.path);
                match find(&tree, &node.path, prop) {
                    Ok(Found::NoNode) => push(
                        false,
                        format!(
                            "{at}: no such node after the host merge (applies on host, not loaded by firmware)"
                        ),
                    ),
                    Ok(Found::NoProperty) => push(
                        false,
                        format!(
                            "{at}: the property is missing after the host merge (applies on host, not loaded by firmware)"
                        ),
                    ),
                    Ok(Found::Value(got)) if got == want => push(
                        true,
                        format!(
                            "{at} is {} after the host merge (applies on host, not loaded by firmware)",
                            describe(&got)
                        ),
                    ),
                    Ok(Found::Value(got)) => push(
                        false,
                        format!(
                            "{at} is {}, expected {}, after the host merge (applies on host, not loaded by firmware)",
                            describe(&got),
                            describe(&want)
                        ),
                    ),
                    Err(e) => push(false, format!("{at}: {e}")),
                }
            }
        }
    }
    out
}
