//! The identity record: what a stage says it speaks, and how a PC side checks it.
//!
//! A stage answers an identify request with this record:
//!
//! ```text
//! magic     2 bytes   "LI"
//! length    u16 LE    the whole record: everything up to and including the trailing bytes
//! count     u8        how many family entries follow
//! entries   count x   family (u8), then the family's hash (u64 LE)
//! build     u64 LE    the build id of the stage image (0 = unset; see README)
//! regions   u8, then MAX_REGIONS x (first u32 LE, end u32 LE, class u8); unused slots are zero
//! trailing  ...       bytes a reader does not know: skipped (the length covers them)
//! ```
//!
//! [`IDENTITY`] is this build's record. [`check`] reads a peer's record and compares it with this build
//! family by family. A family this build knows that the peer does not list, or lists with another hash,
//! is a typed refusal ([`Skew`]). A family listed twice is refused too: it is never resolved by taking the
//! first. A family this build does not know is ignored, so a peer built with an added family still
//! connects, and trailing bytes are skipped.

use crate::layout::{FAMILIES, FAMILY_COUNT, FAMILY_HASHES};
use crate::region::{Class, MAX_REGIONS, Region};

/// The magic at the start of an identity record.
pub const IDENTITY_MAGIC: [u8; 2] = *b"LI";
/// The bytes before the family entries: magic, length, count.
pub const HEAD_LEN: usize = 5;
/// The bytes of one family entry: family, hash.
pub const ENTRY_LEN: usize = 9;
/// The bytes of the build id.
pub const BUILD_LEN: usize = 8;
/// The bytes of one published region: first, end, class.
pub const REGION_LEN: usize = 9;
/// The length of this build's record.
pub const IDENTITY_LEN: usize =
    HEAD_LEN + FAMILY_COUNT * ENTRY_LEN + BUILD_LEN + 1 + MAX_REGIONS * REGION_LEN;

/// The most family entries a peer's record may carry, which bounds how far a reader looks. Part of the wire: it is
/// folded into the identity family's hash.
pub const MAX_PEER_ENTRIES: usize = 16;

/// Builds this stage's identity record: its build id and its region table.
///
/// # Panics
///
/// At compile time, when more than [`MAX_REGIONS`] regions are given.
pub const fn identity_record(build_id: u64, regions: &[Region]) -> [u8; IDENTITY_LEN] {
    assert!(
        regions.len() <= MAX_REGIONS,
        "too many regions for one identity record"
    );
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
    let build = HEAD_LEN + FAMILY_COUNT * ENTRY_LEN;
    let id = build_id.to_le_bytes();
    let mut k = 0;
    while k < BUILD_LEN {
        out[build + k] = id[k];
        k += 1;
    }
    let count = build + BUILD_LEN;
    out[count] = regions.len() as u8;
    let mut r = 0;
    while r < regions.len() {
        let at = count + 1 + r * REGION_LEN;
        let first = regions[r].first.to_le_bytes();
        let end = regions[r].end.to_le_bytes();
        let mut k = 0;
        while k < 4 {
            out[at + k] = first[k];
            out[at + 4 + k] = end[k];
            k += 1;
        }
        out[at + 8] = regions[r].class.to_wire();
        r += 1;
    }
    out
}

/// What a peer's record says, once it agrees with this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The peer's build id; zero means the peer did not set one.
    pub build_id: u64,
    /// The peer's region table, in order; only the first `region_count` are meaningful.
    pub regions: [Region; MAX_REGIONS],
    /// How many of `regions` the peer published.
    pub region_count: usize,
}

impl Peer {
    /// The peer's regions.
    pub fn regions(&self) -> &[Region] {
        &self.regions[..self.region_count]
    }
}

