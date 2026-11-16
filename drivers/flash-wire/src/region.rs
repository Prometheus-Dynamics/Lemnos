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
