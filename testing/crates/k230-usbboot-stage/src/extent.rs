//! The whole-image flash path's extent (FLASH-EXTENT): the sectors one flash run may touch, declared by its job.
//!
//! This is the flash path's bounds, and it is not the bench's. The bench's card stages bound themselves to a build's
//! declared scratch range and refuse the protected sectors ([`crate::bounds`]); a real flash must write the GPT and the
//! boot headers, so the flash path has no protected list. What it has instead:
//! - every piece must carry a declared extent (a piece with none is refused);
//! - a write, zero or write-test piece must lie wholly inside it (a piece that straddles its end is refused whole);
//! - the extent must lie wholly inside the card, by the card's own size.
//!
//! The extent is in each job's header (`piece.rs`, words 13 and 14, the flag [`crate::piece::EXTENT_FLAG`]). A stage
//! is one boot-ROM load per piece and resets after each, so it holds nothing between pieces: each job restates the
//! whole run's extent, and the stage checks each piece against its own job.
//!
//! The types differ on purpose: [`Extent`] is built only from a flash job's header, and [`crate::bounds::Declared`]
//! only from a build's scratch range. Neither can be given to the other's check.

/// The refusal for a piece with no declared extent (an old host's job, or a job that did not declare one).
pub const NO_EXTENT: &str = "piece refused: no declared extent";
/// The refusal for an extent with no sectors.
pub const EMPTY: &str = "piece refused: the declared extent is empty";
/// The refusal for an extent that runs past the card's end.
pub const PAST_CARD: &str = "piece refused: the declared extent runs past the card";
/// The refusal for a write, zero or write-test piece that is not wholly inside the extent.
pub const OUTSIDE: &str = "piece refused: the piece is not wholly inside the declared extent";

/// The sectors a flash run may touch: `count` sectors from `first`. Sectors are 32 bits in the header, so an extent
/// names sectors below 2^32 (2 TiB); a card past that cannot be declared whole and its job is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    first: u32,
    count: u32,
}

impl Extent {
    /// An extent of `count` sectors from `first`. An extent with no sectors is refused.
    pub fn new(first: u32, count: u32) -> Result<Self, &'static str> {
        if count == 0 {
            return Err(EMPTY);
        }
        Ok(Self { first, count })
    }

    /// The first sector.
    pub fn first(&self) -> u32 {
        self.first
    }

    /// The number of sectors.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// One past the last sector.
    pub fn end(&self) -> u64 {
        u64::from(self.first) + u64::from(self.count)
    }

    /// Whether `count` sectors at `lba` lie wholly inside the extent.
    pub fn holds(&self, lba: u64, count: u64) -> bool {
        lba >= u64::from(self.first) && lba.checked_add(count).is_some_and(|end| end <= self.end())
    }

    /// Whether the whole extent lies inside a card of `blocks` sectors.
    pub fn within(&self, blocks: u64) -> bool {
        self.end() <= blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extent_holds_exactly_the_sectors_it_names() {
        let e = Extent::new(1_048_576, 4).expect("four sectors");
        assert!(e.holds(1_048_576, 4));
        assert!(e.holds(1_048_579, 1));
        assert!(!e.holds(1_048_575, 1), "one before");
        assert!(!e.holds(1_048_579, 2), "straddling the end");
        assert!(!e.holds(1_048_580, 1), "one past");
        assert!(!e.holds(u64::MAX, 2), "no wrap");
        assert!(e.within(1_048_580) && !e.within(1_048_579));
    }

    #[test]
    fn an_empty_extent_is_refused() {
        assert_eq!(Extent::new(0, 0), Err(EMPTY));
    }
}
