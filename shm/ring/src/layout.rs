//! The shape of a ring. `shm/README.md` is the reference; the numbers here are pinned by tests.
//!
//! Only the declaration's three numbers are stored. Every offset is derived from them, so there
//! is no stored offset for a hostile writer to point somewhere else.

use std::sync::atomic::Ordering;

use crate::{Mem, MemMut, RingError};

/// `SHMRING1`, little-endian.
pub(crate) const MAGIC: u64 = u64::from_le_bytes(*b"SHMRING1");
/// The layout this build reads and writes. Layout 1 is the ring as first published. Layout 2
/// gives word 10 (the end code) a meaning: a layout 1 reader reads a closed or faulted topic as
/// running, so the value had to move. Old and new readers refuse each other's rings, with a
/// [`RingError::Layout`] naming both values where the reader is new.
pub const LAYOUT: u64 = 2;
/// Words in the header.
pub(crate) const HEADER_WORDS: usize = 16;
/// Words in a slot before its payload: stamp, time, meta, lane sequence.
pub(crate) const SLOT_HEADER_WORDS: usize = 4;

pub(crate) const W_MAGIC: usize = 0;
pub(crate) const W_LAYOUT: usize = 1;
/// Fields in the low half, history in the high half.
pub(crate) const W_SHAPE: usize = 2;
/// Largest payload in the low half, expiry in milliseconds in the high half.
pub(crate) const W_SIZES: usize = 3;
pub(crate) const W_TOTAL: usize = 4;
/// How many samples were ever published. Written by the writer only.
pub(crate) const W_HEAD: usize = 8;
/// How many subscribers there are. Written by the ring's owner only.
pub(crate) const W_SUBSCRIBERS: usize = 9;
/// Why the topic ended: 0 while it runs, nonzero once its owner says it has. Written by the ring's
/// owner only.
pub(crate) const W_END: usize = 10;

/// The most fields one ring holds.
pub const MAX_FIELDS: u32 = 65_535;
/// The most samples a field's lane holds.
pub const MAX_HISTORY: u32 = 1 << 24;
/// The largest payload, in bytes.
pub const MAX_PAYLOAD: u32 = 1 << 24;
/// The largest ring, in words: 4 GiB.
const MAX_WORDS: u64 = 1 << 29;

/// What a declaration asks of a ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// How many fields the topic has. At least 1.
    pub fields: u32,
    /// How many samples to keep per field. Rounded up to a power of two, at least 2.
    pub history: u32,
    /// The largest payload, in bytes.
    pub max_payload: u32,
    /// How long a value stays valid, in milliseconds. Zero for always. Carried, not applied here.
    pub expiry_ms: u32,
}

/// Where everything in a ring is, worked out from a [`Spec`]. A reader keeps its own copy from
/// when it attached and never reads the shape again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub(crate) fields: u32,
    pub(crate) history: u32,
    pub(crate) max_payload: u32,
    pub(crate) expiry_ms: u32,
    pub(crate) index_slots: usize,
    pub(crate) slot_words: usize,
    pub(crate) lane_heads_at: usize,
    pub(crate) index_at: usize,
    pub(crate) slots_at: usize,
    pub(crate) total_words: usize,
}

impl Geometry {
    /// The shape for `spec`.
    ///
    /// # Errors
    ///
    /// [`RingError::Spec`] if a number is out of range or the ring would be too large.
    pub fn for_spec(spec: &Spec) -> Result<Self, RingError> {
        if spec.fields == 0 || spec.fields > MAX_FIELDS {
            return Err(RingError::Spec("fields must be 1 to 65535"));
        }
        if spec.history > MAX_HISTORY {
            return Err(RingError::Spec("history is over the limit"));
        }
        if spec.max_payload > MAX_PAYLOAD {
            return Err(RingError::Spec("largest payload is over the limit"));
        }
        let history = spec.history.max(2).next_power_of_two();
        let slot_words = SLOT_HEADER_WORDS as u64 + u64::from(spec.max_payload).div_ceil(8);
        let lanes = u64::from(spec.fields) * u64::from(history);
        let index_slots = lanes.next_power_of_two();
        let lane_heads_at = HEADER_WORDS as u64;
        let index_at = lane_heads_at + u64::from(spec.fields);
        let slots_at = index_at + index_slots;
        // At most 2^40 * 2^21, so this cannot overflow.
        let total = slots_at + lanes * slot_words;
        if total > MAX_WORDS {
            return Err(RingError::Spec("ring would be over 4 GiB"));
        }
        let narrow =
            |value: u64| usize::try_from(value).map_err(|_| RingError::Spec("ring is too large"));
        Ok(Self {
            fields: spec.fields,
            history,
            max_payload: spec.max_payload,
            expiry_ms: spec.expiry_ms,
            index_slots: narrow(index_slots)?,
            slot_words: narrow(slot_words)?,
            lane_heads_at: narrow(lane_heads_at)?,
            index_at: narrow(index_at)?,
            slots_at: narrow(slots_at)?,
            total_words: narrow(total)?,
        })
    }

