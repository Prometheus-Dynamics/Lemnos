//! The VEPU580 H.264 SCL (kernel class 5): the scaling-list registers (reg_scl).
//!
//! Words 2176 to 2854 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg2176` at 0x2200: 32 words, ReadWrite.
///
/// The C path is `reg_scl.intra8_y`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2176 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.intra8_y",
        2176,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2208` at 0x2280: 32 words, ReadWrite.
///
/// The C path is `reg_scl.intra8_u`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2208 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.intra8_u",
        2208,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2240` at 0x2300: 32 words, ReadWrite.
///
/// The C path is `reg_scl.intra8_v`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2240 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.intra8_v",
        2240,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2272` at 0x2380: 32 words, ReadWrite.
///
/// The C path is `reg_scl.inter8_y`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2272 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.inter8_y",
        2272,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2304` at 0x2400: 32 words, ReadWrite.
///
/// The C path is `reg_scl.inter8_u`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2304 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.inter8_u",
        2304,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2336` at 0x2480: 32 words, ReadWrite.
///
/// The C path is `reg_scl.inter8_v`. Packed: two RK_U16 entries per word, the even entry in the low half.
pub mod reg2336 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.inter8_v",
        2336,
        32,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2368` at 0x2500: 480 words, ReadWrite.
///
/// The C path is `reg_scl.q_iq_16_32`. An array of 480 RK_U32 entries, one per word.
pub mod reg2368 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.q_iq_16_32",
        2368,
        480,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2848` at 0x2c80: 1 word, ReadWrite.
///
/// The C path is `reg_scl.q_dc_y16`.
pub mod reg2848 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.q_dc_y16",
        2848,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2849` at 0x2c84: 1 word, ReadWrite.
///
/// The C path is `reg_scl.q_dc_u16`.
pub mod reg2849 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.q_dc_u16",
        2849,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2850` at 0x2c88: 1 word, ReadWrite.
///
/// The C path is `reg_scl.q_dc_v16`.
pub mod reg2850 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.q_dc_v16",
        2850,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2851` at 0x2c8c: 1 word, ReadWrite.
///
/// The C path is `reg_scl.q_dc_v32`.
pub mod reg2851 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.q_dc_v32",
        2851,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2852` at 0x2c90: 1 word, ReadWrite.
///
/// The C path is `reg_scl.iq_dc_0`.
pub mod reg2852 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.iq_dc_0",
        2852,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2853` at 0x2c94: 1 word, ReadWrite.
///
/// The C path is `reg_scl.iq_dc_1`.
pub mod reg2853 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.iq_dc_1",
        2853,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg2854` at 0x2c98: 1 word, ReadWrite.
///
/// The C path is `reg_scl.scal_clk_sel`.
pub mod reg2854 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(5),
        "reg_scl.scal_clk_sel",
        2854,
        1,
        Access::ReadWrite,
        1,
    );
}
