//! What a finished job reports, from the registers the vendor kernel writes back into the job's
//! register file (`jpgdec_finish`, `jpgdec_vpu730_finish`, `jpgenc_finish`).

use crate::regs::{dec, enc};

/// How a job ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The block finished the frame.
    Done,
    /// The block stopped on an error, with the status bits that say which; the kernel resets the
    /// block after it.
    Error(u32),
    /// Neither done nor an error bit (the block was stopped).
    Unfinished(u32),
}

/// A v1 decoder job's outcome, from register [`dec::INT_EN_INDEX`] as read back (the kernel puts
/// the interrupt status there).
#[must_use]
pub fn decoder_v1(status: u32) -> Outcome {
    if status & dec::V1_ERROR_MASK != 0 {
        Outcome::Error(status & dec::V1_ERROR_MASK)
    } else if status & dec::READY_STA != 0 {
        Outcome::Done
    } else {
        Outcome::Unfinished(status)
    }
}

/// A v2 decoder job's outcome, from register [`dec::V2_INT_STA_INDEX`] as read back.
#[must_use]
pub fn decoder_v2(status: u32) -> Outcome {
    if status & dec::V2_ERROR_MASK != 0 {
        Outcome::Error(status & dec::V2_ERROR_MASK)
    } else if status & dec::V2_DEC_DONE != 0 {
        Outcome::Done
    } else {
        Outcome::Unfinished(status)
    }
}

/// An encoder job's outcome, from register [`enc::INT_STATUS_INDEX`] as read back.
#[must_use]
pub fn encoder(status: u32) -> Outcome {
    if status & enc::ERROR_MASK != 0 {
        Outcome::Error(status & enc::ERROR_MASK)
    } else if status & enc::ENC_DONE != 0 {
        Outcome::Done
    } else {
        Outcome::Unfinished(status)
    }
}

/// The bytes of stream a decoder job consumed, from register [`dec::STREAM_RLC_BASE_INDEX`] as
/// read back: the kernel stores the difference of the block's stream pointer and the start,
/// shifted left by 10, so it can be negative and its top 10 bits are lost.
#[must_use]
pub fn decoded_bytes(stream_register: u32) -> i32 {
    stream_register.cast_signed() >> 10
}

/// The picture size a decoder's register [`dec::PIC_INFO_INDEX`] holds.
#[must_use]
pub fn decoder_picture(pic_info: u32) -> (u32, u32) {
    ((pic_info & 0xffff) + 1, ((pic_info >> 16) & 0xffff) + 1)
}

/// The picture size an encoder's register [`enc::ENC_RSL_INDEX`] holds, in pixels.
#[must_use]
pub fn encoder_picture(rsl: u32) -> (u32, u32) {
    (((rsl & 0x1fff) + 1) << 3, (((rsl >> 16) & 0x1fff) + 1) << 3)
}

/// The encoder's [`enc::ENC_RSL_INDEX`] value for a picture, or `None` unless both sides are
/// multiples of 8 from 8 to 65536.
#[must_use]
pub fn encoder_resolution(width: u32, height: u32) -> Option<u32> {
    let field =
        |n: u32| (n.is_multiple_of(8) && (8..=0x2000 << 3).contains(&n)).then(|| (n >> 3) - 1);
    Some(field(width)? | field(height)? << 16)
}
