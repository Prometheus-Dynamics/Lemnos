//! The first-flash manifest (`manifest` on `system.flash/1`), the signed message, the signature
//! record and the keys the board trusts.
//!
//! The transport JSON (schema 1) is transport only; nothing in it is signed:
//!
//! ```text
//! {"schema":1,"board":"<board id>","format":"disk","image":{"size":N,"sha256":"<64 hex>"},
//!  "signature":"<216 hex: the record, optional>"}
//! ```
//!
//! `format` is the raw image's kind as the board names it (`disk` for a block target), not the
//! compression. `image.size` and `image.sha256` are the raw image's (stream 1's decoded bytes).
//!
//! The signed message is binary and built here from the board's own id and format, the size and
//! the SHA-256 (see [`signed_message`]). The signature is Ed25519 over those bytes. Its record is
//! 108 bytes: `LMNSSIG1`, algorithm 1, three zero bytes, the key id (SHA-256 of the public key),
//! then the 64-byte signature. The record also sits in the image file, as its second xz stream.

use ed25519_dalek::{Signature, VerifyingKey};
use lemnos_core::Status;
use lemnos_slot_store::json::Json;
use sha2::{Digest, Sha256};

/// The domain tag of the signed message, with its NUL.
pub const DOMAIN: &[u8] = b"lemnos-first-flash\0";
/// The signed message's version.
pub const MESSAGE_VERSION: u32 = 1;
/// The record's magic.
pub const RECORD_MAGIC: &[u8; 8] = b"LMNSSIG1";
/// The record's algorithm: Ed25519.
pub const ALGORITHM_ED25519: u8 = 1;
/// The record's length.
pub const RECORD_LEN: usize = 108;
/// The raw image kind a block target takes.
pub const DISK: &str = "disk";

/// A manifest the driver accepted (its shape; the signature is decided separately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The board it is for: the board file's `[board] id`.
    pub board: String,
    /// The raw image's kind.
    pub format: String,
    /// The raw image's size.
    pub image_size: u64,
    /// The raw image's SHA-256.
    pub sha256: [u8; 32],
}

/// The signed message: the fixed binary record over the board's own facts and the image's.
///
/// ```text
/// "lemnos-first-flash\0" | u32 LE version | u32 LE len + board | u32 LE len + format
/// | u64 LE size | 32-byte SHA-256
/// ```
pub fn signed_message(board: &str, format: &str, size: u64, sha256: &[u8; 32]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(DOMAIN.len() + 4 + 4 + board.len() + 4 + format.len() + 8 + 32);
    out.extend_from_slice(DOMAIN);
    out.extend_from_slice(&MESSAGE_VERSION.to_le_bytes());
    out.extend_from_slice(&(board.len() as u32).to_le_bytes());
    out.extend_from_slice(board.as_bytes());
    out.extend_from_slice(&(format.len() as u32).to_le_bytes());
    out.extend_from_slice(format.as_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(sha256);
    out
}

/// A well-formed signature record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub key_id: [u8; 32],
    pub signature: [u8; 64],
}

/// The record's bytes, as they are written: `LMNSSIG1`, algorithm, zeros, key id, signature.
pub fn record_bytes(key_id: &[u8; 32], signature: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(RECORD_LEN);
    out.extend_from_slice(RECORD_MAGIC);
    out.push(ALGORITHM_ED25519);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(key_id);
    out.extend_from_slice(signature);
    out
}

/// Reads a record: exactly 108 bytes, the magic, algorithm 1 and zero padding. Anything else is a
/// malformed record, whatever the policy.
///
/// # Errors
///
/// When the bytes are not a well-formed record.
pub fn parse_record(bytes: &[u8]) -> Result<Record, &'static str> {
    if bytes.len() != RECORD_LEN
        || bytes[..8] != *RECORD_MAGIC
        || bytes[8] != ALGORITHM_ED25519
        || bytes[9..12] != [0, 0, 0]
    {
        return Err("malformed signature record");
    }
    let mut key_id = [0u8; 32];
    key_id.copy_from_slice(&bytes[12..44]);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&bytes[44..108]);
    Ok(Record { key_id, signature })
}

