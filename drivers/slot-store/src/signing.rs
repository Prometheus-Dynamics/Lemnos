//! The signature policy for a signed image, shared by every driver that takes one: slot bundles
//! (`system-slots`) and first-flash images (`system-flash`). A driver checks the signature itself
//! (it holds the Ed25519 code) and reports what it found as a [`Check`]; [`decide`] applies
//! `require_signed` and names the [`Outcome`]. The table exists once, here.
//!
//! | `require_signed` | image | key | result | outcome |
//! |---|---|---|---|---|
//! | off | unsigned | – | accepted | `not_checked_unsigned` |
//! | off | signed | the board's key, verifies | accepted | `verified` |
//! | off | signed | the board's key, does not verify | refused by name | refused |
//! | off | signed | unknown key, or no key list | accepted | `not_checked_unknown_key` |
//! | on | unsigned | – | refused by name | refused |
//! | on | signed | unknown key or no key list | refused by name | refused |
//! | on | signed | known key, bad signature | refused by name | refused |
//! | on | signed | known key, verifies | accepted | `verified` |
//!
//! A malformed signature (wrong length, bad encoding, no named key) is refused in every row.
//! "Not checked" is only for a well-formed signature whose key the board does not hold.

use std::fmt;

use sha2::{Digest, Sha256};

/// The signature record every signed image carries (a slot bundle's `slot.sig`, a first-flash
/// image's second stream), 108 bytes:
///
/// ```text
/// offset  0  8 bytes  "LMNSSIG1"
/// offset  8  1 byte   algorithm: 1 = Ed25519
/// offset  9  3 bytes  zero
/// offset 12 32 bytes  key id: SHA-256 of the signing public key
/// offset 44 64 bytes  Ed25519 signature over the signed message
/// ```
///
/// The signed message is the signing format's own, built by [`signed_message`] from a [`Domain`]
/// (a slot's is [`slot_bundle_message`]; a first-flash image's is the message it defines). Only
/// the container is shared.
pub const RECORD_MAGIC: &[u8; 8] = b"LMNSSIG1";
/// The algorithm byte of an Ed25519 record.
pub const ALGORITHM_ED25519: u8 = 1;
/// The record's length.
pub const RECORD_LEN: usize = 108;

/// A well-formed record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub key_id: [u8; 32],
    pub signature: [u8; 64],
}

/// The record's bytes: magic, algorithm, zeros, key id, signature.
pub fn record_bytes(key_id: &[u8; 32], signature: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(RECORD_LEN);
    out.extend_from_slice(RECORD_MAGIC);
    out.push(ALGORITHM_ED25519);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(key_id);
    out.extend_from_slice(signature);
    out
}

/// Reads a record: exactly 108 bytes, the magic, algorithm 1 and zero padding.
///
/// # Errors
///
/// `"malformed signature record"` for anything else.
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

/// The kinds of signed message. Each kind's message starts with its own domain tag, so a
/// signature made for one kind never verifies as another kind under the same key.
///
/// To add a kind (for example a signed authorization):
///
/// 1. add a variant here, with a doc line naming its driver and its tag;
/// 2. give it a tag in [`Domain::tag`]: a unique `lemnos-<kind>\0` string, never reused
///    (a tag is part of every signature made under it, so it cannot change once shipped);
/// 3. build its message in its own driver with [`signed_message`] over the kind's body bytes,
///    never by hand and never with the bare body, so the tag cannot be forgotten;
/// 4. give the kind's body a version and length prefixes, as the existing bodies do, and add a
///    vector test (a fixed key, body and message in hex) next to the existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// A first-flash image's message (`system-flash`): tag `lemnos-first-flash\0`.
    FirstFlash,
    /// A slot bundle's message (`system-slots`, since I5g2): tag `lemnos-slot-bundle\0`.
    SlotBundle,
    /// The authorization that turns the board's security switch off (`system-security`): tag
    /// `lemnos-security-switch\0`. See [`security_switch_message`].
    SecuritySwitch,
}

