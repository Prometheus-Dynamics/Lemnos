//! The VEPU580 H.265 SCL (kernel class 5): the scaling-list registers, inside reg_rdo.
//!
//! Words 2176 to 2854 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg2176` at 0x2200: 678 words, ReadWrite.
///
/// The C path is `reg_rdo.scaling_list_reg`. An array of 678 RK_U32 entries, one per word.
pub mod reg2176 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_rdo.scaling_list_reg",
        2176,
        678,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2854` at 0x2c98: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.scal_cfg_reg`.
pub mod reg2854 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_rdo.scal_cfg_reg",
        2854,
        1,
        Access::ReadWrite,
        1,
    );
}
