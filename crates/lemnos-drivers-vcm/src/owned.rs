//! [`OwnedVcmFormat`]: a command format that owns its power sequences, for
//! descriptions loaded at run time (feature `alloc`; `serde` with `serde`).

use crate::{FormatError, MAX_VCM_WRITES, VcmChip, VcmFormat};
use alloc::vec::Vec;

/// A [`VcmFormat`] that owns its power-up and power-down messages, as a
/// description (TOML, JSON) writes it:
///
/// ```toml
/// register = 0x03     # absent: the value is the whole message
/// bytes = 2           # default 2
/// shift = 0
/// bits = 10           # default 10
/// or = 0
/// power_up = [[0x02, 0x00]]
/// power_up_us = 1000
/// power_down = [[0x02, 0x01]]
/// ```
///
/// [`Self::with_format`] or [`Self::refs`] lend it as a [`VcmFormat`]
/// without allocating.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct OwnedVcmFormat {
    /// The register the value goes to (none: the value is the whole message).
    #[cfg_attr(feature = "serde", serde(default))]
    pub register: Option<u8>,
    /// Bytes of the value (1 or 2).
    #[cfg_attr(feature = "serde", serde(default = "two"))]
    pub bytes: u8,
    /// Bits the position is shifted left.
    #[cfg_attr(feature = "serde", serde(default))]
    pub shift: u8,
    /// Bits of the position.
    #[cfg_attr(feature = "serde", serde(default = "ten"))]
    pub bits: u8,
    /// Constant bits or-ed in (mode, slew).
    #[cfg_attr(feature = "serde", serde(default))]
    pub or: u16,
    /// Raw messages that power the chip up (at most [`MAX_VCM_WRITES`]).
    #[cfg_attr(feature = "serde", serde(default))]
    pub power_up: Vec<Vec<u8>>,
    /// Microseconds to wait after power-up before the first move.
    #[cfg_attr(feature = "serde", serde(default))]
    pub power_up_us: u32,
    /// Raw messages that put the chip in standby (at most
    /// [`MAX_VCM_WRITES`]).
    #[cfg_attr(feature = "serde", serde(default))]
    pub power_down: Vec<Vec<u8>>,
}

#[cfg(feature = "serde")]
fn two() -> u8 {
    2
}

#[cfg(feature = "serde")]
fn ten() -> u8 {
    10
}

impl Default for OwnedVcmFormat {
    /// Two bytes, ten bits, no register and no power sequences.
    fn default() -> Self {
        Self {
            register: None,
            bytes: 2,
            shift: 0,
            bits: 10,
            or: 0,
            power_up: Vec::new(),
            power_up_us: 0,
            power_down: Vec::new(),
        }
    }
}

impl From<&VcmFormat<'_>> for OwnedVcmFormat {
    fn from(format: &VcmFormat<'_>) -> Self {
        Self {
            register: format.register,
            bytes: format.bytes,
            shift: format.shift,
            bits: format.bits,
            or: format.or,
            power_up: format.power_up.iter().map(|m| m.to_vec()).collect(),
            power_up_us: format.power_up_us,
            power_down: format.power_down.iter().map(|m| m.to_vec()).collect(),
        }
    }
}

impl OwnedVcmFormat {
    /// A built-in chip's format, owned (`None` for [`VcmChip::Custom`]).
    pub fn for_chip(chip: VcmChip) -> Option<Self> {
        chip.builtin_format().map(|format| Self::from(&format))
    }

    /// The format lent as a [`VcmFormat`] (the message lists borrowed
    /// through fixed arrays; writes past [`MAX_VCM_WRITES`] are dropped,
    /// which [`Self::check`] reports).
    pub fn refs(&self) -> VcmFormatRefs<'_> {
        let mut up: [&[u8]; MAX_VCM_WRITES] = [&[]; MAX_VCM_WRITES];
        let mut down: [&[u8]; MAX_VCM_WRITES] = [&[]; MAX_VCM_WRITES];
        for (slot, message) in up.iter_mut().zip(&self.power_up) {
            *slot = message;
        }
        for (slot, message) in down.iter_mut().zip(&self.power_down) {
            *slot = message;
        }
        VcmFormatRefs {
            owner: self,
            up,
            down,
        }
    }

    /// Runs `f` with the format as a [`VcmFormat`].
    pub fn with_format<R>(&self, f: impl FnOnce(&VcmFormat<'_>) -> R) -> R {
        f(&self.refs().format())
    }

    /// The highest position.
    pub fn max_position(&self) -> i32 {
        self.with_format(|f| f.max_position())
    }

    /// Checks the format, including the number of power writes.
    pub fn check(&self) -> Result<(), FormatError> {
        if self.power_up.len() > MAX_VCM_WRITES || self.power_down.len() > MAX_VCM_WRITES {
            return Err(FormatError::TooManyWrites);
        }
        self.with_format(|f| f.check())
    }
}

/// An [`OwnedVcmFormat`] lent as a [`VcmFormat`]: keep it alive as long as
/// the driver that uses [`Self::format`].
#[derive(Debug, Clone, Copy)]
pub struct VcmFormatRefs<'a> {
    owner: &'a OwnedVcmFormat,
    up: [&'a [u8]; MAX_VCM_WRITES],
    down: [&'a [u8]; MAX_VCM_WRITES],
}

impl VcmFormatRefs<'_> {
    /// The format.
    pub fn format(&self) -> VcmFormat<'_> {
        let owner = self.owner;
        VcmFormat {
            register: owner.register,
            bytes: owner.bytes,
            shift: owner.shift,
            bits: owner.bits,
            or: owner.or,
            power_up: &self.up[..owner.power_up.len().min(MAX_VCM_WRITES)],
            power_up_us: owner.power_up_us,
            power_down: &self.down[..owner.power_down.len().min(MAX_VCM_WRITES)],
        }
    }
}