/// The SHA-256 of a public key: its key id.
pub fn key_id(key: &VerifyingKey) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

/// A key the board trusts, its key id, and whose it is.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key: VerifyingKey,
    pub id: [u8; 32],
    /// Who holds the private half, as the key file names them.
    pub owner: String,
}

/// The keys the board trusts: one per line, 64 hex digits, a space, then the owner. Blank lines and
/// `#` comments are skipped; a line without an owner is refused.
///
/// # Errors
///
/// When a line is not a key of that form.
pub fn parse_keys(text: &str) -> Result<Vec<TrustedKey>, String> {
    let mut keys = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, owner) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(format!("line {} of the key file names no owner", at + 1));
        }
        let bytes = hex32(hex)
            .ok_or_else(|| format!("line {} of the key file is not 64 hex digits", at + 1))?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| format!("line {} of the key file is not a key", at + 1))?;
        keys.push(TrustedKey {
            id: key_id(&key),
            key,
            owner: owner.to_string(),
        });
    }
    Ok(keys)
}

/// How a manifest's signature stands, once the policy has been applied to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The record's key is on the board's list and the signature verifies, by that key's owner.
    Verified(String),
    /// No record: the image is unsigned, and the board accepts that.
    NotCheckedUnsigned,
    /// A well-formed record under a key the board doesn't know (or no key list at all).
    NotCheckedUnknownKey,
}

impl Outcome {
    /// The stable word for `info`, the log and the sender's UI.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Verified(_) => "verified",
            Self::NotCheckedUnsigned => "not_checked_unsigned",
            Self::NotCheckedUnknownKey => "not_checked_unknown_key",
        }
    }

    /// The code the write job's result carries after its byte count.
    pub fn code(&self) -> u8 {
        match self {
            Self::Verified(_) => 0,
            Self::NotCheckedUnsigned => 1,
            Self::NotCheckedUnknownKey => 2,
        }
    }

    /// The reason in words, as the sender shows it.
    pub fn reason(&self) -> String {
        match self {
            Self::Verified(owner) => {
                format!("signature verified with a key on this board's list ({owner})")
            }
            Self::NotCheckedUnsigned => "unsigned image; signature not checked".into(),
            Self::NotCheckedUnknownKey => "signed by an unknown key, not checked".into(),
        }
    }

    /// The signer's owner, or empty.
    pub fn signer(&self) -> &str {
        match self {
            Self::Verified(owner) => owner,
            _ => "",
        }
    }
}

/// The signature policy, as the table in the brief sets it. `record` is empty or the record's
/// bytes; the size and hash are the manifest's, and the message is rebuilt from `board` and
/// `format` (the board's own).
///
/// # Errors
///
/// `(status, sentence)` for every refused row: a malformed record in any row; a known key whose
/// signature does not verify in any row; an unsigned or unknown-key image under `require_signed`.
pub fn decide(
    require_signed: bool,
    board: &str,
    format: &str,
    size: u64,
    sha256: &[u8; 32],
    record: &[u8],
    keys: &[TrustedKey],
) -> Result<Outcome, (Status, String)> {
    if record.is_empty() {
        if require_signed {
            return Err((
                Status::NOT_PERMITTED,
                "this board takes only signed images, and this one is unsigned".into(),
            ));
        }
        return Ok(Outcome::NotCheckedUnsigned);
    }
    let parsed = parse_record(record).map_err(|why| (Status::INVALID_ARGUMENT, why.to_string()))?;
    let Some(trusted) = keys.iter().find(|k| k.id == parsed.key_id) else {
        if require_signed {
            return Err((
                Status::NOT_PERMITTED,
                "this board takes only images signed by a key on its list, and this one is signed by an unknown key".into(),
            ));
        }
        return Ok(Outcome::NotCheckedUnknownKey);
    };
    let message = signed_message(board, format, size, sha256);
    let signature = Signature::from_bytes(&parsed.signature);
    if trusted.key.verify_strict(&message, &signature).is_err() {
        return Err((
            Status::NOT_PERMITTED,
            format!(
                "the signature does not verify for the key of {}: the record is corrupted or was not made by that key",
                trusted.owner
            ),
        ));
    }
    Ok(Outcome::Verified(trusted.owner.clone()))
}

