//! The VEPU580 H.265 BASE (kernel class 0): the control and link registers, reg0 to reg22 (reg_ctl).
//!
//! Words 0 to 22 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg0000` at 0x0000: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0000;

/// Register `reg0004` at 0x0010: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0004;

/// Register `reg0005` at 0x0014: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0005;

/// Register `reg0008` at 0x0020: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0008;

/// Register `reg0009` at 0x0024: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0009;

/// Register `reg0010` at 0x0028: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0010;

/// Register `reg0011` at 0x002c: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0011;

/// Register `reg0012` at 0x0030: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0012;

/// Register `reg0013` at 0x0034: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0013;

/// Register `reg0014` at 0x0038: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0014;

/// Register `reg0015` at 0x003c: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0015;

/// Register `reg0016` at 0x0040: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0016;

/// Register `reg0017` at 0x0044: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0017;

/// Register `reg0018` at 0x0048: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0018;

/// Register `reg0019` at 0x004c: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0019;

/// Register `reg0020` at 0x0050: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0020;

/// Register `reg0021` at 0x0054: the same register as in the H.264 layout.
pub use crate::vepu580_h264::base::reg0021;

/// Register `reg0022` at 0x0058: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.rdo_ckg_hevc`.
pub mod reg0022 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.rdo_ckg_hevc",
        22,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RECON32_CKG: Field = Field::new(Region::Class(0), 22, 0, 1, false, true, &[]);
    pub const IQIT32_CKG: Field = Field::new(Region::Class(0), 22, 1, 1, false, true, &[]);
    pub const Q32_CKG: Field = Field::new(Region::Class(0), 22, 2, 1, false, true, &[]);
    pub const T32_CKG: Field = Field::new(Region::Class(0), 22, 3, 1, false, true, &[]);
    pub const CABAC32_CKG: Field = Field::new(Region::Class(0), 22, 4, 1, false, true, &[]);
    pub const RECON16_CKG: Field = Field::new(Region::Class(0), 22, 5, 1, false, true, &[]);
    pub const IQIT16_CKG: Field = Field::new(Region::Class(0), 22, 6, 1, false, true, &[]);
    pub const Q16_CKG: Field = Field::new(Region::Class(0), 22, 7, 1, false, true, &[]);
    pub const T16_CKG: Field = Field::new(Region::Class(0), 22, 8, 1, false, true, &[]);
    pub const CABAC16_CKG: Field = Field::new(Region::Class(0), 22, 9, 1, false, true, &[]);
    pub const RECON8_CKG: Field = Field::new(Region::Class(0), 22, 10, 1, false, true, &[]);
    pub const IQIT8_CKG: Field = Field::new(Region::Class(0), 22, 11, 1, false, true, &[]);
    pub const Q8_CKG: Field = Field::new(Region::Class(0), 22, 12, 1, false, true, &[]);
    pub const T8_CKG: Field = Field::new(Region::Class(0), 22, 13, 1, false, true, &[]);
    pub const CABAC8_CKG: Field = Field::new(Region::Class(0), 22, 14, 1, false, true, &[]);
    pub const RECON4_CKG: Field = Field::new(Region::Class(0), 22, 15, 1, false, true, &[]);
    pub const IQIT4_CKG: Field = Field::new(Region::Class(0), 22, 16, 1, false, true, &[]);
    pub const Q4_CKG: Field = Field::new(Region::Class(0), 22, 17, 1, false, true, &[]);
    pub const T4_CKG: Field = Field::new(Region::Class(0), 22, 18, 1, false, true, &[]);
    pub const CABAC4_CKG: Field = Field::new(Region::Class(0), 22, 19, 1, false, true, &[]);
    pub const INTRA32_CKG: Field = Field::new(Region::Class(0), 22, 20, 1, false, true, &[]);
    pub const INTRA16_CKG: Field = Field::new(Region::Class(0), 22, 21, 1, false, true, &[]);
    pub const INTRA8_CKG: Field = Field::new(Region::Class(0), 22, 22, 1, false, true, &[]);
    pub const INTRA4_CKG: Field = Field::new(Region::Class(0), 22, 23, 1, false, true, &[]);
    pub const INTER_PRED_CKG: Field = Field::new(Region::Class(0), 22, 24, 1, false, true, &[]);
}
