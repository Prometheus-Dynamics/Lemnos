//! The blob writer: a PC side calls [`write`] with the target's layout, the pinned code words of
//! [`crate::pinned`] and the run's fields, and gets the bytes to write at the thunk address. Nothing here
//! allocates: the caller owns the output buffer, which is the blob's length.

use crate::layout::Layout;
use crate::pinned;

/// One swap: `size` bytes at `buf1` (a live FEL stack region) exchange with `size` bytes at `buf2` (a free
/// backup area). The thunk swaps the two before the SPL runs and again after it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swap {
    pub buf1: u32,
    pub buf2: u32,
    pub size: u32,
}

/// The watchdog the thunk arms first and disarms before it returns to the ROM. The addresses and values are
/// the SoC's, and come from the layout the PC side holds; this crate does not know them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watchdog {
    /// The watchdog's register, the one that takes the arm value and the disarm value.
    pub address: u32,
    /// The value that arms it. Zero is refused: it would arm nothing.
    pub arm: u32,
    /// The value that disarms it. Zero is allowed.
    pub disarm: u32,
}

/// Everything the PC side fills in for one run.
#[derive(Clone, Copy, Debug)]
pub struct Fields<'a> {
    /// The SPL's entry address. Zero is refused, and so is an address that is not a whole word.
    pub entry: u32,
    pub watchdog: Watchdog,
    pub swaps: &'a [Swap],
}

/// Why a blob can't be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobError {
    /// The output buffer is not the blob's length.
    Output { need: usize, got: usize },
    /// The SPL entry is zero or not a whole word.
    Entry,
    /// The watchdog address is zero or not a whole word.
    WatchdogAddress,
    /// The watchdog's arm value is zero.
    WatchdogArm,
    /// More swaps than the table holds before its terminator.
    TooManySwaps { count: usize, max: usize },
    /// A swap's size is zero. A zero size would end the table early.
    EmptySwap { index: usize },
    /// A swap's size or buffer is not a whole number of words.
    UnalignedSwap { index: usize },
}

impl core::fmt::Display for BlobError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Output { need, got } => {
                write!(f, "the blob needs {need} bytes, the buffer has {got}")
            }
            Self::Entry => f.write_str("the SPL entry is zero or not a whole word"),
            Self::WatchdogAddress => {
                f.write_str("the watchdog address is zero or not a whole word")
            }
            Self::WatchdogArm => f.write_str("the watchdog's arm value is zero"),
            Self::TooManySwaps { count, max } => {
                write!(f, "{count} swap buffers; the table holds {max}")
            }
            Self::EmptySwap { index } => write!(f, "swap {index} has size zero"),
            Self::UnalignedSwap { index } => {
                write!(f, "swap {index} is not a whole number of words")
            }
        }
    }
}

/// The pinned code words of a target, as bytes: what the thunk's code and zeroed data are before the fields
/// are written. `layout.blob_len` is their length.
fn code(layout: &Layout) -> Option<&'static [u32]> {
    match layout.triple {
        "armv7a-none-eabi" => Some(pinned::ARMV7_WORDS),
        "aarch64-unknown-none" => Some(pinned::AARCH64_WORDS),
        _ => None,
    }
}