    /// Reads and checks the shape of the ring in `mem`.
    ///
    /// # Errors
    ///
    /// [`RingError::Malformed`] if this isn't a ring of this layout or it doesn't fit in `mem`.
    pub fn read(mem: &impl Mem) -> Result<Self, RingError> {
        if mem.words() < HEADER_WORDS {
            return Err(RingError::Malformed("shorter than a header"));
        }
        let word = |index| mem.load(index, Ordering::Relaxed);
        // The layout is checked first: every other check below is only meaningful for the layout.
        let layout = word(W_LAYOUT);
        if layout != LAYOUT {
            return Err(RingError::Layout {
                what: "this ring",
                found: layout,
                reads: LAYOUT,
            });
        }
        if word(W_MAGIC) != MAGIC {
            return Err(RingError::Malformed("wrong magic"));
        }
        let (shape, sizes) = (word(W_SHAPE), word(W_SIZES));
        let spec = Spec {
            fields: shape as u32,
            history: (shape >> 32) as u32,
            max_payload: sizes as u32,
            expiry_ms: (sizes >> 32) as u32,
        };
        let geometry =
            Self::for_spec(&spec).map_err(|_| RingError::Malformed("shape out of range"))?;
        if geometry.history != spec.history {
            return Err(RingError::Malformed("history is not a power of two"));
        }
        if word(W_TOTAL) != geometry.total_words as u64 {
            return Err(RingError::Malformed("size does not match the shape"));
        }
        if geometry.total_words > mem.words() {
            return Err(RingError::Malformed("memory is smaller than the ring"));
        }
        Ok(geometry)
    }

    /// How many fields there are.
    #[must_use]
    pub const fn fields(&self) -> u32 {
        self.fields
    }

    /// How many samples each field keeps.
    #[must_use]
    pub const fn history(&self) -> u32 {
        self.history
    }

    /// The largest payload, in bytes.
    #[must_use]
    pub const fn max_payload(&self) -> u32 {
        self.max_payload
    }

    /// How long a value stays valid, in milliseconds. Zero for always.
    #[must_use]
    pub const fn expiry_ms(&self) -> u32 {
        self.expiry_ms
    }

    /// How many published samples back a reader can still be told the order of.
    #[must_use]
    pub const fn window(&self) -> u64 {
        self.index_slots as u64
    }

    /// The size of the ring in bytes, before rounding to pages.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.total_words * 8
    }

    /// The first word of slot `lane_slot` of `field`. Both must be in range.
    pub(crate) const fn slot_at(&self, field: usize, lane_slot: usize) -> usize {
        self.slots_at + (field * self.history as usize + lane_slot) * self.slot_words
    }
}

/// The stamp of a finished sample with sequence `seq`. Never zero, never odd.
pub(crate) const fn stamp(seq: u64) -> u64 {
    seq.wrapping_add(1) << 1
}

/// The stamp of sample `seq` while it is being written.
pub(crate) const fn busy(seq: u64) -> u64 {
    stamp(seq) | 1
}

