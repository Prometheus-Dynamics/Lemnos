//! The card's regions, as the stage polices them. The table is part of the wire: a stage publishes it in
//! its identity record ([`crate::identity`]), so a host can predict which writes are refused without
//! the stage's source.
//!
//! The regions are sector ranges `first..end` (end exclusive) and a [`Class`]. The stage classifies a
//! write as the strictest class of any region it overlaps.

use crate::hash;

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

impl Class {
    /// The class's byte on the wire.
    pub const fn to_wire(self) -> u8 {
        match self {
            Class::Plain => 0,
            Class::BootArea => 1,
            Class::Header => 2,
        }
    }

    /// The class a byte names, if it names one.
    pub const fn from_wire(byte: u8) -> Option<Class> {
        match byte {
            0 => Some(Class::Plain),
            1 => Some(Class::BootArea),
            2 => Some(Class::Header),
            _ => None,
        }
    }
}

/// A region: sectors `first..end` (end exclusive), and its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// The first sector.
    pub first: u32,
    /// The sector after the last.
    pub end: u32,
    /// The class.
    pub class: Class,
}

/// The most regions one identity record publishes.
pub const MAX_REGIONS: usize = 4;

/// Folds a region table into a hash: the count, then each region's first, end and class. A host that
/// pins a table compares this, and the table's own values in the record, to be sure they agree.
pub const fn region_digest(regions: &[Region]) -> u64 {
    let mut h = hash::text(hash::OFFSET, "regions");
    h = hash::word(h, regions.len() as u64);
    let mut i = 0;
    while i < regions.len() {
        h = hash::word(h, regions[i].first as u64);
        h = hash::word(h, regions[i].end as u64);
        h = hash::byte(h, regions[i].class.to_wire());
        i += 1;
    }
    h
}

/// The strictest class of any region that a write of `count` sectors at `lba` overlaps. A write that overlaps no
/// region is [`Class::Plain`]. The stage gates a write by this, and the host's compare rule (see [`decide`]) uses the
/// same function, so the two cannot disagree about which writes need a mark.
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

/// The host's decision for one write, under the compare rule (the wire README, "The compare rule").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The write is plain data. No compare is needed, and no mark is set.
    Write { mark: bool },
    /// The write's range already equals the image. Nothing is written, and no mark is set.
    Skip,
}

/// Why the host cannot decide: the two byte strings it compares are not the same length as the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecideError {
    /// The card's read or the image is not `count * 512` bytes. The write is not made.
    Length,
}

/// The compare rule, as code. For a write of `count` sectors at `lba`:
///
/// - **Plain** (no region of the write is a boot area or a header): the write is made without a mark. No compare is
///   required by the stage, and none is made here.
/// - **Boot area or header**, whether the write lies wholly inside the region or straddles into it: the host compares
///   **every sector of the write**, from `lba` to `lba + count`, byte for byte, the card's read against the image it
///   means to write (the baseline). If all bytes are equal, the write is [`Decision::Skip`]: nothing is written and no
///   mark is set. If any byte differs, the write is made with the mark. The mark is never set on a range the host has
///   not compared, and never when the compare found equality.
///
/// `card` and `image` are the read of the range from the card and the image bytes for the same range, each
/// `count * 512` bytes, read fresh under the validity rule of the README.
pub fn decide(
    regions: &[Region],
    lba: u32,
    count: u16,
    card: &[u8],
    image: &[u8],
) -> Result<Decision, DecideError> {
    let len = usize::from(count) * 512;
    if card.len() != len || image.len() != len {
        return Err(DecideError::Length);
    }
    match classify(regions, lba, count) {
        Class::Plain => Ok(Decision::Write { mark: false }),
        Class::BootArea | Class::Header => {
            if card == image {
                Ok(Decision::Skip)
            } else {
                Ok(Decision::Write { mark: true })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! One test per case of the compare rule. The text of the README's rule and these cases must match.
    use super::*;

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

    fn bytes(fill: u8, sectors: u16) -> Vec<u8> {
        vec![fill; usize::from(sectors) * 512]
    }

    #[test]
    fn a_plain_write_is_made_without_a_mark_and_needs_no_compare() {
        // Sector 40000 is past every region. Even a differing card does not change the decision.
        assert_eq!(
            decide(&REGIONS, 40_000, 1, &bytes(1, 1), &bytes(2, 1)),
            Ok(Decision::Write { mark: false })
        );
    }

    #[test]
    fn a_boot_area_write_whose_range_equals_the_image_is_skipped() {
        assert_eq!(
            decide(&REGIONS, 100, 1, &bytes(7, 1), &bytes(7, 1)),
            Ok(Decision::Skip)
        );
    }

    #[test]
    fn a_boot_area_write_whose_range_differs_is_made_with_the_mark() {
        assert_eq!(
            decide(&REGIONS, 100, 1, &bytes(7, 1), &bytes(8, 1)),
            Ok(Decision::Write { mark: true })
        );
    }

    #[test]
    fn a_header_write_whose_range_equals_the_image_is_skipped_and_a_differing_one_is_marked() {
        assert_eq!(
            decide(&REGIONS, 64, 1, &bytes(7, 1), &bytes(7, 1)),
            Ok(Decision::Skip)
        );
        assert_eq!(
            decide(&REGIONS, 64, 1, &bytes(7, 1), &bytes(8, 1)),
            Ok(Decision::Write { mark: true })
        );
    }

    #[test]
    fn a_straddling_write_compares_every_sector_of_it_so_an_equal_boot_part_and_a_differing_plain_part_is_marked()
     {
        // Sectors 63 (plain) and 64 (header). Only sector 63 differs: the whole write is compared, so it is marked.
        let mut card = bytes(7, 2);
        let mut image = bytes(7, 2);
        card[0] = 1;
        image[0] = 2;
        assert_eq!(
            decide(&REGIONS, 63, 2, &card, &image),
            Ok(Decision::Write { mark: true })
        );
    }

    #[test]
    fn a_straddling_write_whose_every_sector_equals_the_image_is_skipped() {
        assert_eq!(
            decide(&REGIONS, 63, 2, &bytes(7, 2), &bytes(7, 2)),
            Ok(Decision::Skip)
        );
    }

    #[test]
    fn a_compare_of_the_wrong_length_is_refused_and_nothing_is_decided() {
        assert_eq!(
            decide(&REGIONS, 100, 2, &bytes(7, 1), &bytes(7, 2)),
            Err(DecideError::Length)
        );
    }

    #[test]
    fn the_stage_and_the_host_classify_a_straddle_the_same_way() {
        assert_eq!(classify(&REGIONS, 63, 2), Class::Header);
        assert_eq!(
            classify(&REGIONS, 60, 4),
            Class::Plain,
            "a write ending before the boot area is plain"
        );
    }
}
