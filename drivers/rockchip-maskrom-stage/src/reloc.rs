//! Self-relocation of the 472 image. The image is linked as a static PIE: every absolute address in
//! its data is an R_AARCH64_RELATIVE entry in `.rela.dyn`, and the prologue applies them against the
//! image's runtime base before any Rust code runs. The file's bytes do not depend on the base; only the
//! relocated slots do, and they take their value here.
//!
//! The formula is the AArch64 ELF ABI's for R_AARCH64_RELATIVE (ELF for the Arm 64-bit Architecture,
//! section 5.7, "Relocations"): the slot receives B + A, where B is the load base and A the addend.
//! The entry layout is the ELF64 Elf64_Rela: r_offset (8), r_info (8), r_addend (8), 24 bytes.

/// R_AARCH64_RELATIVE (ELF for the Arm 64-bit Architecture, table of relocation codes).
pub const R_AARCH64_RELATIVE: u32 = 1027;

/// One Elf64_Rela entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rela {
    pub offset: u64,
    pub info: u64,
    pub addend: i64,
}

impl Rela {
    /// The relocation type: the low 32 bits of r_info.
    pub fn kind(&self) -> u32 {
        (self.info & 0xffff_ffff) as u32
    }
}

/// Why a table could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocError {
    /// An entry is not R_AARCH64_RELATIVE: the image needs a symbol, which nothing here resolves.
    NotRelative { kind: u32 },
    /// An entry's slot lies outside the image.
    OutOfImage { offset: u64 },
}

/// Parses one 24-byte `.rela.dyn` entry (little-endian).
pub fn parse_entry(entry: &[u8; 24]) -> Rela {
    let word = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&entry[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    Rela {
        offset: word(0),
        info: word(8),
        addend: word(16) as i64,
    }
}

/// Applies `table` to `image`, which is loaded at `base`: each slot receives `base + addend`.
pub fn apply_relative(image: &mut [u8], base: u64, table: &[Rela]) -> Result<(), RelocError> {
    for entry in table {
        if entry.kind() != R_AARCH64_RELATIVE {
            return Err(RelocError::NotRelative { kind: entry.kind() });
        }
        let at = usize::try_from(entry.offset).map_err(|_| RelocError::OutOfImage {
            offset: entry.offset,
        })?;
        let end = at.checked_add(8).ok_or(RelocError::OutOfImage {
            offset: entry.offset,
        })?;
        if end > image.len() {
            return Err(RelocError::OutOfImage {
                offset: entry.offset,
            });
        }
        let value = base.wrapping_add(entry.addend as u64);
        image[at..end].copy_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: u64, addend: i64) -> Rela {
        Rela {
            offset,
            info: u64::from(R_AARCH64_RELATIVE),
            addend,
        }
    }

    #[test]
    fn a_relative_slot_holds_base_plus_addend_at_each_base() {
        let table = [entry(8, 0x10), entry(16, -4)];
        let mut low = vec![0u8; 32];
        apply_relative(&mut low, 0x1000, &table).unwrap();
        assert_eq!(u64::from_le_bytes(low[8..16].try_into().unwrap()), 0x1010);
        assert_eq!(
            u64::from_le_bytes(low[16..24].try_into().unwrap()),
            0x1000 - 4
        );

        let mut high = vec![0u8; 32];
        apply_relative(&mut high, 0x4000_0000, &table).unwrap();
        assert_eq!(
            u64::from_le_bytes(high[8..16].try_into().unwrap()),
            0x4000_0010
        );
        assert_eq!(
            u64::from_le_bytes(high[16..24].try_into().unwrap()),
            0x4000_0000 - 4
        );
    }

    #[test]
    fn the_same_file_bytes_relocate_to_different_values_only_in_the_slots() {
        let table = [entry(0, 0x20)];
        let file = vec![0xAAu8; 16];
        let (mut a, mut b) = (file.clone(), file.clone());
        apply_relative(&mut a, 0x0, &table).unwrap();
        apply_relative(&mut b, 0x8000, &table).unwrap();
        assert_eq!(a[8..], file[8..], "bytes outside the slot are the file's");
        assert_eq!(b[8..], file[8..]);
        assert_ne!(a[..8], b[..8], "the slot depends on the base");
    }

    #[test]
    fn a_symbol_relocation_is_refused() {
        let table = [Rela {
            offset: 0,
            info: (1u64 << 32) | 257,
            addend: 0,
        }];
        let mut image = vec![0u8; 8];
        assert_eq!(
            apply_relative(&mut image, 0, &table),
            Err(RelocError::NotRelative { kind: 257 })
        );
    }

    #[test]
    fn a_slot_past_the_end_is_refused() {
        let mut image = vec![0u8; 8];
        assert_eq!(
            apply_relative(&mut image, 0, &[entry(4, 0)]),
            Err(RelocError::OutOfImage { offset: 4 })
        );
    }

    #[test]
    fn an_entry_parses_from_its_24_bytes() {
        let mut bytes = [0u8; 24];
        bytes[0..8].copy_from_slice(&8u64.to_le_bytes());
        bytes[8..16].copy_from_slice(&u64::from(R_AARCH64_RELATIVE).to_le_bytes());
        bytes[16..24].copy_from_slice(&(-16i64).to_le_bytes());
        assert_eq!(parse_entry(&bytes), entry(8, -16));
    }
}