fn put(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes the blob for `layout` into `out`, which must be exactly `layout.blob_len` bytes. Returns the blob's
/// length. Every check runs before the first byte is written, so a refusal leaves `out` untouched.
pub fn write(layout: &Layout, fields: &Fields<'_>, out: &mut [u8]) -> Result<usize, BlobError> {
    let words = code(layout).unwrap_or(&[]);
    if out.len() != layout.blob_len || words.len() * 4 != layout.blob_len {
        return Err(BlobError::Output {
            need: layout.blob_len,
            got: out.len(),
        });
    }
    if fields.entry == 0 || !fields.entry.is_multiple_of(4) {
        return Err(BlobError::Entry);
    }
    let wdt = fields.watchdog;
    if wdt.address == 0 || !wdt.address.is_multiple_of(4) {
        return Err(BlobError::WatchdogAddress);
    }
    if wdt.arm == 0 {
        return Err(BlobError::WatchdogArm);
    }
    let max = layout.table_entries - 1;
    if fields.swaps.len() > max {
        return Err(BlobError::TooManySwaps {
            count: fields.swaps.len(),
            max,
        });
    }
    for (index, swap) in fields.swaps.iter().enumerate() {
        if swap.size == 0 {
            return Err(BlobError::EmptySwap { index });
        }
        if !swap.size.is_multiple_of(4)
            || !swap.buf1.is_multiple_of(4)
            || !swap.buf2.is_multiple_of(4)
        {
            return Err(BlobError::UnalignedSwap { index });
        }
    }

    for (i, word) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    // The table: the swaps, then zeros, so the terminator is always there.
    for entry in 0..layout.table_entries {
        let at = layout.table_offset + entry * 12;
        let (buf1, buf2, size) = match fields.swaps.get(entry) {
            Some(swap) => (swap.buf1, swap.buf2, swap.size),
            None => (0, 0, 0),
        };
        put(out, at, buf1);
        put(out, at + 4, buf2);
        put(out, at + 8, size);
    }
    put(out, layout.entry_offset, fields.entry);
    put(out, layout.flag_offset, 0);
    put(out, layout.wdt_addr_offset, wdt.address);
    put(out, layout.wdt_arm_offset, wdt.arm);
    put(out, layout.wdt_disarm_offset, wdt.disarm);
    for at in (layout.save_offset..layout.blob_len).step_by(4) {
        put(out, at, 0);
    }
    Ok(layout.blob_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash;
    use crate::layout::{AARCH64, ARMV7};

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    fn fields(swaps: &[Swap]) -> Fields<'_> {
        Fields {
            entry: 0x2000_1000,
            watchdog: Watchdog {
                address: 0x2000_2000,
                arm: 0x5a5a_0001,
                disarm: 0x0000_0002,
            },
            swaps,
        }
    }

    fn one_swap() -> [Swap; 1] {
        [Swap {
            buf1: 0x2000_3000,
            buf2: 0x2000_3100,
            size: 0x100,
        }]
    }

    #[test]
    fn the_blob_hashes_are_pinned() {
        // The FNV-1a walk over the pinned bytes, with the length first. Checked against an independent walk in
        // Python (the same fold, the same vectors) when the words were pinned.
        assert_eq!(
            hash::blob(&bytes(pinned::ARMV7_WORDS)),
            9_449_696_536_236_141_269
        );
        assert_eq!(
            hash::blob(&bytes(pinned::AARCH64_WORDS)),
            9_620_355_475_417_539_984
        );
    }

    #[test]
    fn the_pinned_code_is_the_layouts_length() {
        assert_eq!(bytes(pinned::ARMV7_WORDS).len(), ARMV7.blob_len);
        assert_eq!(bytes(pinned::AARCH64_WORDS).len(), AARCH64.blob_len);
    }

    #[test]
    fn the_fields_land_at_the_layout_offsets() {
        let swaps = one_swap();
        for layout in [ARMV7, AARCH64] {
            let mut out = vec![0u8; layout.blob_len];
            let written = write(&layout, &fields(&swaps), &mut out).unwrap();
            assert_eq!(written, layout.blob_len);
            let word = |at: usize| u32::from_le_bytes(out[at..at + 4].try_into().unwrap());
            assert_eq!(word(layout.table_offset), 0x2000_3000);
            assert_eq!(word(layout.table_offset + 4), 0x2000_3100);
            assert_eq!(word(layout.table_offset + 8), 0x100);
            assert_eq!(word(layout.entry_offset), 0x2000_1000);
            assert_eq!(word(layout.flag_offset), 0);
            assert_eq!(word(layout.wdt_addr_offset), 0x2000_2000);
            assert_eq!(word(layout.wdt_arm_offset), 0x5a5a_0001);
            assert_eq!(word(layout.wdt_disarm_offset), 0x0000_0002);
            assert!(out[layout.save_offset..].iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn the_code_bytes_are_the_pinned_words() {
        let swaps = one_swap();
        let mut out = vec![0u8; ARMV7.blob_len];
        write(&ARMV7, &fields(&swaps), &mut out).unwrap();
        assert_eq!(
            &out[..ARMV7.table_offset],
            &bytes(pinned::ARMV7_WORDS)[..ARMV7.table_offset]
        );
    }

    #[test]
    fn the_table_always_ends_with_a_terminator() {
        // Seven swaps fill the table's first seven entries; the eighth stays zero, and so does every later word.
        let swaps = [Swap {
            buf1: 0x2000_3000,
            buf2: 0x2000_3100,
            size: 0x100,
        }; 7];
        let mut out = vec![0u8; ARMV7.blob_len];
        write(&ARMV7, &fields(&swaps), &mut out).unwrap();
        let eighth = ARMV7.table_offset + 7 * 12;
        assert!(out[eighth..eighth + 12].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn a_refusal_writes_nothing() {
        let swaps = one_swap();
        let zero = [Swap {
            buf1: 0x2000_3000,
            buf2: 0x2000_3100,
            size: 0,
        }];
        let odd = [Swap {
            buf1: 0x2000_3002,
            buf2: 0x2000_3100,
            size: 0x100,
        }];
        let many = [swaps[0]; 8];
        let mut bad_entry = fields(&swaps);
        bad_entry.entry = 0;
        let mut bad_wdt = fields(&swaps);
        bad_wdt.watchdog.arm = 0;
        let cases = [
            ("zero entry", bad_entry),
            ("zero watchdog arm", bad_wdt),
            ("empty swap", fields(&zero)),
            ("unaligned swap", fields(&odd)),
            ("eight swaps", fields(&many)),
        ];
        for (what, case) in cases {
            let mut out = vec![0xAAu8; ARMV7.blob_len];
            assert!(
                write(&ARMV7, &case, &mut out).is_err(),
                "{what} was accepted"
            );
            assert!(
                out.iter().all(|byte| *byte == 0xAA),
                "{what} wrote before it refused"
            );
        }
    }

    #[test]
    fn a_buffer_of_the_wrong_length_is_refused() {
        let swaps = one_swap();
        let mut out = vec![0u8; ARMV7.blob_len - 4];
        assert_eq!(
            write(&ARMV7, &fields(&swaps), &mut out),
            Err(BlobError::Output {
                need: ARMV7.blob_len,
                got: ARMV7.blob_len - 4
            })
        );
    }
}