/// Reads a schema 1 transport manifest for `board_id`. A manifest for another board is refused
/// first, by name. Returns the manifest and the JSON's `signature` field, decoded, when present.
///
/// # Errors
///
/// `NOT_SUPPORTED` for another board; `INVALID_ARGUMENT` for anything else out of shape.
pub fn parse(text: &str, board_id: &str) -> Result<Parsed, (Status, String)> {
    let bad = |why: &str| (Status::INVALID_ARGUMENT, why.to_string());
    let json = Json::parse(text).ok_or_else(|| bad("the manifest is not JSON"))?;
    if json.get("schema").and_then(Json::u64) != Some(1) {
        return Err(bad("the manifest is not schema 1"));
    }
    let board = json
        .get("board")
        .and_then(Json::text)
        .ok_or_else(|| bad("the manifest has no board"))?;
    if board != board_id {
        return Err((
            Status::NOT_SUPPORTED,
            format!("this image is for board {board}, and this is board {board_id}"),
        ));
    }
    if !valid_board(board) {
        return Err(bad("the manifest's board is not a board id"));
    }
    let format = json
        .get("format")
        .and_then(Json::text)
        .ok_or_else(|| bad("the manifest has no format"))?;
    if format != DISK {
        return Err(bad(
            "the manifest's format is not disk: this board takes a whole disk image",
        ));
    }
    let image_size = json
        .path("image.size")
        .and_then(Json::u64)
        .ok_or_else(|| bad("the manifest's image.size is missing or not a number"))?;
    if image_size == 0 {
        return Err(bad("the manifest's image.size is zero"));
    }
    let sha = json
        .path("image.sha256")
        .and_then(Json::text)
        .ok_or_else(|| bad("the manifest's image.sha256 is missing"))?;
    let sha256 = hex32(sha)
        .ok_or_else(|| bad("the manifest's image.sha256 is not 64 lowercase hex digits"))?;
    let signature = match json.get("signature").and_then(Json::text) {
        None => None,
        Some(hex) => Some(
            hex_bytes(hex, RECORD_LEN * 2)
                .ok_or_else(|| bad("the manifest's signature is not 216 hex digits"))?,
        ),
    };
    Ok((
        Manifest {
            board: board.to_string(),
            format: format.to_string(),
            image_size,
            sha256,
        },
        signature,
    ))
}

/// A transport manifest as [`parse`] reads it: the manifest, and its `signature` field decoded.
pub type Parsed = (Manifest, Option<Vec<u8>>);

