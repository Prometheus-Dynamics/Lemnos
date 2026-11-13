//! The tuning blocks whose register encodings the vendor kernel's headers define, written into a
//! load image through [`Image`].
//!
//! What is here is only what the kernel's headers define as bit fields, and only what a caller
//! may set without naming memory or how far the block reads or writes:
//!
//! - [`expand`]: each channel's black level enable, its inverse, and its input and output bit
//!   depths (`ISP_CHn_EXPAND_CFG0`). The black level's offset is not in the headers, so the
//!   black level's value is not here.
//! - [`module`](crate::layout::module) enables, written as one word ([`Image::set_modules`]);
//!   the bits the headers leave reserved are refused.
//! - [`temporal_record`]: the temporal denoise's record enable (`ISP_D3D_CFG0`).
//!
//! What is not here, and why, is in the crate's documentation and `docs/drivers.md`: no
//! white balance, colour matrix, gamma, tone, lens shading, spatial denoise, sharpen, or
//! statistics encodings. The library source that might define them (`radxa/allwinner-isp6xx`)
//! computes their register words in a prebuilt binary that is not read, so they are not in it.
//!
//! Every value is written little-endian, the way the block reads its register image on the
//! A733-class chip. Each field's range is the width the header gives it; the headers state no
//! narrower valid range, so none is checked here. Nothing here writes an address word, an extent
//! word ([`layout::EXTENT_WORDS`]) or a word the kernel owns, because no setter names one.
//!
//! A setter changes only the bits of its own fields and keeps the rest of the word, so an image
//! that starts from the library's default words (which set bits the headers name for other
//! fields, as the crate's `library_tests` vectors record) keeps them.
//! [`Image::set_modules`] is the one whole word: every bit of it is a module enable.
//!
//! Source: the same kernel headers as [`layout`]
//! (`drivers/vin/vin-isp/isp600/isp600_reg.h`, GPL-2.0), from `radxa/allwinner-bsp` at
//! `2045a3ca2a01f088c0314dc924bda59d154e363e`.

use crate::layout::{self, Checked, LOAD_BYTES, LOAD_REGISTERS_OFFSET, Refusal, module, reg};

/// A value that does not fit the bits its field has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange {
    /// The field's name in the vendor header.
    pub field: &'static str,
    pub value: u32,
    /// The largest value the field holds.
    pub max: u32,
}

/// Why a setter refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A channel number past the block's channels.
    Channel(usize),
    /// A field does not fit its bits.
    Range(OutOfRange),
    /// A module enable bit the headers leave reserved, or a value with one set.
    Reserved(u32),
}

/// `ISP_CHn_EXPAND_CFG0`: each channel's black level and bit depths.
pub mod expand {
    /// Channels that have the register.
    pub const CHANNELS: usize = 3;
    /// Each channel's register, from the register image's loaded registers.
    pub const REGISTERS: [usize; CHANNELS] = [
        crate::layout::reg::CH0_EXPAND_CFG0,
        crate::layout::reg::CH1_EXPAND_CFG0,
        crate::layout::reg::CH2_EXPAND_CFG0,
    ];
    /// `blc_en`, bit 0.
    pub const BLC: u32 = 1 << 0;
    /// `inv_blc_en`, bit 1.
    pub const INVERSE_BLC: u32 = 1 << 1;
    /// `input_bit`: five bits from bit 4.
    pub const INPUT_BIT_SHIFT: u32 = 4;
    /// `output_bit`: five bits from bit 16.
    pub const OUTPUT_BIT_SHIFT: u32 = 16;
    /// The largest value a five-bit bit-depth field holds.
    pub const BITS_MAX: u8 = 0x1f;
    /// The bits this module's setter owns. Every other bit of the word is kept.
    pub const MASK: u32 = BLC
        | INVERSE_BLC
        | ((BITS_MAX as u32) << INPUT_BIT_SHIFT)
        | ((BITS_MAX as u32) << OUTPUT_BIT_SHIFT);
}

/// One channel's black level and bit depths, as [`expand`] lays them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelExpand {
    pub black_level: bool,
    pub inverse_black_level: bool,
    pub input_bits: u8,
    pub output_bits: u8,
}

