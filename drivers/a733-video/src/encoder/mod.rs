//! The encoder's records and values as the vendor header defines them: the per-macroblock QP map
//! and the per-CTU info word, and the codec, rate-control, pixel, H.264 profile and level, buffer
//! flag and slice values a caller names.
//!
//! Source: `mashiqi/A733-Cedarc` at `2e07a43cff96903e962e6ff2f83da6def6cb3d9c`, the header files
//! under `vendor/headers/`:
//!
//! - `vencoder_platform_v2.h`: `uMbQpMap` (one byte), `uCtuQpMap` (four of them, one word) and
//!   `sCtuQpInfo` (one word). Their bit fields are compiled with gcc and clang on x86_64 (the
//!   bit order is the one the declarations give, least significant first, and both agree); see
//!   the tests.
//! - `vencoder.h`: `VENC_CODEC_TYPE`, `VENC_PIXEL_FMT`, `VENC_RC_MODE`, `VENC_H264PROFILETYPE`,
//!   `VENC_H264LEVELTYPE`, `VENC_PICTURE_SLICE_TYPE` and the `VENC_BUFFERFLAG_*` macros.
//!
//! # What the headers do not define
//!
//! The headers are the library's programming interface. They give no register offset and no bit
//! position for any encoder register, and no job structure: the library writes the engine's
//! registers itself, from its own binary. So nothing here is a register layout, and no job is
//! built from it. The facts a job would still need, and that no header or open kernel source
//! gives, are:
//!
//! - the register each record's address is written to (the QP map, and the CTU info buffer),
//!   and whether the engine reads the QP map or writes the CTU info, or both;
//! - which macroblock of the picture each byte of a CTU word covers (the header gives `mb[4]` and
//!   no order), and the map's stride;
//! - the encodings of the H.264 parameters a caller passes (`VencH264Param`, `VencBaseConfig`,
//!   the rate-control fields) in the engine's registers;
//! - the bitstream's header and trailer, and how the engine reports a picture's length;
//! - the H.265 and JPEG encoders' registers. The header defines their parameters (`VencH265*`,
//!   `JpegEncInfo`) and the codec ids, and nothing about their registers.
//!
//! The records here are the buffer contents a job would carry if the engine's address for them
//! were known. Nothing in this crate writes them to an engine.

pub mod ctu_info;
pub mod qp_map;
pub mod values;

#[cfg(test)]
mod tests;

/// A value a record or builder refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A QP above [`qp_map::MAX_QP`]: the byte has six bits for it.
    QpOutOfRange(u8),
    /// A bit the header leaves unnamed (`sCtuQpInfo.r0`) set to something other than zero.
    Reserved(u8),
    /// The output buffer is shorter than the records need.
    TooShort {
        /// The bytes the records fill.
        need: usize,
        /// The bytes the buffer holds.
        have: usize,
    },
}
