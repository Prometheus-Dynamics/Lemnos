//! The QP map: one byte per macroblock (`uMbQpMap`), four to a CTU word (`uCtuQpMap`). The
//! header's bit fields are, least significant first: `qp` (6 bits), `skip` (bit 6), `en` (bit 7).
//! A CTU word holds its four macroblocks' bytes in order, the first in the lowest byte.

use super::Error;

/// The largest QP a map byte holds: six bits.
pub const MAX_QP: u8 = 63;
/// The bytes of one CTU word.
pub const CTU_BYTES: usize = 4;

const QP_MASK: u8 = 0x3f;
const SKIP_BIT: u8 = 1 << 6;
const EN_BIT: u8 = 1 << 7;

/// One macroblock's entry in the QP map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MbQp {
    /// The quantiser, `0` to [`MAX_QP`].
    pub qp: u8,
    /// Whether the macroblock is skipped.
    pub skip: bool,
    /// Whether the entry applies (`en`).
    pub enable: bool,
}

impl MbQp {
    /// The map byte for this entry. [`Error::QpOutOfRange`] when `qp` does not fit six bits.
    pub const fn to_byte(self) -> Result<u8, Error> {
        if self.qp > MAX_QP {
            return Err(Error::QpOutOfRange(self.qp));
        }
        let skip = if self.skip { SKIP_BIT } else { 0 };
        let enable = if self.enable { EN_BIT } else { 0 };
        Ok(self.qp | skip | enable)
    }

    /// The entry a map byte holds. Every byte is a valid entry.
    pub const fn from_byte(byte: u8) -> Self {
        Self {
            qp: byte & QP_MASK,
            skip: byte & SKIP_BIT != 0,
            enable: byte & EN_BIT != 0,
        }
    }
}

/// The CTU word for four macroblocks, in order. The word's little-endian bytes are the four
/// entries' bytes, so the first macroblock is the lowest byte.
pub const fn pack_ctu(mbs: &[MbQp; CTU_BYTES]) -> Result<u32, Error> {
    let mut bytes = [0u8; CTU_BYTES];
    let mut i = 0;
    while i < CTU_BYTES {
        bytes[i] = match mbs[i].to_byte() {
            Ok(byte) => byte,
            Err(e) => return Err(e),
        };
        i += 1;
    }
    Ok(u32::from_le_bytes(bytes))
}

/// The four entries a CTU word holds, in order.
pub const fn unpack_ctu(word: u32) -> [MbQp; CTU_BYTES] {
    let b = word.to_le_bytes();
    [
        MbQp::from_byte(b[0]),
        MbQp::from_byte(b[1]),
        MbQp::from_byte(b[2]),
        MbQp::from_byte(b[3]),
    ]
}

/// Writes the map for `ctus` into `out`, four bytes a CTU, in order, and returns the bytes written.
/// Nothing is written when a value is refused or `out` is too short.
pub fn pack_map(ctus: &[[MbQp; CTU_BYTES]], out: &mut [u8]) -> Result<usize, Error> {
    let need = ctus.len() * CTU_BYTES;
    if out.len() < need {
        return Err(Error::TooShort {
            need,
            have: out.len(),
        });
    }
    // Every value is checked before the first byte goes out.
    for ctu in ctus {
        pack_ctu(ctu)?;
    }
    for (index, ctu) in ctus.iter().enumerate() {
        for (k, mb) in ctu.iter().enumerate() {
            out[index * CTU_BYTES + k] = mb.to_byte()?;
        }
    }
    Ok(need)
}
