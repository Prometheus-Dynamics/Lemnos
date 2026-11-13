//! The configuration layout: a list of register writes, checked whole before any reaches the
//! block.
//!
//! A config buffer in [`LAYOUT_WRITES`] holds, little-endian:
//!
//! ```text
//! offset 0          u32 count             writes that follow, at most MAX_WRITES
//! offset 4 + 8 * n  u32 addr, u32 value   write n: a register offset in the window, its value
//! ```
//!
//! Bytes past the last write are ignored. Writes are carried out in order, so a register may
//! appear more than once (the lens shading table's data registers, which advance their own table
//! address, take one write per sample pair). What the values mean is the caller's: Lemnos holds
//! no tuning and runs no 3A.
//!
//! [`check`] refuses the whole list when any write names a register outside
//! [`access::WRITABLE`](lemnos_driver_k230isp_sys::access::WRITABLE) or sets a bit outside that
//! register's mask, or when the count is past [`MAX_WRITES`] or past the bytes given. No write
//! of a refused list reaches the block. Every number here is read from the driver's copy of the
//! buffer, never from the caller's memory.

use lemnos_driver_k230isp_sys::access::allowed;

/// `camera.isp/1` config layout: a list of register writes.
pub const LAYOUT_WRITES: u32 = 0x0001;
/// Writes one configuration holds at most.
pub const MAX_WRITES: usize = 1024;
/// Bytes of a configuration in [`LAYOUT_WRITES`]: the count and [`MAX_WRITES`] pairs.
pub const CONFIG_BYTES: usize = 4 + 8 * MAX_WRITES;

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A list of writes that passed [`check`]: each `(addr, value)` may be written as it stands.
#[derive(Debug, Clone)]
pub struct Writes<'a> {
    pairs: core::slice::Iter<'a, [u8; 8]>,
}

impl Iterator for Writes<'_> {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<(u32, u32)> {
        let [a0, a1, a2, a3, v0, v1, v2, v3] = *self.pairs.next()?;
        Some((
            u32::from_le_bytes([a0, a1, a2, a3]),
            u32::from_le_bytes([v0, v1, v2, v3]),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.pairs.size_hint()
    }
}

impl ExactSizeIterator for Writes<'_> {}

/// The writes `config` holds, if every one may be written; `None` refuses the whole list.
#[must_use]
pub fn check(config: &[u8]) -> Option<Writes<'_>> {
    let count = usize::try_from(word(config, 0)?).ok()?;
    if count > MAX_WRITES {
        return None;
    }
    // At most 8 * 1024 + 4: no overflow.
    let pairs = config.get(4..4 + 8 * count)?;
    let writes = Writes {
        pairs: pairs.as_chunks::<8>().0.iter(),
    };
    for (addr, value) in writes.clone() {
        if !allowed(addr, value) {
            return None;
        }
    }
    Some(writes)
}