impl Domain {
    /// The domain tag, with its NUL. Unique per variant; see the enum's docs before adding one.
    pub fn tag(self) -> &'static [u8] {
        match self {
            Self::FirstFlash => b"lemnos-first-flash\0",
            Self::SlotBundle => b"lemnos-slot-bundle\0",
            Self::SecuritySwitch => b"lemnos-security-switch\0",
        }
    }
}

/// The version of the slot bundle's signed message.
pub const SLOT_BUNDLE_VERSION: u32 = 1;

/// The bytes a signature is made over: the domain tag, then `body`, the kind's own fields. This
/// is the only builder of a signed message: a driver that adds a kind calls it with its own
/// [`Domain`] variant (see the enum's docs for the steps).
pub fn signed_message(domain: Domain, body: &[u8]) -> Vec<u8> {
    let tag = domain.tag();
    let mut out = Vec::with_capacity(tag.len() + body.len());
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    out
}

/// A slot bundle's signed message over its `slot.json` bytes, exactly as the bundle holds them:
///
/// ```text
/// "lemnos-slot-bundle\0"   19 bytes: the domain tag, with its NUL
/// u32 LE version = 1
/// u32 LE length, then the manifest's exact bytes
/// ```
pub fn slot_bundle_message(manifest: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + manifest.len());
    body.extend_from_slice(&SLOT_BUNDLE_VERSION.to_le_bytes());
    body.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    body.extend_from_slice(manifest);
    signed_message(Domain::SlotBundle, &body)
}

/// The version of the security switch's authorization message.
pub const SECURITY_SWITCH_VERSION: u32 = 1;

/// The message an authorization signs to turn the security switch to `on`, on the board whose
/// identity (its serial, as `system.identity/1` gives it) is `serial`, while the switch's record is
/// at `generation`. Turning it off needs this message signed under a key the board holds:
///
/// ```text
/// "lemnos-security-switch\0"   23 bytes: the domain tag, with its NUL
/// u32 LE version = 1
/// u32 LE length, then the board's serial bytes
/// u8     target: 0 = off, 1 = on
/// u64 LE generation of the switch's record
/// ```
///
/// Every field is bound: another board's serial, another target or another generation gives other
/// bytes, which do not verify. The generation moves on each change, so an authorization is spent by
/// the change it allowed.
pub fn security_switch_message(serial: &[u8], on: bool, generation: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + 4 + serial.len() + 1 + 8);
    body.extend_from_slice(&SECURITY_SWITCH_VERSION.to_le_bytes());
    body.extend_from_slice(&(serial.len() as u32).to_le_bytes());
    body.extend_from_slice(serial);
    body.push(u8::from(on));
    body.extend_from_slice(&generation.to_le_bytes());
    signed_message(Domain::SecuritySwitch, &body)
}

/// A key id: the SHA-256 of a public key's bytes. Records and the board's key lists name keys by it.
pub fn key_id(public_key: &[u8]) -> [u8; 32] {
    Sha256::digest(public_key).into()
}

/// What a driver found when it checked an image's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check<'a> {
    /// No signature at all.
    Unsigned,
    /// A signature that is not well formed. Refused in every row.
    Malformed(String),
    /// A well-formed signature naming a key the board does not hold, or the board holds no keys.
    UnknownKey,
    /// A signature under a key the board holds, which does not verify.
    BadSignature { owner: &'a str },
    /// A signature under a key the board holds, which verifies.
    Verified { owner: &'a str },
}

/// How a signature stands once the policy has been applied, for an accepted image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Signed by a key on the board's list, which verifies; the key's owner.
    Verified(String),
    /// No signature, and the board accepts that.
    NotCheckedUnsigned,
    /// Signed by a key the board does not hold (or it holds none), so not checked.
    NotCheckedUnknownKey,
}

