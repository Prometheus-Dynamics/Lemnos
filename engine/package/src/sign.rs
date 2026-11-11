//! Signed mode: whose packages are trusted, and whether a file's bytes carry one of their
//! signatures.

use std::path::Path;

use ed25519_dalek::{Signature, VerifyingKey};

/// The length of a package's detached signature, in bytes.
pub const SIGNATURE_LEN: usize = 64;
/// The length of a public key, in bytes.
pub const KEY_LEN: usize = 32;

/// Why a set of trusted keys could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The key file could not be read.
    Unreadable(String),
    /// The line (from 1) of a key file that is not 64 hex digits.
    NotHex(usize),
    /// The key (from 1) that is not a valid Ed25519 public key.
    NotAKey(usize),
}

impl core::fmt::Display for KeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "the key file can't be read: {why}"),
            Self::NotHex(line) => write!(f, "line {line} of the key file is not 64 hex digits"),
            Self::NotAKey(key) => write!(f, "key {key} is not an Ed25519 public key"),
        }
    }
}

impl std::error::Error for KeyError {}

/// The public keys whose signatures make a package loadable in signed mode. They come from the
/// service's configuration, never from a package. With none, signed mode loads nothing.
#[derive(Debug, Clone, Default)]
pub struct TrustedKeys(Vec<VerifyingKey>);

impl TrustedKeys {
    /// Keys given as bytes: 32 each.
    pub fn from_bytes(keys: &[[u8; KEY_LEN]]) -> Result<Self, KeyError> {
        keys.iter()
            .enumerate()
            .map(|(at, key)| VerifyingKey::from_bytes(key).map_err(|_| KeyError::NotAKey(at + 1)))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// Keys from the text of a key file: one key per line as 64 hex digits. Blank lines and
    /// lines starting with `#` are skipped.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let mut keys = Vec::new();
        for (at, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            keys.push(hex(line).ok_or(KeyError::NotHex(at + 1))?);
        }
        Self::from_bytes(&keys)
    }

    /// Keys from a key file. See [`TrustedKeys::parse`].
    pub fn from_file(path: &Path) -> Result<Self, KeyError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| KeyError::Unreadable(error.to_string()))?;
        Self::parse(&text)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `signature` is one of these keys' signature over exactly `bytes`.
    pub(crate) fn signed(&self, bytes: &[u8], signature: &[u8]) -> bool {
        let Ok(signature) = <[u8; SIGNATURE_LEN]>::try_from(signature) else {
            return false;
        };
        let signature = Signature::from_bytes(&signature);
        self.0
            .iter()
            .any(|key| key.verify_strict(bytes, &signature).is_ok())
    }
}

fn hex(text: &str) -> Option<[u8; KEY_LEN]> {
    let digits = text.as_bytes();
    if digits.len() != KEY_LEN * 2 {
        return None;
    }
    let mut out = [0; KEY_LEN];
    for (byte, pair) in out.iter_mut().zip(digits.as_chunks::<2>().0) {
        let high = char::from(pair[0]).to_digit(16)?;
        let low = char::from(pair[1]).to_digit(16)?;
        *byte = (high * 16 + low) as u8;
    }
    Some(out)
}
