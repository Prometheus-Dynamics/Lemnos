//! The identity facts (`docs/identity.md`), each checked against its format before it is kept.
//! A fact that cannot be known is never set: it is absent.

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