impl ChannelExpand {
    /// The register's value. Reserved bits are zero.
    pub fn encode(self) -> Result<u32, OutOfRange> {
        if self.input_bits > expand::BITS_MAX {
            return Err(OutOfRange {
                field: "input_bit",
                value: u32::from(self.input_bits),
                max: u32::from(expand::BITS_MAX),
            });
        }
        if self.output_bits > expand::BITS_MAX {
            return Err(OutOfRange {
                field: "output_bit",
                value: u32::from(self.output_bits),
                max: u32::from(expand::BITS_MAX),
            });
        }
        let mut word = 0;
        if self.black_level {
            word |= expand::BLC;
        }
        if self.inverse_black_level {
            word |= expand::INVERSE_BLC;
        }
        word |= u32::from(self.input_bits) << expand::INPUT_BIT_SHIFT;
        word |= u32::from(self.output_bits) << expand::OUTPUT_BIT_SHIFT;
        Ok(word)
    }
}

/// The module enable bits the headers define in `MODULE_BYPASS1`: bits 0 to 18 and 24 to 29.
/// Bits 19 to 23 and 30 to 31 are reserved.
pub const MODULE_ENABLE_BITS: u32 = 0x0007_ffff | 0x3f00_0000;

/// `ISP_D3D_CFG0`: `d3d_rec_en`, the temporal denoise's record enable, bit 0.
pub mod temporal {
    pub const RECORD: u32 = 1 << 0;
}

/// A load image being filled in. Every setter writes one whole register word; the image's
/// address and extent words are never written by any setter.
pub struct Image<'a>(&'a mut [u8; LOAD_BYTES]);

impl<'a> Image<'a> {
    /// Fills `image` in place. Words no setter writes keep their value.
    #[must_use]
    pub fn new(image: &'a mut [u8; LOAD_BYTES]) -> Self {
        Self(image)
    }

    /// Writes the black level and bit depths of `channel`, keeping the word's other bits.
    pub fn set_channel_expand(
        &mut self,
        channel: usize,
        value: ChannelExpand,
    ) -> Result<(), Error> {
        let Some(&register) = expand::REGISTERS.get(channel) else {
            return Err(Error::Channel(channel));
        };
        let word = value.encode().map_err(Error::Range)?;
        let kept = self.get(register) & !expand::MASK;
        self.put(register, kept | word);
        Ok(())
    }

    /// Writes the module enables, the bits of [`module`] or'd together.
    pub fn set_modules(&mut self, enables: u32) -> Result<(), Error> {
        let reserved = enables & !MODULE_ENABLE_BITS;
        if reserved != 0 {
            return Err(Error::Reserved(reserved));
        }
        self.put(reg::MODULE_BYPASS1, enables);
        Ok(())
    }

    /// Sets or clears the temporal denoise's record enable, keeping the word's other bits.
    pub fn set_temporal_record(&mut self, on: bool) {
        let kept = self.get(reg::D3D_CFG0) & !temporal::RECORD;
        let bit = if on { temporal::RECORD } else { 0 };
        self.put(reg::D3D_CFG0, kept | bit);
    }

    /// The image, if it may go to the kernel. See [`layout::check_load`].
    pub fn check(&self) -> Result<Checked<'_>, Refusal> {
        layout::check_load(&self.0[..])
    }

    fn get(&self, register: usize) -> u32 {
        let at = LOAD_REGISTERS_OFFSET + register;
        u32::from_le_bytes([self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]])
    }

    fn put(&mut self, register: usize, word: u32) {
        let at = LOAD_REGISTERS_OFFSET + register;
        self.0[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
}

const _: () = {
    assert!(module::BLC == 1 << 6);
    assert!(MODULE_ENABLE_BITS & module::HIST1 == module::HIST1);
    assert!(MODULE_ENABLE_BITS & module::CEM == module::CEM);
    assert!(MODULE_ENABLE_BITS & module::AE == module::AE);
    assert!(MODULE_ENABLE_BITS & (1 << 19) == 0);
    assert!(MODULE_ENABLE_BITS & (1 << 30) == 0);
};