/// The outcome words, as `info`, the log and the slot records show them.
pub const VERIFIED: &str = "verified";
pub const NOT_CHECKED_UNSIGNED: &str = "not_checked_unsigned";
pub const NOT_CHECKED_UNKNOWN_KEY: &str = "not_checked_unknown_key";

impl Outcome {
    /// The stable word: `verified`, `not_checked_unsigned` or `not_checked_unknown_key`.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Verified(_) => VERIFIED,
            Self::NotCheckedUnsigned => NOT_CHECKED_UNSIGNED,
            Self::NotCheckedUnknownKey => NOT_CHECKED_UNKNOWN_KEY,
        }
    }

    /// The code a job result carries after its byte count (first flash): 0, 1 or 2.
    pub fn code(&self) -> u8 {
        match self {
            Self::Verified(_) => 0,
            Self::NotCheckedUnsigned => 1,
            Self::NotCheckedUnknownKey => 2,
        }
    }

    /// The reason in words, as a sender shows it.
    pub fn reason(&self) -> String {
        match self {
            Self::Verified(owner) => {
                format!("signature verified with a key on this board's list ({owner})")
            }
            Self::NotCheckedUnsigned => "unsigned image; signature not checked".into(),
            Self::NotCheckedUnknownKey => "signed by an unknown key, not checked".into(),
        }
    }

    /// The signing key's owner when it verified, or `""`.
    pub fn signer(&self) -> &str {
        match self {
            Self::Verified(owner) => owner,
            _ => "",
        }
    }

    /// The word a slot record kept before the outcome words existed (`SIGNED`): `trusted`,
    /// `untrusted` or `unsigned`. Kept so existing slots and their consumers read the same labels.
    pub fn legacy_word(&self) -> &'static str {
        match self {
            Self::Verified(_) => "trusted",
            Self::NotCheckedUnknownKey => "untrusted",
            Self::NotCheckedUnsigned => "unsigned",
        }
    }
}

/// The `SIGNED` label a word stands for, as `list` shows it: `trusted`, `untrusted` or `unsigned`.
pub fn legacy_label(word: &str) -> &'static str {
    match word {
        VERIFIED => "trusted",
        NOT_CHECKED_UNKNOWN_KEY => "untrusted",
        _ => "unsigned",
    }
}

/// The marker a board keeps in its state directory while `require_signed` is on. The boot side
/// reads it (through [`security_on`]): with it there, a trial whose signature did not verify is
/// not started, and the default is started anyway (see `docs/slots.md`).
pub const REQUIRE_MARKER: &str = "require-signed";

/// The file the boot side writes when it started the default slot although the default's
/// signature did not verify: the slot's id. The driver reports it; a verified boot removes it.
pub const BOOT_UNVERIFIED: &str = "boot-unverified";

/// Whether the board requires signing: the one reader of the switch. Its source is the
/// `require_signed` setting the driver writes into [`REQUIRE_MARKER`] at start (see
/// `docs/slots.md`, "Where the switch comes from"); moving the source changes this function only.
pub fn security_on(layout: &crate::store::Layout) -> bool {
    layout.marker(REQUIRE_MARKER).exists()
}

/// Writes the switch's source for the boot side: the marker when on, its absence when off.
///
/// # Errors
///
/// When the marker cannot be written or removed.
pub fn set_security(layout: &crate::store::Layout, on: bool) -> std::io::Result<()> {
    let path = layout.marker(REQUIRE_MARKER);
    if on {
        std::fs::create_dir_all(layout.state_dir())?;
        crate::store::replace(&path, b"yes\n")
    } else {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            Ok(()) => crate::store::sync_dir(&layout.state_dir()),
            Err(_) => Ok(()),
        }
    }
}

/// The slot the boot side started unverified (its default), `""` when none.
pub fn boot_unverified(layout: &crate::store::Layout) -> String {
    crate::store::read_line(&layout.marker(BOOT_UNVERIFIED))
}

