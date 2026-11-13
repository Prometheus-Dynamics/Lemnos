//! The per-CTU info word (`sCtuQpInfo`): three flags, five bits the header leaves unnamed (`r0`),
//! the MAD value in bits 8 to 15 and the SSE value in bits 16 to 31. Least significant bit first.
//!
//! The header does not say whether the engine reads these words or writes them, so this layout
//! is only what the word holds.

use super::Error;

const MOVE: u32 = 1 << 0;
const EXPAND: u32 = 1 << 1;
const EN_MAD: u32 = 1 << 2;
const RESERVED_SHIFT: u32 = 3;
const RESERVED_MASK: u32 = 0x1f;
const MAD_SHIFT: u32 = 8;
const MSE_SHIFT: u32 = 16;

/// One CTU's info.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CtuInfo {
    /// `is_move`.
    pub is_move: bool,
    /// `is_expand`.
    pub is_expand: bool,
    /// `en_mad`.
    pub en_mad: bool,
    /// `r0`, the unnamed five bits. Only zero packs.
    pub reserved: u8,
    /// `mad_val`.
    pub mad: u8,
    /// `mse_val`.
    pub mse: u16,
}

impl CtuInfo {
    /// The word for this info. [`Error::Reserved`] when `reserved` is not zero.
    pub const fn to_word(self) -> Result<u32, Error> {
        if self.reserved != 0 {
            return Err(Error::Reserved(self.reserved));
        }
        let mut word = (self.mad as u32) << MAD_SHIFT | (self.mse as u32) << MSE_SHIFT;
        if self.is_move {
            word |= MOVE;
        }
        if self.is_expand {
            word |= EXPAND;
        }
        if self.en_mad {
            word |= EN_MAD;
        }
        Ok(word)
    }

    /// The info a word holds. Every word is a valid info.
    pub const fn from_word(word: u32) -> Self {
        Self {
            is_move: word & MOVE != 0,
            is_expand: word & EXPAND != 0,
            en_mad: word & EN_MAD != 0,
            reserved: ((word >> RESERVED_SHIFT) & RESERVED_MASK) as u8,
            mad: (word >> MAD_SHIFT) as u8,
            mse: (word >> MSE_SHIFT) as u16,
        }
    }
}
