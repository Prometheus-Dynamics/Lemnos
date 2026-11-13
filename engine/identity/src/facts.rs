//! The identity facts (`docs/identity.md`): what the board file and the slot files say, each
//! checked against its format before it is kept.
//!
//! A fact that cannot be known is absent. [`gather`] never invents one, and says in
//! [`Gathered::notes`] which fact it could not read and why.

use std::fs;
use std::path::Path;

use crate::board::BoardFile;
use crate::json::string_field;
use crate::serial::normalise;

/// The most facts one node may publish.
pub const MAX_FACTS: usize = 64;
/// The most bytes one fact's value may be.
pub const MAX_VALUE_BYTES: usize = 256;
/// The most bytes a `family`, `model` or `version.product` value may be.
const NAME_BYTES: usize = 64;

/// The facts, in the order they were set. Each name is one of the stack's agreed names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pairs: Vec<(&'static str, String)>,
}

/// The facts gathered from the board, and why each missing one is missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gathered {
    pub facts: Facts,
    pub notes: Vec<String>,
}

/// The names Lemnos owns, in the order they are published.
pub const NAMES: [&str; 8] = [
    "family",
    "serial",
    "model",
    "version.slot",
    "version.lemnos",
    "version.product",
    "version.os",
    "offers",
];

impl Facts {
    /// Sets `name` to `value`, replacing an earlier value of the same name.
    ///
    /// # Errors
    ///
    /// When `name` is not one of [`NAMES`], or the value does not fit that fact's format.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let key = NAMES
            .iter()
            .copied()
            .find(|n| *n == name)
            .ok_or_else(|| format!("{name} is not an identity fact"))?;
        check(key, value)?;
        let value = value.to_string();
        match self.pairs.iter_mut().find(|(n, _)| *n == key) {
            Some(pair) => pair.1 = value,
            None => self.pairs.push((key, value)),
        }
        Ok(())
    }

    /// The value of `name`, when it is known.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every known fact, in the order they were set.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs.iter().map(|(n, v)| (*n, v.as_str()))
    }
}

/// Checks `value` against the format of `name`.
fn check(name: &str, value: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("{name}: {why}"));
    if value.len() > MAX_VALUE_BYTES {
        return bad(&format!("{} bytes; at most {MAX_VALUE_BYTES}", value.len()));
    }
    if value.is_empty() {
        return bad("empty");
    }
    if value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return bad("contains a control character");
    }
    match name {
        "family" | "version.product" => {
            if value.len() > NAME_BYTES {
                return bad(&format!("{} bytes; at most {NAME_BYTES}", value.len()));
            }
            if !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return bad("not lowercase [a-z0-9-]");
            }
        }
        "model" => {
            if value.len() > NAME_BYTES {
                return bad(&format!("{} bytes; at most {NAME_BYTES}", value.len()));
            }
        }
        "serial" => {
            if normalise(value).as_deref() != Ok(value) {
                return bad("not a normalised serial");
            }
        }
        "version.slot" => {
            if value.bytes().any(|b| b.is_ascii_whitespace() || b == b'/') {
                return bad("a slot id has no spaces or slashes");
            }
        }
        "offers" if value.split(' ').any(str::is_empty) => {
            return bad("contract ids separated by single spaces");
        }
        _ => {}
    }
    Ok(())
}

/// Gathers the facts the board file and, when `data` is given, the slot files on the data
/// partition say. `version.lemnos` and `offers` are the daemon's: the caller sets them.
#[must_use]
pub fn gather(board: &BoardFile, data: Option<&Path>) -> Gathered {
    let mut out = Gathered::default();
    let set = |out: &mut Gathered, name: &str, value: Option<String>, why: &str| match value {
        Some(value) => {
            if let Err(e) = out.facts.set(name, &value) {
                out.notes.push(e);
            }
        }
        None => out.notes.push(format!("{name}: {why}")),
    };

    set(
        &mut out,
        "family",
        Some(board.board.id.clone()),
        "no [board] id",
    );

    let serial = board
        .board
        .serial
        .as_ref()
        .ok_or_else(|| "no [board.serial] source".to_string())
        .and_then(|source| source.read().map_err(|e| e.to_string()));
    match serial {
        Ok(serial) => set(&mut out, "serial", Some(serial), ""),
        Err(why) => out.notes.push(format!("serial: {why}")),
    }

    let model = board
        .board
        .name
        .as_ref()
        .map(|name| match &board.board.revision {
            Some(revision) => format!("{name} {revision}"),
            None => name.clone(),
        });
    set(&mut out, "model", model, "no [board] name");

    if let Some(data) = data {
        slot_facts(data, &mut out);
    }
    out
}

/// The running slot's facts: its id from `board/slots/booted`, and its product and version from
/// its manifest, `slots/<id>.json`.
fn slot_facts(data: &Path, out: &mut Gathered) {
    let booted = data.join("board/slots/booted");
    let Ok(text) = fs::read_to_string(&booted) else {
        out.notes
            .push(format!("version.slot: cannot read {}", booted.display()));
        return;
    };
    let Some(slot) = text.lines().next().map(str::trim).filter(|s| !s.is_empty()) else {
        out.notes
            .push(format!("version.slot: {} is empty", booted.display()));
        return;
    };
    if let Err(e) = out.facts.set("version.slot", slot) {
        out.notes.push(e);
        return;
    }
    let manifest = data.join("slots").join(format!("{slot}.json"));
    let Ok(json) = fs::read_to_string(&manifest) else {
        out.notes.push(format!(
            "version.product, version.os: cannot read {}",
            manifest.display()
        ));
        return;
    };
    for (name, key) in [("version.product", "product"), ("version.os", "version")] {
        match string_field(&json, key) {
            Some(value) => {
                if let Err(e) = out.facts.set(name, &value) {
                    out.notes.push(e);
                }
            }
            None => out.notes.push(format!(
                "{name}: {} has no string {key}",
                manifest.display()
            )),
        }
    }
}