/// Why an image is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The signature is not well formed: an invalid-argument refusal.
    Malformed(String),
    /// The policy refuses it, by name: a permission refusal.
    Policy(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(text) | Self::Policy(text) => f.write_str(text),
        }
    }
}

/// Applies the policy to a check. `what` names the image in the refusal text ("the bundle",
/// "the image").
///
/// # Errors
///
/// [`Refusal::Malformed`] for a malformed signature in every row; [`Refusal::Policy`] for an
/// unsigned, unknown-key or bad-signature image when `require_signed` is on, and for a bad
/// signature under a known key whatever the policy.
pub fn decide(require_signed: bool, what: &str, check: Check<'_>) -> Result<Outcome, Refusal> {
    match check {
        Check::Malformed(why) => Err(Refusal::Malformed(why)),
        Check::Unsigned if require_signed => Err(Refusal::Policy(format!(
            "{what} is not signed, and this board takes only signed ones"
        ))),
        Check::Unsigned => Ok(Outcome::NotCheckedUnsigned),
        Check::UnknownKey if require_signed => Err(Refusal::Policy(format!(
            "{what} is signed by a key this board does not trust, and this board takes only signed ones by a trusted key"
        ))),
        Check::UnknownKey => Ok(Outcome::NotCheckedUnknownKey),
        Check::BadSignature { owner } => Err(Refusal::Policy(format!(
            "{what} does not verify with the key of {owner}: it is corrupted, or was not made by that key"
        ))),
        Check::Verified { owner } => Ok(Outcome::Verified(owner.to_string())),
    }
}

