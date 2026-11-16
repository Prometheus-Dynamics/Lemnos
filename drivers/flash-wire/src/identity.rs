//! The identity record: what a stage says it speaks, and how a PC side checks it.
//!
//! A stage answers an identify request with this record:
//!
//! ```text
//! magic     2 bytes   "LI"
//! length    u16 LE    the whole record, head and entries and any trailing bytes
//! count     u8        how many entries follow
//! entries   count x   family (u8), then the family's hash (u64 LE)
//! trailing  ...       bytes a reader does not know: skipped (the length covers them)
//! ```
//!
//! [`IDENTITY`] is this build's record. [`check`] reads a peer's record and compares it with this
//! build family by family. A family this build knows that the peer does not list, or lists with another
//! hash, is a typed refusal ([`Skew`]) that names the family. A family the peer lists that this build
//! does not know is ignored, so a peer built with an added family still connects. Trailing bytes are
//! skipped, so a field can be appended to the record without moving any hash.

use crate::layout::{FAMILIES, FAMILY_COUNT, FAMILY_HASHES};

/// The magic at the start of an identity record.
pub const IDENTITY_MAGIC: [u8; 2] = *b"LI";
/// The bytes before the entries: magic, length, count.
pub const HEAD_LEN: usize = 5;
/// The bytes of one entry: family, hash.
pub const ENTRY_LEN: usize = 9;
/// The length of this build's record.
pub const IDENTITY_LEN: usize = HEAD_LEN + FAMILY_COUNT * ENTRY_LEN;

const fn identity_record() -> [u8; IDENTITY_LEN] {
    let mut out = [0u8; IDENTITY_LEN];
    out[0] = IDENTITY_MAGIC[0];
    out[1] = IDENTITY_MAGIC[1];
    let len = (IDENTITY_LEN as u16).to_le_bytes();
    out[2] = len[0];
    out[3] = len[1];
    out[4] = FAMILY_COUNT as u8;
    let mut i = 0;
    while i < FAMILY_COUNT {
        let at = HEAD_LEN + i * ENTRY_LEN;
        out[at] = FAMILIES[i];
        let hash = FAMILY_HASHES[i].to_le_bytes();
        let mut k = 0;
        while k < 8 {
            out[at + 1 + k] = hash[k];
            k += 1;
        }
        i += 1;
    }
    out
}

/// This build's identity record.
pub const IDENTITY: [u8; IDENTITY_LEN] = identity_record();

/// Why a peer's identity record does not agree with this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skew {
    /// The bytes are not an identity record: the magic, the length or the count is wrong.
    Malformed,
    /// The peer does not list a family this build needs.
    Missing { family: u8 },
    /// The peer lists the family with another hash.
    Hash { family: u8, ours: u64, theirs: u64 },
}

/// Checks a peer's identity record against this build.
pub fn check(record: &[u8]) -> Result<(), Skew> {
    if record.len() < HEAD_LEN || record[0..2] != IDENTITY_MAGIC {
        return Err(Skew::Malformed);
    }
    let length = usize::from(u16::from_le_bytes([record[2], record[3]]));
    let count = usize::from(record[4]);
    if length < HEAD_LEN + count * ENTRY_LEN || length > record.len() {
        return Err(Skew::Malformed);
    }
    let entries = &record[HEAD_LEN..HEAD_LEN + count * ENTRY_LEN];
    for (family, ours) in FAMILIES.iter().zip(FAMILY_HASHES) {
        let mut found = None;
        for entry in entries.as_chunks::<ENTRY_LEN>().0 {
            if entry[0] == *family {
                let mut hash = [0u8; 8];
                hash.copy_from_slice(&entry[1..9]);
                found = Some(u64::from_le_bytes(hash));
                break;
            }
        }
        match found {
            None => return Err(Skew::Missing { family: *family }),
            Some(theirs) if theirs != ours => {
                return Err(Skew::Hash {
                    family: *family,
                    ours,
                    theirs,
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FAMILY_OPS, FAMILY_RESET};

    fn with_hash(record: &[u8], family: u8, hash: u64) -> Vec<u8> {
        let mut out = record.to_vec();
        for k in 0..count(&out) {
            let at = HEAD_LEN + k * ENTRY_LEN;
            if out[at] == family {
                out[at + 1..at + 9].copy_from_slice(&hash.to_le_bytes());
            }
        }
        out
    }

    fn count(record: &[u8]) -> usize {
        usize::from(record[4])
    }

    #[test]
    fn this_build_agrees_with_itself() {
        assert_eq!(check(&IDENTITY), Ok(()));
    }

    #[test]
    fn a_record_has_the_magic_the_length_and_one_entry_per_family() {
        assert_eq!(&IDENTITY[0..2], b"LI");
        assert_eq!(
            usize::from(u16::from_le_bytes([IDENTITY[2], IDENTITY[3]])),
            IDENTITY_LEN
        );
        assert_eq!(usize::from(IDENTITY[4]), FAMILY_COUNT);
        assert_eq!(IDENTITY_LEN, 5 + 5 * 9);
    }

    #[test]
    fn a_trailing_field_is_skipped_and_moves_no_hash() {
        let mut longer = IDENTITY.to_vec();
        longer.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let len = (longer.len() as u16).to_le_bytes();
        longer[2..4].copy_from_slice(&len);
        assert_eq!(check(&longer), Ok(()));
    }

    #[test]
    fn a_family_this_build_does_not_know_is_ignored() {
        let mut more = IDENTITY.to_vec();
        more.extend_from_slice(&[99, 1, 2, 3, 4, 5, 6, 7, 8]);
        more[4] = (FAMILY_COUNT + 1) as u8;
        let len = (more.len() as u16).to_le_bytes();
        more[2..4].copy_from_slice(&len);
        assert_eq!(check(&more), Ok(()));
    }

    #[test]
    fn a_hash_mismatch_is_refused_by_family() {
        let bad = with_hash(&IDENTITY, FAMILY_OPS, FAMILY_HASHES[1] ^ 1);
        assert_eq!(
            check(&bad),
            Err(Skew::Hash {
                family: FAMILY_OPS,
                ours: FAMILY_HASHES[1],
                theirs: FAMILY_HASHES[1] ^ 1,
            })
        );
    }

    #[test]
    fn a_missing_family_is_refused_by_family() {
        let mut short = IDENTITY.to_vec();
        // Drop the reset family's entry: the count drops by one and its bytes go.
        let at = HEAD_LEN + 3 * ENTRY_LEN;
        short.drain(at..at + ENTRY_LEN);
        short[4] = (FAMILY_COUNT - 1) as u8;
        let len = (short.len() as u16).to_le_bytes();
        short[2..4].copy_from_slice(&len);
        assert_eq!(
            check(&short),
            Err(Skew::Missing {
                family: FAMILY_RESET
            })
        );
    }

    #[test]
    fn bytes_that_are_not_a_record_are_malformed() {
        assert_eq!(check(&[]), Err(Skew::Malformed));
        let mut magic = IDENTITY;
        magic[0] = b'X';
        assert_eq!(check(&magic), Err(Skew::Malformed));
        let mut long = IDENTITY;
        long[2..4].copy_from_slice(&200u16.to_le_bytes());
        assert_eq!(
            check(&long),
            Err(Skew::Malformed),
            "a length past the bytes given"
        );
        let mut count = IDENTITY;
        count[4] = 9;
        assert_eq!(
            check(&count),
            Err(Skew::Malformed),
            "entries past the length"
        );
    }
}