/// Writes a fresh, empty ring of shape `spec` into `mem`, which must be zeroed.
///
/// # Errors
///
/// [`RingError::Spec`] if the shape isn't allowed or doesn't fit in `mem`.
pub fn format(mem: &impl MemMut, spec: &Spec) -> Result<Geometry, RingError> {
    let geometry = Geometry::for_spec(spec)?;
    if geometry.total_words > mem.words() {
        return Err(RingError::Spec("memory is smaller than the ring"));
    }
    let put = |index, value| mem.store(index, value, Ordering::Relaxed);
    put(W_LAYOUT, LAYOUT);
    put(
        W_SHAPE,
        u64::from(geometry.fields) | u64::from(geometry.history) << 32,
    );
    put(
        W_SIZES,
        u64::from(geometry.max_payload) | u64::from(geometry.expiry_ms) << 32,
    );
    put(W_TOTAL, geometry.total_words as u64);
    // Last, and published: nothing attaches to a half-written header.
    mem.store(W_MAGIC, MAGIC, Ordering::Release);
    Ok(geometry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Heap;

    fn spec(fields: u32, history: u32, max_payload: u32) -> Spec {
        Spec {
            fields,
            history,
            max_payload,
            expiry_ms: 0,
        }
    }

    /// These numbers are the layout. Changing one needs a new layout number.
    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(MAGIC.to_le_bytes(), *b"SHMRING1");
        assert_eq!((LAYOUT, HEADER_WORDS, SLOT_HEADER_WORDS), (2, 16, 4));
        assert_eq!(
            (W_MAGIC, W_LAYOUT, W_SHAPE, W_SIZES, W_TOTAL),
            (0, 1, 2, 3, 4)
        );
        assert_eq!((W_HEAD, W_SUBSCRIBERS, W_END), (8, 9, 10));
        let g = Geometry::for_spec(&spec(3, 5, 20)).expect("shape");
        // History 5 rounds to 8; 24 lane slots round to a 32-entry index; a slot is 4 + 3 words.
        assert_eq!((g.history, g.index_slots, g.slot_words), (8, 32, 7));
        assert_eq!((g.lane_heads_at, g.index_at, g.slots_at), (16, 19, 51));
        assert_eq!(g.total_words, 51 + 24 * 7);
        assert_eq!(g.slot_at(2, 7), 51 + 23 * 7);
        assert_eq!((stamp(0), busy(0), stamp(41)), (2, 3, 84));
    }

    #[test]
    fn a_latest_value_ring_of_small_fields_fits_one_page() {
        let g = Geometry::for_spec(&spec(8, 2, 64)).expect("shape");
        assert!(g.bytes() <= 4096, "{}", g.bytes());
    }

    #[test]
    fn shapes_out_of_range_are_refused() {
        for bad in [
            spec(0, 2, 8),
            spec(MAX_FIELDS + 1, 2, 8),
            spec(1, MAX_HISTORY + 1, 8),
            spec(1, 2, MAX_PAYLOAD + 1),
            spec(MAX_FIELDS, MAX_HISTORY, MAX_PAYLOAD),
        ] {
            assert!(
                matches!(Geometry::for_spec(&bad), Err(RingError::Spec(_))),
                "{bad:?}"
            );
        }
    }

    /// The magic this layout carried before the rename, as its 8 ASCII bytes. Rings written with
    /// it must not attach.
    const RETIRED_MAGIC: [u8; 8] = [0x4f, 0x52, 0x49, 0x4f, 0x4e, 0x52, 0x47, 0x31];

    #[test]
    fn a_ring_with_the_retired_magic_is_refused() {
        let mem = Heap::new(4096);
        format(&mem, &spec(3, 4, 100)).expect("format");
        mem.store(
            W_MAGIC,
            u64::from_le_bytes(RETIRED_MAGIC),
            Ordering::Relaxed,
        );
        assert!(matches!(
            Geometry::read(&mem),
            Err(RingError::Malformed("wrong magic"))
        ));
    }

    #[test]
    fn a_formatted_ring_reads_back_and_a_damaged_header_does_not() {
        let mem = Heap::new(4096);
        let made = format(&mem, &spec(3, 4, 100)).expect("format");
        assert_eq!(Geometry::read(&mem).expect("read"), made);

        let damage: [(usize, u64, &str); 5] = [
            (W_MAGIC, 7, "wrong magic"),
            (W_SHAPE, 0, "shape out of range"),
            (W_SHAPE, 3 | 5 << 32, "history is not a power of two"),
            (W_TOTAL, 1, "size does not match the shape"),
            (W_SIZES, u64::from(MAX_PAYLOAD) + 1, "shape out of range"),
        ];
        for (index, value, why) in damage {
            let was = mem.load(index, Ordering::Relaxed);
            mem.store(index, value, Ordering::Relaxed);
            match Geometry::read(&mem) {
                Err(RingError::Malformed(found)) => assert_eq!(found, why),
                other => panic!("{why}: {other:?}"),
            }
            mem.store(index, was, Ordering::Relaxed);
        }
        // A shape that is fine but larger than the memory it claims to be in.
        mem.store(W_SHAPE, 3 | 1024 << 32, Ordering::Relaxed);
        let big = Geometry::for_spec(&spec(3, 1024, 100)).expect("shape");
        mem.store(W_TOTAL, big.total_words as u64, Ordering::Relaxed);
        assert!(matches!(
            Geometry::read(&mem),
            Err(RingError::Malformed("memory is smaller than the ring"))
        ));
        assert!(matches!(
            Geometry::read(&Heap::new(4)),
            Err(RingError::Malformed("shorter than a header"))
        ));
    }

    /// The header of a layout 1 ring, word for word, as the layout 1 build wrote it: one field,
    /// history 2, payloads of 8 bytes, 29 words in all. Layout 1 wrote 1 in word 1 and kept word
    /// 10 reserved and zero. Nothing else of the layout changed, so the rest is zero.
    fn layout_one_ring() -> Heap {
        let mem = Heap::new(29);
        let header: [u64; 10] = [
            u64::from_le_bytes(*b"SHMRING1"), // word 0: magic
            1,                                // word 1: layout 1
            1 | 2 << 32,                      // word 2: one field, history 2
            8,                                // word 3: largest payload 8, expiry 0
            29,                               // word 4: total words
            0,
            0,
            0, // words 5 to 7: reserved
            0, // word 8: head
            0, // word 9: subscribers
        ];
        for (index, word) in header.iter().enumerate() {
            mem.store(index, *word, Ordering::Relaxed);
        }
        mem
    }

    /// The error an attach gave, which must have been a refusal.
    fn refused<T>(attached: Result<T, RingError>) -> RingError {
        match attached {
            Err(error) => error,
            Ok(_) => panic!("a layout this build does not read was accepted"),
        }
    }

    /// A layout 1 ring is refused by name, by a reader and by a writer, before its size or magic
    /// are looked at.
    #[test]
    fn a_layout_one_ring_is_refused_naming_both_layouts() {
        let mem = layout_one_ring();
        let reader = refused(crate::Reader::over(&mem));
        let writer = refused(crate::Writer::over(&mem));
        for error in [reader, writer] {
            match &error {
                RingError::Layout {
                    what: "this ring",
                    found: 1,
                    reads: 2,
                } => {}
                other => panic!("not the layout error: {other:?}"),
            }
            assert_eq!(
                error.to_string(),
                "this ring is layout 1; this build reads layout 2"
            );
        }
    }

    /// A layout 2 ring is written and read back by both ends, with a sample in it.
    #[test]
    fn a_layout_two_ring_round_trips_in_both_roles() {
        let spec = Spec {
            fields: 1,
            history: 2,
            max_payload: 8,
            expiry_ms: 0,
        };
        let mem = Heap::new(Geometry::for_spec(&spec).expect("shape").total_words);
        format(&mem, &spec).expect("format");
        assert_eq!(mem.load(W_LAYOUT, Ordering::Relaxed), 2);
        let mut writer = crate::Writer::over(&mem).expect("writer");
        writer
            .publish(
                crate::FieldId(0),
                crate::SampleTime::from_nanos(5),
                crate::Flags::NONE,
                &[7],
            )
            .expect("publish");
        let mut reader = crate::Reader::over(&mem).expect("reader");
        assert_eq!(reader.end(), 0, "a running topic has no end code");
        let latest = reader.latest(crate::FieldId(0)).expect("latest");
        assert_eq!((latest.seq, latest.payload), (0, &[7][..]));
    }
}