/// A board id: lowercase `[a-z0-9-]`, at most 64.
pub fn valid_board(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn hex32(text: &str) -> Option<[u8; 32]> {
    hex_bytes(text, 64)?.try_into().ok()
}

/// `len` lowercase hex digits, as bytes.
fn hex_bytes(text: &str, len: usize) -> Option<Vec<u8>> {
    if text.len() != len
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const SHA: &str = "edca4f812cbd2a855990162d14394578f5609ea1af267ede04670a084b1e6ab0";

    /// The brief's test vector: the test seed, the board `flash-test`, format `disk`, size 16384.
    const VECTOR_SHA: &str = "f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58";
    const VECTOR_MESSAGE: &str = "6c656d6e6f732d66697273742d666c61736800010000000a000000666c6173682d74657374040000006469736b0040000000000000f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58";
    const VECTOR_PUBLIC: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
    const VECTOR_SIGNATURE: &str = "bd894b37df1a05915c0c6baa7606d57c8cc2a6aa10a01451a3e4f67b8edfd517df6794fbcdeeaadfd7f46e099cea00be896e99445a8942f5e7592ec31c1b6c09";
    const VECTOR_KEY_ID: &str = "fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889";
    const VECTOR_RECORD: &str = "4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889bd894b37df1a05915c0c6baa7606d57c8cc2a6aa10a01451a3e4f67b8edfd517df6794fbcdeeaadfd7f46e099cea00be896e99445a8942f5e7592ec31c1b6c09";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn vector_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn vector_sha() -> [u8; 32] {
        hex32(VECTOR_SHA).unwrap()
    }

    fn trusted() -> Vec<TrustedKey> {
        parse_keys(&format!("# the build\n{VECTOR_PUBLIC} the build key\n")).unwrap()
    }

    fn json(board: &str, format: &str, size: u64, sha: &str) -> String {
        format!(
            r#"{{"schema":1,"board":"{board}","format":"{format}","image":{{"size":{size},"sha256":"{sha}"}}}}"#
        )
    }

    fn vector_record() -> Vec<u8> {
        hex_bytes(VECTOR_RECORD, RECORD_LEN * 2).unwrap()
    }

    #[test]
    fn the_signed_message_is_rebuilt_to_the_vector() {
        let message = signed_message("flash-test", DISK, 16384, &vector_sha());
        assert_eq!(hex(&message), VECTOR_MESSAGE);
        assert_eq!(message.len(), 85);
    }

    #[test]
    fn the_vector_signature_verifies_over_the_rebuilt_message() {
        let key = vector_key();
        assert_eq!(hex(key.verifying_key().as_bytes()), VECTOR_PUBLIC);
        let message = signed_message("flash-test", DISK, 16384, &vector_sha());
        let signature = key.sign(&message);
        assert_eq!(hex(&signature.to_bytes()), VECTOR_SIGNATURE);
        let record = parse_record(&vector_record()).unwrap();
        assert_eq!(hex(&record.key_id), VECTOR_KEY_ID);
        assert_eq!(
            record_bytes(&record.key_id, &record.signature),
            vector_record()
        );
        let outcome = decide(
            true,
            "flash-test",
            DISK,
            16384,
            &vector_sha(),
            &vector_record(),
            &trusted(),
        );
        assert_eq!(outcome, Ok(Outcome::Verified("the build key".into())));
    }

    #[test]
    fn a_transport_manifest_reads_and_names_another_board_first() {
        let (m, sig) = parse(&json("flash-test", "disk", 16384, VECTOR_SHA), "flash-test").unwrap();
        assert_eq!((m.format.as_str(), m.image_size), ("disk", 16384));
        assert!(sig.is_none());
        let other = parse(&json("other", "disk", 1, SHA), "flash-test").unwrap_err();
        assert_eq!(other.0, Status::NOT_SUPPORTED);
        assert!(other.1.contains("this image is for board other"));
        // Another board's manifest is refused by name even when its shape is also wrong.
        assert_eq!(
            parse(&json("other", "xz", 0, "x"), "flash-test")
                .unwrap_err()
                .0,
            Status::NOT_SUPPORTED
        );
    }

    #[test]
    fn a_manifest_out_of_shape_is_refused() {
        let b = "flash-test";
        assert!(
            parse(&json(b, "xz", 1, SHA), b).is_err(),
            "format is the image's kind"
        );
        assert!(parse(&json(b, "disk", 0, SHA), b).is_err());
        assert!(parse(&json(b, "disk", 1, "EDCA"), b).is_err());
        assert!(
            parse(
                &json(b, "disk", 1, SHA).replace("\"schema\":1", "\"schema\":2"),
                b
            )
            .is_err()
        );
        let with_sig =
            json(b, "disk", 1, SHA).replace('}', &format!(r#","signature":"{}"}}"#, VECTOR_RECORD));
        assert!(parse(&with_sig, b).unwrap().1.is_some());
    }

    #[test]
    fn a_malformed_record_is_refused_whatever_the_policy() {
        let good = vector_record();
        let mut magic = good.clone();
        magic[0] = b'X';
        let mut alg = good.clone();
        alg[8] = 2;
        let mut zeros = good.clone();
        zeros[10] = 1;
        for bytes in [&good[..107], &magic[..], &alg[..], &zeros[..]] {
            let got = decide(
                false,
                "flash-test",
                DISK,
                16384,
                &vector_sha(),
                bytes,
                &trusted(),
            );
            assert_eq!(got.unwrap_err().0, Status::INVALID_ARGUMENT);
        }
        let got = decide(
            false,
            "flash-test",
            DISK,
            16384,
            &vector_sha(),
            &good[..107],
            &[],
        );
        assert!(got.is_err(), "a short record is malformed with no keys too");
    }

    /// The eight rows of the policy table in the brief, each by its own row.
    #[test]
    fn the_policy_table_has_eight_rows() {
        let sha = vector_sha();
        let good = vector_record();
        let mut bad_sig = good.clone();
        bad_sig[50] ^= 1;
        let unknown = {
            let other = SigningKey::from_bytes(&[9; 32]);
            let sig = other
                .sign(&signed_message("flash-test", DISK, 16384, &sha))
                .to_bytes();
            record_bytes(&key_id(&other.verifying_key()), &sig)
        };
        let keys = trusted();
        let none: Vec<TrustedKey> = Vec::new();
        let run = |require: bool, record: &[u8], keys: &[TrustedKey]| {
            decide(require, "flash-test", DISK, 16384, &sha, record, keys).map(|o| o.word())
        };
        // Off: unsigned accepted, known verifies accepted, known bad refused, unknown accepted.
        assert_eq!(run(false, &[], &keys), Ok("not_checked_unsigned"));
        assert_eq!(run(false, &good, &keys), Ok("verified"));
        assert_eq!(
            run(false, &bad_sig, &keys).unwrap_err().0,
            Status::NOT_PERMITTED
        );
        assert_eq!(run(false, &unknown, &keys), Ok("not_checked_unknown_key"));
        assert_eq!(
            run(false, &good, &none),
            Ok("not_checked_unknown_key"),
            "no key list is an unknown key"
        );
        // On: unsigned refused, unknown refused (with or without a list), known bad refused, verifies accepted.
        assert_eq!(run(true, &[], &keys).unwrap_err().0, Status::NOT_PERMITTED);
        assert_eq!(
            run(true, &unknown, &keys).unwrap_err().0,
            Status::NOT_PERMITTED
        );
        assert_eq!(
            run(true, &good, &none).unwrap_err().0,
            Status::NOT_PERMITTED
        );
        assert_eq!(
            run(true, &bad_sig, &keys).unwrap_err().0,
            Status::NOT_PERMITTED
        );
        assert_eq!(run(true, &good, &keys), Ok("verified"));
    }

    #[test]
    fn a_signature_under_the_wrong_message_does_not_verify() {
        // The same signature, offered for a different size: the message differs, so it fails.
        let r = decide(
            false,
            "flash-test",
            DISK,
            16385,
            &vector_sha(),
            &vector_record(),
            &trusted(),
        );
        assert_eq!(r.unwrap_err().0, Status::NOT_PERMITTED);
        // The same signature for another board id: refused too.
        let r = decide(
            false,
            "other",
            DISK,
            16384,
            &vector_sha(),
            &vector_record(),
            &trusted(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn keys_are_read_one_per_line_with_an_owner() {
        assert!(
            parse_keys(&format!("{VECTOR_PUBLIC}\n")).is_err(),
            "a key with no owner"
        );
        assert!(parse_keys("zz owner\n").is_err());
        assert_eq!(
            parse_keys(&format!("{VECTOR_PUBLIC} owner\n")).unwrap()[0]
                .id
                .len(),
            32
        );
    }
}