/// Why a peer's identity record does not agree with this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skew {
    /// The bytes are not an identity record: the magic, the length, the count, the build id or the regions
    /// are wrong, or a region's class is not one this build knows.
    Malformed,
    /// The peer lists a family twice. It is refused, not resolved by taking one of them.
    Duplicate {
        /// The family listed twice.
        family: u8,
    },
    /// The peer does not list a family this build needs.
    Missing {
        /// The family the peer does not list.
        family: u8,
    },
    /// The peer lists the family with another hash.
    Hash {
        /// The family with another hash.
        family: u8,
        /// This build's hash.
        ours: u64,
        /// The peer's hash.
        theirs: u64,
    },
}

/// Checks a peer's identity record against this build, and returns what the peer says about itself.
pub fn check(record: &[u8]) -> Result<Peer, Skew> {
    if record.len() < HEAD_LEN || record[0..2] != IDENTITY_MAGIC {
        return Err(Skew::Malformed);
    }
    let length = usize::from(u16::from_le_bytes([record[2], record[3]]));
    let count = usize::from(record[4]);
    if length > record.len() || count > MAX_PEER_ENTRIES || HEAD_LEN + count * ENTRY_LEN > length {
        return Err(Skew::Malformed);
    }
    // Collect the peer's families, refusing any that is listed twice.
    let mut seen: [(u8, u64); MAX_PEER_ENTRIES] = [(0, 0); MAX_PEER_ENTRIES];
    for (n, entry) in record[HEAD_LEN..HEAD_LEN + count * ENTRY_LEN]
        .as_chunks::<ENTRY_LEN>()
        .0
        .iter()
        .enumerate()
    {
        let mut hash = [0u8; 8];
        hash.copy_from_slice(&entry[1..9]);
        let family = entry[0];
        if seen[..n].iter().any(|(f, _)| *f == family) {
            return Err(Skew::Duplicate { family });
        }
        seen[n] = (family, u64::from_le_bytes(hash));
    }
    let peer = &seen[..count];
    for (family, ours) in FAMILIES.iter().zip(FAMILY_HASHES) {
        match peer.iter().find(|(f, _)| f == family) {
            None => return Err(Skew::Missing { family: *family }),
            Some((_, theirs)) if *theirs != ours => {
                return Err(Skew::Hash {
                    family: *family,
                    ours,
                    theirs: *theirs,
                });
            }
            Some(_) => {}
        }
    }
    // The build id and the regions follow the entries. Bytes past them, up to the length, are trailing.
    let build = HEAD_LEN + count * ENTRY_LEN;
    let regions_at = build + BUILD_LEN;
    if regions_at + 1 > length {
        return Err(Skew::Malformed);
    }
    let mut id = [0u8; 8];
    id.copy_from_slice(&record[build..regions_at]);
    let region_count = usize::from(record[regions_at]);
    if region_count > MAX_REGIONS || regions_at + 1 + region_count * REGION_LEN > length {
        return Err(Skew::Malformed);
    }
    let mut regions = [Region {
        first: 0,
        end: 0,
        class: Class::Plain,
    }; MAX_REGIONS];
    for (r, slot) in regions.iter_mut().enumerate().take(region_count) {
        let at = regions_at + 1 + r * REGION_LEN;
        let class = Class::from_wire(record[at + 8]).ok_or(Skew::Malformed)?;
        *slot = Region {
            first: u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]]),
            end: u32::from_le_bytes([
                record[at + 4],
                record[at + 5],
                record[at + 6],
                record[at + 7],
            ]),
            class,
        };
    }
    Ok(Peer {
        build_id: u64::from_le_bytes(id),
        regions,
        region_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FAMILY_OPS, FAMILY_RESET};

    const TABLE: [Region; 2] = [
        Region {
            first: 64,
            end: 32_768,
            class: Class::BootArea,
        },
        Region {
            first: 64,
            end: 65,
            class: Class::Header,
        },
    ];

    fn record() -> [u8; IDENTITY_LEN] {
        identity_record(0x1122_3344_5566_7788, &TABLE)
    }

    fn with_hash(record: &[u8], family: u8, hash: u64) -> Vec<u8> {
        let mut out = record.to_vec();
        for k in 0..usize::from(out[4]) {
            let at = HEAD_LEN + k * ENTRY_LEN;
            if out[at] == family {
                out[at + 1..at + 9].copy_from_slice(&hash.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn this_build_agrees_with_itself_and_publishes_its_table() {
        let peer = check(&record()).unwrap();
        assert_eq!(peer.build_id, 0x1122_3344_5566_7788);
        assert_eq!(peer.regions(), &TABLE[..]);
    }

    #[test]
    fn the_record_has_its_fixed_length_and_magic() {
        let r = record();
        assert_eq!(&r[0..2], b"LI");
        assert_eq!(usize::from(u16::from_le_bytes([r[2], r[3]])), IDENTITY_LEN);
        assert_eq!(usize::from(r[4]), FAMILIES.len());
        assert_eq!(IDENTITY_LEN, 5 + 6 * 9 + 8 + 1 + 4 * 9);
    }

    #[test]
    fn trailing_bytes_are_skipped_and_move_no_hash() {
        let mut longer = record().to_vec();
        longer.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let len = (longer.len() as u16).to_le_bytes();
        longer[2..4].copy_from_slice(&len);
        assert!(check(&longer).is_ok());
    }

    #[test]
    fn a_family_this_build_does_not_know_is_ignored() {
        // Insert an unknown family entry (id 99) before the build id, and bump the count and length.
        let mut r = record().to_vec();
        let at = HEAD_LEN + FAMILIES.len() * ENTRY_LEN;
        let mut entry = vec![99u8];
        entry.extend_from_slice(&7u64.to_le_bytes());
        r.splice(at..at, entry);
        r[4] = (FAMILIES.len() + 1) as u8;
        let len = (r.len() as u16).to_le_bytes();
        r[2..4].copy_from_slice(&len);
        assert!(check(&r).is_ok());
    }

    #[test]
    fn a_hash_mismatch_is_refused_by_family() {
        let bad = with_hash(&record(), FAMILY_OPS, FAMILY_HASHES[1] ^ 1);
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
    fn a_family_listed_twice_is_refused_not_resolved() {
        // Make the second entry a copy of the first (the reset family) with the same hash.
        let mut r = record().to_vec();
        let first = HEAD_LEN + 3 * ENTRY_LEN;
        let copy = r[first..first + ENTRY_LEN].to_vec();
        r.splice(first + ENTRY_LEN..first + ENTRY_LEN, copy);
        r[4] = (FAMILIES.len() + 1) as u8;
        let len = (r.len() as u16).to_le_bytes();
        r[2..4].copy_from_slice(&len);
        assert_eq!(
            check(&r),
            Err(Skew::Duplicate {
                family: FAMILY_RESET
            })
        );
    }

    #[test]
    fn a_missing_family_is_refused_by_family() {
        let mut short = record().to_vec();
        let at = HEAD_LEN + 3 * ENTRY_LEN;
        short.drain(at..at + ENTRY_LEN);
        short[4] = (FAMILIES.len() - 1) as u8;
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
        let mut magic = record();
        magic[0] = b'X';
        assert_eq!(check(&magic), Err(Skew::Malformed));
        let mut long = record();
        long[2..4].copy_from_slice(&400u16.to_le_bytes());
        assert_eq!(
            check(&long),
            Err(Skew::Malformed),
            "a length past the bytes given"
        );
        let mut count = record();
        count[4] = 12;
        assert_eq!(
            check(&count),
            Err(Skew::Malformed),
            "twelve entries are past the length"
        );
        let mut bad_class = record();
        let class_at = HEAD_LEN + FAMILIES.len() * ENTRY_LEN + BUILD_LEN + 1 + 8;
        bad_class[class_at] = 7;
        assert_eq!(check(&bad_class), Err(Skew::Malformed));
        let mut too_many = record();
        let regions_at = HEAD_LEN + FAMILIES.len() * ENTRY_LEN + BUILD_LEN;
        too_many[regions_at] = (MAX_REGIONS + 1) as u8;
        assert_eq!(check(&too_many), Err(Skew::Malformed));
    }
}