/// The outcome word a record shows: its `SIGNATURE` word (`signature`) when it has one, else the
/// older `SIGNED` label (`signed`): `trusted` is `verified`, `untrusted` is
/// `not_checked_unknown_key` (a record from before the policy cannot say more than that), and
/// anything else `not_checked_unsigned`.
pub fn recorded(signature: &str, signed: &str) -> &'static str {
    match signature {
        VERIFIED => VERIFIED,
        NOT_CHECKED_UNSIGNED => NOT_CHECKED_UNSIGNED,
        NOT_CHECKED_UNKNOWN_KEY => NOT_CHECKED_UNKNOWN_KEY,
        _ => match signed {
            "trusted" => VERIFIED,
            "untrusted" => NOT_CHECKED_UNKNOWN_KEY,
            _ => NOT_CHECKED_UNSIGNED,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The vector in `docs/slots.md`: the test seed `0x07` repeated, and this manifest.
    const VECTOR_MANIFEST: &[u8] = b"{\"id\":\"vector\"}";

    #[test]
    fn the_slot_bundle_message_is_tagged_and_length_prefixed() {
        let message = slot_bundle_message(VECTOR_MANIFEST);
        assert_eq!(
            hex(&message),
            "6c656d6e6f732d736c6f742d62756e646c6500010000000f0000007b226964223a22766563746f72227d"
        );
    }

    #[test]
    fn the_domain_tags_differ_so_no_kind_verifies_as_another() {
        assert_ne!(Domain::FirstFlash.tag(), Domain::SlotBundle.tag());
        assert!(slot_bundle_message(VECTOR_MANIFEST).starts_with(b"lemnos-slot-bundle\0"));
        assert_ne!(
            slot_bundle_message(VECTOR_MANIFEST),
            VECTOR_MANIFEST.to_vec(),
            "a bare manifest is not the signed message"
        );
    }

    #[test]
    fn the_security_switch_message_binds_serial_target_and_generation() {
        let message = security_switch_message(b"0123456789abcdef", false, 1);
        assert_eq!(
            hex(&message),
            "6c656d6e6f732d73656375726974792d73776974636800010000001000000030313233343536373839616263646566000100000000000000"
        );
        assert_ne!(
            message,
            security_switch_message(b"fedcba9876543210", false, 1),
            "board"
        );
        assert_ne!(
            message,
            security_switch_message(b"0123456789abcdef", true, 1),
            "target"
        );
        assert_ne!(
            message,
            security_switch_message(b"0123456789abcdef", false, 2),
            "generation"
        );
        assert!(message.starts_with(Domain::SecuritySwitch.tag()));
    }

    #[test]
    fn the_security_switch_domain_is_its_own() {
        assert_ne!(Domain::SecuritySwitch.tag(), Domain::SlotBundle.tag());
        assert_ne!(Domain::SecuritySwitch.tag(), Domain::FirstFlash.tag());
        assert_eq!(Domain::SecuritySwitch.tag(), b"lemnos-security-switch\0");
    }

    fn policy(require: bool, check: Check<'_>) -> Result<Outcome, Refusal> {
        decide(require, "the image", check)
    }

    #[test]
    fn off_unsigned_is_accepted_as_not_checked_unsigned() {
        let out = policy(false, Check::Unsigned).unwrap();
        assert_eq!(out.word(), "not_checked_unsigned");
    }

    #[test]
    fn off_known_key_that_verifies_is_accepted_as_verified() {
        let out = policy(false, Check::Verified { owner: "build" }).unwrap();
        assert_eq!(out, Outcome::Verified("build".into()));
        assert_eq!(out.word(), "verified");
        assert_eq!(out.signer(), "build");
    }

    #[test]
    fn off_known_key_with_a_bad_signature_is_refused_by_name() {
        let err = policy(false, Check::BadSignature { owner: "build" }).unwrap_err();
        assert!(
            matches!(err, Refusal::Policy(ref t) if t.contains("build")),
            "{err}"
        );
    }

    #[test]
    fn off_unknown_key_or_no_key_list_is_accepted_as_not_checked_unknown_key() {
        let out = policy(false, Check::UnknownKey).unwrap();
        assert_eq!(out.word(), "not_checked_unknown_key");
        assert_eq!(out.legacy_word(), "untrusted");
    }

    #[test]
    fn on_unsigned_is_refused_by_name() {
        let err = policy(true, Check::Unsigned).unwrap_err();
        assert!(
            matches!(err, Refusal::Policy(ref t) if t.contains("not signed")),
            "{err}"
        );
    }

    #[test]
    fn on_unknown_key_is_refused_by_name() {
        let err = policy(true, Check::UnknownKey).unwrap_err();
        assert!(
            matches!(err, Refusal::Policy(ref t) if t.contains("does not trust")),
            "{err}"
        );
    }

    #[test]
    fn on_known_key_with_a_bad_signature_is_refused_by_name() {
        let err = policy(true, Check::BadSignature { owner: "build" }).unwrap_err();
        assert!(
            matches!(err, Refusal::Policy(ref t) if t.contains("build")),
            "{err}"
        );
    }

    #[test]
    fn on_known_key_that_verifies_is_accepted_as_verified() {
        let out = policy(true, Check::Verified { owner: "build" }).unwrap();
        assert_eq!(out.word(), "verified");
    }

    #[test]
    fn a_malformed_signature_is_refused_in_both_rows_as_malformed() {
        for require in [false, true] {
            let err = policy(
                require,
                Check::Malformed("slot.key is not 64 hex digits".into()),
            )
            .unwrap_err();
            assert_eq!(
                err,
                Refusal::Malformed("slot.key is not 64 hex digits".into())
            );
        }
    }

    #[test]
    fn a_record_from_before_the_outcome_words_reads_as_its_legacy_label() {
        assert_eq!(recorded("", "trusted"), "verified");
        assert_eq!(recorded("", "untrusted"), "not_checked_unknown_key");
        assert_eq!(recorded("", "unsigned"), "not_checked_unsigned");
        assert_eq!(recorded("", ""), "not_checked_unsigned");
        assert_eq!(recorded("verified", "untrusted"), "verified");
    }
}
