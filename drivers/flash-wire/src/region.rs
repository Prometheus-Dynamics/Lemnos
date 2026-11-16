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

/// Whether a write of `count` sectors at `lba` lies wholly inside one class. A write is one class: the stage refuses a
/// chunk that crosses a region boundary (a start or an end of any region strictly inside the write), with a bad-request
/// status, and the host splits its writes at the boundaries of the region table it read from the identity record.
///
/// A boundary exactly at `lba` or exactly at `lba + count` does not cross the write: the write starts or ends there.
pub fn wholly_inside_one_class(regions: &[Region], lba: u32, count: u16) -> bool {
    let first = u64::from(lba);
    let end = first + u64::from(count);
    !regions.iter().any(|region| {
        [region.first, region.end]
            .iter()
            .any(|&b| first < u64::from(b) && u64::from(b) < end)
    })
}

/// The host's decision for one write that lies wholly inside one class, under the compare rule (the wire README,
/// "The compare rule").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The write is plain data. No compare is needed, and no mark is set.
    Write { mark: bool },
    /// The write's range already equals the image. Nothing is written, and no mark is set.
    Skip,
}

/// Why [`decide`] refuses to decide. The host checks both before it compares, so a refusal here is a host bug: the
/// caller must stop, not write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecideError {
    /// The card's read or the image is not the write's length (`count * 512` bytes each).
    Length,
    /// The write crosses a region boundary: the host splits the write at region boundaries first.
    Straddle,
}

/// The compare rule, as code, for a write that [`wholly_inside_one_class`] accepts.
///
/// - **Plain**: the write is made without a mark. No compare is required, and none is made here.
/// - **Boot area or header**: the host has compared **every sector of the write**, byte for byte, the card's read of the
///   range (`card`) against the image bytes the host means to write for the same range (`image`). If the two are equal
///   the write is [`Decision::Skip`]. If any byte differs it is made with the mark.
///
/// `card` and `image` must each be `count * 512` bytes; any other length is [`DecideError::Length`]. A write that crosses
/// a region boundary is [`DecideError::Straddle`]. Both checks run on every call, in every build.
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
    if !wholly_inside_one_class(regions, lba, count) {
        return Err(DecideError::Straddle);
    }
    Ok(match classify(regions, lba, count) {
        Class::Plain => Decision::Write { mark: false },
        Class::BootArea | Class::Header => {
            if card == image {
                Decision::Skip
            } else {
                Decision::Write { mark: true }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    //! One test per case of the compare rule, and one refusal test per boundary of each class. The text of the wire
    //! README's rule and these cases must match.
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
    fn every_sector_of_a_multi_sector_write_is_compared_and_one_differing_byte_marks_it() {
        let mut card = bytes(7, 3);
        let image = bytes(7, 3);
        card[2 * 512 + 9] = 1;
        assert_eq!(
            decide(&REGIONS, 100, 3, &card, &image),
            Ok(Decision::Write { mark: true })
        );
    }

    #[test]
    fn the_stage_and_the_host_classify_a_write_the_same_way() {
        assert_eq!(
            classify(&REGIONS, 63, 2),
            Class::Header,
            "a straddle classes by its strictest part"
        );
        assert_eq!(
            classify(&REGIONS, 60, 4),
            Class::Plain,
            "a write ending before the boot area is plain"
        );
    }

    // The boundary cases: a write is refused (not wholly inside one class) when a boundary is strictly inside it.

    #[test]
    fn a_write_starting_before_the_boot_area_and_crossing_into_it_is_not_wholly_inside_one_class() {
        // 63..65: the boundary at 64 is inside.
        assert!(!wholly_inside_one_class(&REGIONS, 63, 2));
    }

    #[test]
    fn a_write_starting_at_the_boot_area_start_is_wholly_inside_it() {
        assert!(wholly_inside_one_class(&REGIONS, 64, 1));
    }

    #[test]
    fn a_write_crossing_the_header_end_into_the_boot_area_is_not_wholly_inside_one_class() {
        // 64..66: the header ends at 65, inside the write.
        assert!(!wholly_inside_one_class(&REGIONS, 64, 2));
    }

    #[test]
    fn a_write_starting_at_the_header_end_is_wholly_inside_the_boot_area() {
        assert!(wholly_inside_one_class(&REGIONS, 65, 1));
    }

    #[test]
    fn a_write_crossing_the_boot_area_end_into_plain_data_is_not_wholly_inside_one_class() {
        // 32767..32769: the boot area ends at 32768, inside the write.
        assert!(!wholly_inside_one_class(&REGIONS, 32_767, 2));
    }

    #[test]
    fn a_write_ending_exactly_at_the_boot_area_end_is_wholly_inside_it() {
        assert!(wholly_inside_one_class(&REGIONS, 32_767, 1));
    }

    #[test]
    fn a_write_starting_at_the_boot_area_end_is_wholly_plain() {
        assert!(wholly_inside_one_class(&REGIONS, 32_768, 1));
    }

    #[test]
    fn a_compare_of_the_wrong_length_is_refused_and_nothing_is_decided() {
        // One sector at 100 with a card read of two sectors, and an image of one: both lengths are refused, whichever is
        // wrong, and no decision is made.
        assert_eq!(
            decide(&REGIONS, 100, 1, &bytes(7, 2), &bytes(7, 1)),
            Err(DecideError::Length)
        );
        assert_eq!(
            decide(&REGIONS, 100, 1, &bytes(7, 1), &bytes(7, 2)),
            Err(DecideError::Length)
        );
    }

    #[test]
    fn a_write_crossing_a_region_boundary_is_refused_as_straddle_and_nothing_is_decided() {
        // Sectors 63 and 64: the header starts at 64, so the write crosses into it.
        assert_eq!(
            decide(&REGIONS, 63, 2, &bytes(7, 2), &bytes(7, 2)),
            Err(DecideError::Straddle)
        );
    }
}
