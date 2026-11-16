//! The order rules, enforced by the stage rather than by the host.
//!
//! A chunk is written only through three steps, each of which is a type:
//! 1. [`verify`] checks the chunk's CRC and yields a [`Verified`]. (Rule B: a header or boot-record write
//!    is refused before its payload is verified; every write is gated the same way.)
//! 2. [`gate`] checks the region's class against the request's compared mark and yields an [`Admitted`]
//!    only if the write may go. (Rule A: a boot-area write without the mark is refused. A header needs the
//!    mark too, because it is inside the boot area.)
//! 3. The storage write is given the payload of an [`Admitted`] and nothing else.
//!
//! What the stage can check is the payload's CRC and the region gating. It cannot know that the host
//! compared anything: the mark is the host's word, and the compare rule for it is in the wire's README.

pub use lemnos_flash_wire::region::{
    Class, MAX_REGIONS, Region, classify, region_digest, wholly_inside_one_class,
};

use lemnos_flash_wire::frame::crc16_ccitt;

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

/// Checks a chunk: its payload then its CRC-16 (big-endian). `None` when the CRC does not match or the chunk
/// is too short to hold one.
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

    fn chunk(payload: &[u8], corrupt: bool) -> Vec<u8> {
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
        assert_eq!(classify(&REGIONS, 60, 10), Class::Header);
        assert_eq!(classify(&REGIONS, 40_000, 1), Class::Plain);
    }

    #[test]
    fn a_write_that_ends_just_before_a_region_does_not_overlap_it() {
        assert_eq!(classify(&REGIONS, 60, 4), Class::Plain);
    }

    #[test]
    fn a_chunk_is_verified_only_when_its_crc_matches() {
        let good = chunk(&[1, 2, 3], false);
        assert_eq!(verify(&good).map(|v| v.payload()), Some(&[1u8, 2, 3][..]));
        assert!(verify(&chunk(&[1, 2, 3], true)).is_none());
        assert!(verify(&[7]).is_none());
    }

    #[test]
    fn plain_data_is_admitted_once_verified() {
        let c = chunk(&[9; 4], false);
        assert_eq!(
            gate(Class::Plain, false, verify(&c).unwrap())
                .unwrap()
                .payload(),
            &[9u8; 4][..]
        );
    }

    #[test]
    fn a_boot_area_or_header_write_needs_the_mark() {
        let c = chunk(&[5; 4], false);
        for class in [Class::BootArea, Class::Header] {
            assert_eq!(
                gate(class, false, verify(&c).unwrap()).unwrap_err(),
                Refusal::NotCompared
            );
            assert!(gate(class, true, verify(&c).unwrap()).is_ok());
        }
    }

    #[test]
    fn a_bad_chunk_never_reaches_the_gate() {
        assert!(verify(&chunk(&[5; 4], true)).is_none());
    }
}
