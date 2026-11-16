//! The order rules, enforced by the stage rather than by the host.
//!
//! A chunk is written only through three steps, each of which is a type:
//! 1. [`verify`] checks the chunk's CRC and yields a [`Verified`]. No write takes anything else, so no
//!    write happens before the payload is verified. (Rule B: a header or boot-record write is refused
//!    before its payload is verified, and every class is gated the same way.)
//! 2. [`gate`] checks the region's class against the request's compared mark and yields an [`Admitted`]
//!    only if the write may go. A boot-area write without the mark is refused. (Rule A.) A header or
//!    boot-record write needs the mark as well, because it is inside the boot area.
//! 3. The storage write takes the [`Admitted`] payload and nothing else.
//!
//! The regions are the chip's: a stage passes its own table, and [`classify`] returns the strictest class
//! any sector of the write falls in, so a write that straddles a boundary is classed by its strictest part.

use lemnos_flash_wire::frame::crc16_ccitt;

/// The class of a region. Ordered from least to most strict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Ordinary data: a verified payload is enough.
    Plain,
    /// The boot area: a write needs the compared mark.
    BootArea,
    /// A header or boot record: a write needs the compared mark, and its payload verified first.
    Header,
}

/// A region of sectors, `first` up to `end` (exclusive), and its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub first: u32,
    pub end: u32,
    pub class: Class,
}

/// The strictest class of any region that a write of `count` sectors at `lba` overlaps. A write that
/// overlaps no region is [`Class::Plain`].
pub fn classify(regions: &[Region], lba: u32, count: u16) -> Class {
    let first = u64::from(lba);
    let end = first + u64::from(count);
    let mut class = Class::Plain;
    for region in regions {
        if first < u64::from(region.end) && u64::from(region.first) < end && region.class > class {
            class = region.class;
        }
    }
    class
}

/// A chunk whose CRC matched its payload. Only [`verify`] makes one.
#[derive(Debug, PartialEq, Eq)]
pub struct Verified<'a> {
    payload: &'a [u8],
}

impl<'a> Verified<'a> {
    /// The payload that matched its CRC.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// Checks a chunk: its payload then its CRC-16 (big-endian). `None` when the CRC does not match or the
/// chunk is too short to hold one.
pub fn verify(chunk: &[u8]) -> Option<Verified<'_>> {
    let (payload, crc) = chunk.split_at(chunk.len().checked_sub(2)?);
    (crc16_ccitt(payload) == u16::from_be_bytes([crc[0], crc[1]])).then_some(Verified { payload })
}

/// Why a write may not go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The region needs the compared mark and the request did not carry it.
    NotCompared,
}

/// A payload cleared to be written. Only [`gate`] makes one.
#[derive(Debug, PartialEq, Eq)]
pub struct Admitted<'a> {
    payload: &'a [u8],
}

impl<'a> Admitted<'a> {
    /// The payload to write.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// Clears a verified payload for a region of `class`. A boot area and a header need the compared mark.
pub fn gate<'a>(
    class: Class,
    compared: bool,
    verified: Verified<'a>,
) -> Result<Admitted<'a>, Refusal> {
    match class {
        Class::Plain => {}
        Class::BootArea | Class::Header if compared => {}
        Class::BootArea | Class::Header => return Err(Refusal::NotCompared),
    }
    Ok(Admitted {
        payload: verified.payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_flash_wire::frame::payload_crc;

    const REGIONS: [Region; 2] = [
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

    fn chunk(payload: &[u8], corrupt: bool) -> std::vec::Vec<u8> {
        let mut out = payload.to_vec();
        let mut crc = payload_crc(payload);
        if corrupt {
            crc[1] ^= 1;
        }
        out.extend_from_slice(&crc);
        out
    }

    #[test]
    fn the_strictest_class_of_an_overlap_wins() {
        assert_eq!(classify(&REGIONS, 0, 1), Class::Plain);
        assert_eq!(classify(&REGIONS, 100, 1), Class::BootArea);
        assert_eq!(classify(&REGIONS, 64, 1), Class::Header);
        // A write that starts in plain data and runs into the boot area is a boot-area write.
        assert_eq!(classify(&REGIONS, 60, 10), Class::Header);
        assert_eq!(classify(&REGIONS, 40_000, 1), Class::Plain);
    }

    #[test]
    fn a_write_that_ends_just_before_a_region_does_not_overlap_it() {
        assert_eq!(
            classify(&REGIONS, 60, 4),
            Class::Plain,
            "sectors 60..64 end where the boot area starts"
        );
    }

    #[test]
    fn a_chunk_is_verified_only_when_its_crc_matches() {
        let good = chunk(&[1, 2, 3], false);
        assert_eq!(verify(&good).map(|v| v.payload()), Some(&[1u8, 2, 3][..]));
        assert!(verify(&chunk(&[1, 2, 3], true)).is_none());
        assert!(verify(&[7]).is_none(), "too short to hold a CRC");
    }

    #[test]
    fn plain_data_is_admitted_once_verified() {
        let c = chunk(&[9; 4], false);
        let admitted = gate(Class::Plain, false, verify(&c).unwrap()).unwrap();
        assert_eq!(admitted.payload(), &[9u8; 4][..]);
    }

    #[test]
    fn a_boot_area_write_without_the_mark_is_refused_and_with_it_is_admitted() {
        let c = chunk(&[5; 4], false);
        assert_eq!(
            gate(Class::BootArea, false, verify(&c).unwrap()).unwrap_err(),
            Refusal::NotCompared
        );
        assert!(gate(Class::BootArea, true, verify(&c).unwrap()).is_ok());
    }

    #[test]
    fn a_header_write_needs_the_mark_too() {
        let c = chunk(&[5; 4], false);
        assert_eq!(
            gate(Class::Header, false, verify(&c).unwrap()).unwrap_err(),
            Refusal::NotCompared
        );
        assert!(gate(Class::Header, true, verify(&c).unwrap()).is_ok());
    }

    #[test]
    fn the_gate_takes_only_a_verified_chunk_so_an_unverified_header_cannot_reach_it() {
        // The types are the rule: `gate` takes a `Verified`, which only `verify` builds, and `verify`
        // answers `None` for this chunk. So there is no call that admits a header with a bad CRC.
        let bad = chunk(&[5; 4], true);
        assert!(verify(&bad).is_none());
    }
}
