//! The VEPU580 H.264 OSD (kernel class 6): the OSD position, address and palette registers (reg_osd).
//!
//! Words 3072 to 3359 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg3072` at 0x3000: 1 word, ReadWrite.
///
/// The C path is `reg_osd.reg3072`.
pub mod reg3072 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.reg3072",
        3072,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LU_INV_EN: Field = Field::new(Region::Class(6), 3072, 0, 8, false, true, &[]);
    pub const OSD_CH_INV_EN: Field = Field::new(Region::Class(6), 3072, 8, 8, false, true, &[]);
    pub const OSD_LU_INV_MSK: Field = Field::new(Region::Class(6), 3072, 16, 8, false, true, &[]);
    pub const OSD_CH_INV_MSK: Field = Field::new(Region::Class(6), 3072, 24, 8, false, true, &[]);
}

/// Register `reg3073` at 0x3004: 1 word, ReadWrite.
///
/// The C path is `reg_osd.reg3073`.
pub mod reg3073 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.reg3073",
        3073,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_ITHD_R0: Field = Field::new(Region::Class(6), 3073, 0, 4, false, true, &[]);
    pub const OSD_ITHD_R1: Field = Field::new(Region::Class(6), 3073, 4, 4, false, true, &[]);
    pub const OSD_ITHD_R2: Field = Field::new(Region::Class(6), 3073, 8, 4, false, true, &[]);
    pub const OSD_ITHD_R3: Field = Field::new(Region::Class(6), 3073, 12, 4, false, true, &[]);
    pub const OSD_ITHD_R4: Field = Field::new(Region::Class(6), 3073, 16, 4, false, true, &[]);
    pub const OSD_ITHD_R5: Field = Field::new(Region::Class(6), 3073, 20, 4, false, true, &[]);
    pub const OSD_ITHD_R6: Field = Field::new(Region::Class(6), 3073, 24, 4, false, true, &[]);
    pub const OSD_ITHD_R7: Field = Field::new(Region::Class(6), 3073, 28, 4, false, true, &[]);
}

/// Register `reg3074` at 0x3008: 1 word, ReadWrite.
///
/// The C path is `reg_osd.reg3074`.
pub mod reg3074 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.reg3074",
        3074,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_E: Field = Field::new(Region::Class(6), 3074, 0, 8, false, true, &[]);
    pub const OSD_ITYPE: Field = Field::new(Region::Class(6), 3074, 8, 8, false, true, &[]);
    pub const OSD_PLT_CKS: Field = Field::new(Region::Class(6), 3074, 16, 1, false, true, &[]);
    pub const OSD_PLT_TYP: Field = Field::new(Region::Class(6), 3074, 17, 1, false, true, &[]);
}

/// Register `reg3076` at 0x3010: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[0].w0`.
pub mod reg3076 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[0].w0",
        3076,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3076, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3076, 16, 10, false, true, &[]);
}

/// Register `reg3077` at 0x3014: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[0].w1`.
pub mod reg3077 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[0].w1",
        3077,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3077, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3077, 16, 10, false, true, &[]);
}

/// Register `reg3078` at 0x3018: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[1].w0`.
pub mod reg3078 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[1].w0",
        3078,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3078, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3078, 16, 10, false, true, &[]);
}

/// Register `reg3079` at 0x301c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[1].w1`.
pub mod reg3079 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[1].w1",
        3079,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3079, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3079, 16, 10, false, true, &[]);
}

/// Register `reg3080` at 0x3020: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[2].w0`.
pub mod reg3080 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[2].w0",
        3080,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3080, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3080, 16, 10, false, true, &[]);
}

/// Register `reg3081` at 0x3024: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[2].w1`.
pub mod reg3081 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[2].w1",
        3081,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3081, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3081, 16, 10, false, true, &[]);
}

/// Register `reg3082` at 0x3028: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[3].w0`.
pub mod reg3082 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[3].w0",
        3082,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3082, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3082, 16, 10, false, true, &[]);
}

/// Register `reg3083` at 0x302c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[3].w1`.
pub mod reg3083 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[3].w1",
        3083,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3083, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3083, 16, 10, false, true, &[]);
}

/// Register `reg3084` at 0x3030: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[4].w0`.
pub mod reg3084 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[4].w0",
        3084,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3084, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3084, 16, 10, false, true, &[]);
}

/// Register `reg3085` at 0x3034: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[4].w1`.
pub mod reg3085 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[4].w1",
        3085,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3085, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3085, 16, 10, false, true, &[]);
}

/// Register `reg3086` at 0x3038: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[5].w0`.
pub mod reg3086 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[5].w0",
        3086,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3086, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3086, 16, 10, false, true, &[]);
}

/// Register `reg3087` at 0x303c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[5].w1`.
pub mod reg3087 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[5].w1",
        3087,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3087, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3087, 16, 10, false, true, &[]);
}

/// Register `reg3088` at 0x3040: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[6].w0`.
pub mod reg3088 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[6].w0",
        3088,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3088, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3088, 16, 10, false, true, &[]);
}

/// Register `reg3089` at 0x3044: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[6].w1`.
pub mod reg3089 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[6].w1",
        3089,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3089, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3089, 16, 10, false, true, &[]);
}

/// Register `reg3090` at 0x3048: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[7].w0`.
pub mod reg3090 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[7].w0",
        3090,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_LT_X: Field = Field::new(Region::Class(6), 3090, 0, 10, false, true, &[]);
    pub const OSD_LT_Y: Field = Field::new(Region::Class(6), 3090, 16, 10, false, true, &[]);
}

/// Register `reg3091` at 0x304c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.osd_pos[7].w1`.
pub mod reg3091 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_pos[7].w1",
        3091,
        1,
        Access::ReadWrite,
        1,
    );
    pub const OSD_RB_X: Field = Field::new(Region::Class(6), 3091, 0, 10, false, true, &[]);
    pub const OSD_RB_Y: Field = Field::new(Region::Class(6), 3091, 16, 10, false, true, &[]);
}

/// Register `reg3092` at 0x3050: 8 words, ReadWrite.
///
/// The C path is `reg_osd.osd_addr`. An array of 8 RK_U32 entries, one per word.
pub mod reg3092 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.osd_addr",
        3092,
        8,
        Access::ReadWrite,
        16,
    );
    /// Each entry is a buffer's bus address: the driver writes it from the caller's buffer.
    pub const OSD_ADDR: [AddressReg; 8] = [
        AddressReg::new(Region::Class(6), 3092, 16),
        AddressReg::new(Region::Class(6), 3093, 16),
        AddressReg::new(Region::Class(6), 3094, 16),
        AddressReg::new(Region::Class(6), 3095, 16),
        AddressReg::new(Region::Class(6), 3096, 16),
        AddressReg::new(Region::Class(6), 3097, 16),
        AddressReg::new(Region::Class(6), 3098, 16),
        AddressReg::new(Region::Class(6), 3099, 16),
    ];
}

/// Register `reg3104` at 0x3080: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[0]`.
pub mod reg3104 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[0]",
        3104,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3104, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3104, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3104, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3104, 24, 8, false, true, &[]);
}

/// Register `reg3105` at 0x3084: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[1]`.
pub mod reg3105 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[1]",
        3105,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3105, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3105, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3105, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3105, 24, 8, false, true, &[]);
}

/// Register `reg3106` at 0x3088: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[2]`.
pub mod reg3106 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[2]",
        3106,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3106, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3106, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3106, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3106, 24, 8, false, true, &[]);
}

/// Register `reg3107` at 0x308c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[3]`.
pub mod reg3107 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[3]",
        3107,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3107, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3107, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3107, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3107, 24, 8, false, true, &[]);
}

/// Register `reg3108` at 0x3090: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[4]`.
pub mod reg3108 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[4]",
        3108,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3108, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3108, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3108, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3108, 24, 8, false, true, &[]);
}

/// Register `reg3109` at 0x3094: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[5]`.
pub mod reg3109 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[5]",
        3109,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3109, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3109, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3109, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3109, 24, 8, false, true, &[]);
}

/// Register `reg3110` at 0x3098: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[6]`.
pub mod reg3110 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[6]",
        3110,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3110, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3110, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3110, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3110, 24, 8, false, true, &[]);
}

/// Register `reg3111` at 0x309c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[7]`.
pub mod reg3111 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[7]",
        3111,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3111, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3111, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3111, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3111, 24, 8, false, true, &[]);
}

/// Register `reg3112` at 0x30a0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[8]`.
pub mod reg3112 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[8]",
        3112,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3112, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3112, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3112, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3112, 24, 8, false, true, &[]);
}

/// Register `reg3113` at 0x30a4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[9]`.
pub mod reg3113 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[9]",
        3113,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3113, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3113, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3113, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3113, 24, 8, false, true, &[]);
}

/// Register `reg3114` at 0x30a8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[10]`.
pub mod reg3114 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[10]",
        3114,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3114, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3114, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3114, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3114, 24, 8, false, true, &[]);
}

/// Register `reg3115` at 0x30ac: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[11]`.
pub mod reg3115 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[11]",
        3115,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3115, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3115, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3115, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3115, 24, 8, false, true, &[]);
}

/// Register `reg3116` at 0x30b0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[12]`.
pub mod reg3116 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[12]",
        3116,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3116, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3116, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3116, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3116, 24, 8, false, true, &[]);
}

/// Register `reg3117` at 0x30b4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[13]`.
pub mod reg3117 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[13]",
        3117,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3117, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3117, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3117, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3117, 24, 8, false, true, &[]);
}

/// Register `reg3118` at 0x30b8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[14]`.
pub mod reg3118 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[14]",
        3118,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3118, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3118, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3118, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3118, 24, 8, false, true, &[]);
}

/// Register `reg3119` at 0x30bc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[15]`.
pub mod reg3119 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[15]",
        3119,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3119, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3119, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3119, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3119, 24, 8, false, true, &[]);
}

/// Register `reg3120` at 0x30c0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[16]`.
pub mod reg3120 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[16]",
        3120,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3120, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3120, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3120, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3120, 24, 8, false, true, &[]);
}

/// Register `reg3121` at 0x30c4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[17]`.
pub mod reg3121 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[17]",
        3121,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3121, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3121, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3121, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3121, 24, 8, false, true, &[]);
}

/// Register `reg3122` at 0x30c8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[18]`.
pub mod reg3122 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[18]",
        3122,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3122, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3122, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3122, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3122, 24, 8, false, true, &[]);
}

/// Register `reg3123` at 0x30cc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[19]`.
pub mod reg3123 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[19]",
        3123,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3123, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3123, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3123, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3123, 24, 8, false, true, &[]);
}

/// Register `reg3124` at 0x30d0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[20]`.
pub mod reg3124 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[20]",
        3124,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3124, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3124, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3124, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3124, 24, 8, false, true, &[]);
}

/// Register `reg3125` at 0x30d4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[21]`.
pub mod reg3125 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[21]",
        3125,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3125, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3125, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3125, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3125, 24, 8, false, true, &[]);
}

/// Register `reg3126` at 0x30d8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[22]`.
pub mod reg3126 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[22]",
        3126,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3126, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3126, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3126, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3126, 24, 8, false, true, &[]);
}

/// Register `reg3127` at 0x30dc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[23]`.
pub mod reg3127 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[23]",
        3127,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3127, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3127, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3127, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3127, 24, 8, false, true, &[]);
}

/// Register `reg3128` at 0x30e0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[24]`.
pub mod reg3128 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[24]",
        3128,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3128, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3128, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3128, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3128, 24, 8, false, true, &[]);
}

/// Register `reg3129` at 0x30e4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[25]`.
pub mod reg3129 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[25]",
        3129,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3129, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3129, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3129, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3129, 24, 8, false, true, &[]);
}

/// Register `reg3130` at 0x30e8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[26]`.
pub mod reg3130 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[26]",
        3130,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3130, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3130, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3130, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3130, 24, 8, false, true, &[]);
}

/// Register `reg3131` at 0x30ec: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[27]`.
pub mod reg3131 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[27]",
        3131,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3131, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3131, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3131, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3131, 24, 8, false, true, &[]);
}

/// Register `reg3132` at 0x30f0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[28]`.
pub mod reg3132 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[28]",
        3132,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3132, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3132, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3132, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3132, 24, 8, false, true, &[]);
}

/// Register `reg3133` at 0x30f4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[29]`.
pub mod reg3133 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[29]",
        3133,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3133, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3133, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3133, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3133, 24, 8, false, true, &[]);
}

/// Register `reg3134` at 0x30f8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[30]`.
pub mod reg3134 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[30]",
        3134,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3134, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3134, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3134, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3134, 24, 8, false, true, &[]);
}

/// Register `reg3135` at 0x30fc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[31]`.
pub mod reg3135 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[31]",
        3135,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3135, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3135, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3135, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3135, 24, 8, false, true, &[]);
}

/// Register `reg3136` at 0x3100: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[32]`.
pub mod reg3136 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[32]",
        3136,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3136, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3136, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3136, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3136, 24, 8, false, true, &[]);
}

/// Register `reg3137` at 0x3104: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[33]`.
pub mod reg3137 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[33]",
        3137,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3137, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3137, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3137, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3137, 24, 8, false, true, &[]);
}

/// Register `reg3138` at 0x3108: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[34]`.
pub mod reg3138 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[34]",
        3138,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3138, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3138, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3138, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3138, 24, 8, false, true, &[]);
}

/// Register `reg3139` at 0x310c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[35]`.
pub mod reg3139 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[35]",
        3139,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3139, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3139, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3139, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3139, 24, 8, false, true, &[]);
}

/// Register `reg3140` at 0x3110: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[36]`.
pub mod reg3140 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[36]",
        3140,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3140, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3140, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3140, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3140, 24, 8, false, true, &[]);
}

/// Register `reg3141` at 0x3114: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[37]`.
pub mod reg3141 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[37]",
        3141,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3141, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3141, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3141, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3141, 24, 8, false, true, &[]);
}

/// Register `reg3142` at 0x3118: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[38]`.
pub mod reg3142 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[38]",
        3142,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3142, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3142, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3142, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3142, 24, 8, false, true, &[]);
}

/// Register `reg3143` at 0x311c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[39]`.
pub mod reg3143 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[39]",
        3143,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3143, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3143, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3143, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3143, 24, 8, false, true, &[]);
}

/// Register `reg3144` at 0x3120: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[40]`.
pub mod reg3144 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[40]",
        3144,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3144, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3144, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3144, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3144, 24, 8, false, true, &[]);
}

/// Register `reg3145` at 0x3124: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[41]`.
pub mod reg3145 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[41]",
        3145,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3145, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3145, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3145, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3145, 24, 8, false, true, &[]);
}

/// Register `reg3146` at 0x3128: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[42]`.
pub mod reg3146 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[42]",
        3146,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3146, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3146, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3146, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3146, 24, 8, false, true, &[]);
}

/// Register `reg3147` at 0x312c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[43]`.
pub mod reg3147 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[43]",
        3147,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3147, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3147, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3147, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3147, 24, 8, false, true, &[]);
}

/// Register `reg3148` at 0x3130: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[44]`.
pub mod reg3148 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[44]",
        3148,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3148, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3148, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3148, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3148, 24, 8, false, true, &[]);
}

/// Register `reg3149` at 0x3134: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[45]`.
pub mod reg3149 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[45]",
        3149,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3149, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3149, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3149, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3149, 24, 8, false, true, &[]);
}

/// Register `reg3150` at 0x3138: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[46]`.
pub mod reg3150 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[46]",
        3150,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3150, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3150, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3150, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3150, 24, 8, false, true, &[]);
}

/// Register `reg3151` at 0x313c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[47]`.
pub mod reg3151 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[47]",
        3151,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3151, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3151, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3151, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3151, 24, 8, false, true, &[]);
}

/// Register `reg3152` at 0x3140: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[48]`.
pub mod reg3152 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[48]",
        3152,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3152, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3152, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3152, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3152, 24, 8, false, true, &[]);
}

/// Register `reg3153` at 0x3144: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[49]`.
pub mod reg3153 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[49]",
        3153,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3153, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3153, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3153, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3153, 24, 8, false, true, &[]);
}

/// Register `reg3154` at 0x3148: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[50]`.
pub mod reg3154 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[50]",
        3154,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3154, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3154, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3154, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3154, 24, 8, false, true, &[]);
}

/// Register `reg3155` at 0x314c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[51]`.
pub mod reg3155 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[51]",
        3155,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3155, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3155, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3155, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3155, 24, 8, false, true, &[]);
}

/// Register `reg3156` at 0x3150: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[52]`.
pub mod reg3156 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[52]",
        3156,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3156, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3156, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3156, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3156, 24, 8, false, true, &[]);
}

/// Register `reg3157` at 0x3154: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[53]`.
pub mod reg3157 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[53]",
        3157,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3157, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3157, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3157, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3157, 24, 8, false, true, &[]);
}

/// Register `reg3158` at 0x3158: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[54]`.
pub mod reg3158 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[54]",
        3158,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3158, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3158, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3158, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3158, 24, 8, false, true, &[]);
}

/// Register `reg3159` at 0x315c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[55]`.
pub mod reg3159 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[55]",
        3159,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3159, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3159, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3159, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3159, 24, 8, false, true, &[]);
}

/// Register `reg3160` at 0x3160: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[56]`.
pub mod reg3160 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[56]",
        3160,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3160, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3160, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3160, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3160, 24, 8, false, true, &[]);
}

/// Register `reg3161` at 0x3164: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[57]`.
pub mod reg3161 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[57]",
        3161,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3161, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3161, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3161, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3161, 24, 8, false, true, &[]);
}

/// Register `reg3162` at 0x3168: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[58]`.
pub mod reg3162 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[58]",
        3162,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3162, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3162, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3162, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3162, 24, 8, false, true, &[]);
}

/// Register `reg3163` at 0x316c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[59]`.
pub mod reg3163 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[59]",
        3163,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3163, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3163, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3163, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3163, 24, 8, false, true, &[]);
}

/// Register `reg3164` at 0x3170: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[60]`.
pub mod reg3164 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[60]",
        3164,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3164, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3164, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3164, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3164, 24, 8, false, true, &[]);
}

/// Register `reg3165` at 0x3174: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[61]`.
pub mod reg3165 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[61]",
        3165,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3165, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3165, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3165, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3165, 24, 8, false, true, &[]);
}

/// Register `reg3166` at 0x3178: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[62]`.
pub mod reg3166 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[62]",
        3166,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3166, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3166, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3166, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3166, 24, 8, false, true, &[]);
}

/// Register `reg3167` at 0x317c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[63]`.
pub mod reg3167 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[63]",
        3167,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3167, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3167, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3167, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3167, 24, 8, false, true, &[]);
}

/// Register `reg3168` at 0x3180: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[64]`.
pub mod reg3168 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[64]",
        3168,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3168, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3168, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3168, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3168, 24, 8, false, true, &[]);
}

/// Register `reg3169` at 0x3184: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[65]`.
pub mod reg3169 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[65]",
        3169,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3169, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3169, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3169, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3169, 24, 8, false, true, &[]);
}

/// Register `reg3170` at 0x3188: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[66]`.
pub mod reg3170 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[66]",
        3170,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3170, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3170, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3170, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3170, 24, 8, false, true, &[]);
}

/// Register `reg3171` at 0x318c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[67]`.
pub mod reg3171 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[67]",
        3171,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3171, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3171, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3171, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3171, 24, 8, false, true, &[]);
}

/// Register `reg3172` at 0x3190: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[68]`.
pub mod reg3172 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[68]",
        3172,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3172, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3172, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3172, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3172, 24, 8, false, true, &[]);
}

/// Register `reg3173` at 0x3194: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[69]`.
pub mod reg3173 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[69]",
        3173,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3173, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3173, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3173, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3173, 24, 8, false, true, &[]);
}

/// Register `reg3174` at 0x3198: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[70]`.
pub mod reg3174 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[70]",
        3174,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3174, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3174, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3174, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3174, 24, 8, false, true, &[]);
}

/// Register `reg3175` at 0x319c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[71]`.
pub mod reg3175 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[71]",
        3175,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3175, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3175, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3175, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3175, 24, 8, false, true, &[]);
}

/// Register `reg3176` at 0x31a0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[72]`.
pub mod reg3176 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[72]",
        3176,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3176, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3176, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3176, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3176, 24, 8, false, true, &[]);
}

/// Register `reg3177` at 0x31a4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[73]`.
pub mod reg3177 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[73]",
        3177,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3177, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3177, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3177, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3177, 24, 8, false, true, &[]);
}

/// Register `reg3178` at 0x31a8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[74]`.
pub mod reg3178 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[74]",
        3178,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3178, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3178, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3178, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3178, 24, 8, false, true, &[]);
}

/// Register `reg3179` at 0x31ac: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[75]`.
pub mod reg3179 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[75]",
        3179,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3179, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3179, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3179, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3179, 24, 8, false, true, &[]);
}

/// Register `reg3180` at 0x31b0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[76]`.
pub mod reg3180 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[76]",
        3180,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3180, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3180, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3180, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3180, 24, 8, false, true, &[]);
}

/// Register `reg3181` at 0x31b4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[77]`.
pub mod reg3181 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[77]",
        3181,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3181, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3181, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3181, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3181, 24, 8, false, true, &[]);
}

/// Register `reg3182` at 0x31b8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[78]`.
pub mod reg3182 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[78]",
        3182,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3182, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3182, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3182, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3182, 24, 8, false, true, &[]);
}

/// Register `reg3183` at 0x31bc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[79]`.
pub mod reg3183 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[79]",
        3183,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3183, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3183, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3183, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3183, 24, 8, false, true, &[]);
}

/// Register `reg3184` at 0x31c0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[80]`.
pub mod reg3184 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[80]",
        3184,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3184, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3184, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3184, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3184, 24, 8, false, true, &[]);
}

/// Register `reg3185` at 0x31c4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[81]`.
pub mod reg3185 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[81]",
        3185,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3185, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3185, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3185, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3185, 24, 8, false, true, &[]);
}

/// Register `reg3186` at 0x31c8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[82]`.
pub mod reg3186 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[82]",
        3186,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3186, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3186, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3186, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3186, 24, 8, false, true, &[]);
}

/// Register `reg3187` at 0x31cc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[83]`.
pub mod reg3187 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[83]",
        3187,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3187, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3187, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3187, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3187, 24, 8, false, true, &[]);
}

/// Register `reg3188` at 0x31d0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[84]`.
pub mod reg3188 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[84]",
        3188,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3188, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3188, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3188, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3188, 24, 8, false, true, &[]);
}

/// Register `reg3189` at 0x31d4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[85]`.
pub mod reg3189 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[85]",
        3189,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3189, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3189, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3189, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3189, 24, 8, false, true, &[]);
}

/// Register `reg3190` at 0x31d8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[86]`.
pub mod reg3190 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[86]",
        3190,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3190, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3190, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3190, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3190, 24, 8, false, true, &[]);
}

/// Register `reg3191` at 0x31dc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[87]`.
pub mod reg3191 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[87]",
        3191,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3191, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3191, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3191, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3191, 24, 8, false, true, &[]);
}

/// Register `reg3192` at 0x31e0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[88]`.
pub mod reg3192 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[88]",
        3192,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3192, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3192, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3192, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3192, 24, 8, false, true, &[]);
}

/// Register `reg3193` at 0x31e4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[89]`.
pub mod reg3193 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[89]",
        3193,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3193, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3193, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3193, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3193, 24, 8, false, true, &[]);
}

/// Register `reg3194` at 0x31e8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[90]`.
pub mod reg3194 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[90]",
        3194,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3194, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3194, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3194, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3194, 24, 8, false, true, &[]);
}

/// Register `reg3195` at 0x31ec: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[91]`.
pub mod reg3195 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[91]",
        3195,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3195, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3195, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3195, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3195, 24, 8, false, true, &[]);
}

/// Register `reg3196` at 0x31f0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[92]`.
pub mod reg3196 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[92]",
        3196,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3196, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3196, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3196, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3196, 24, 8, false, true, &[]);
}

/// Register `reg3197` at 0x31f4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[93]`.
pub mod reg3197 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[93]",
        3197,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3197, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3197, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3197, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3197, 24, 8, false, true, &[]);
}

/// Register `reg3198` at 0x31f8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[94]`.
pub mod reg3198 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[94]",
        3198,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3198, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3198, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3198, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3198, 24, 8, false, true, &[]);
}

/// Register `reg3199` at 0x31fc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[95]`.
pub mod reg3199 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[95]",
        3199,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3199, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3199, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3199, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3199, 24, 8, false, true, &[]);
}

/// Register `reg3200` at 0x3200: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[96]`.
pub mod reg3200 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[96]",
        3200,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3200, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3200, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3200, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3200, 24, 8, false, true, &[]);
}

/// Register `reg3201` at 0x3204: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[97]`.
pub mod reg3201 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[97]",
        3201,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3201, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3201, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3201, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3201, 24, 8, false, true, &[]);
}

/// Register `reg3202` at 0x3208: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[98]`.
pub mod reg3202 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[98]",
        3202,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3202, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3202, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3202, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3202, 24, 8, false, true, &[]);
}

/// Register `reg3203` at 0x320c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[99]`.
pub mod reg3203 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[99]",
        3203,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3203, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3203, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3203, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3203, 24, 8, false, true, &[]);
}

/// Register `reg3204` at 0x3210: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[100]`.
pub mod reg3204 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[100]",
        3204,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3204, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3204, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3204, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3204, 24, 8, false, true, &[]);
}

/// Register `reg3205` at 0x3214: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[101]`.
pub mod reg3205 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[101]",
        3205,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3205, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3205, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3205, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3205, 24, 8, false, true, &[]);
}

/// Register `reg3206` at 0x3218: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[102]`.
pub mod reg3206 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[102]",
        3206,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3206, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3206, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3206, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3206, 24, 8, false, true, &[]);
}

/// Register `reg3207` at 0x321c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[103]`.
pub mod reg3207 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[103]",
        3207,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3207, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3207, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3207, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3207, 24, 8, false, true, &[]);
}

/// Register `reg3208` at 0x3220: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[104]`.
pub mod reg3208 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[104]",
        3208,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3208, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3208, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3208, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3208, 24, 8, false, true, &[]);
}

/// Register `reg3209` at 0x3224: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[105]`.
pub mod reg3209 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[105]",
        3209,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3209, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3209, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3209, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3209, 24, 8, false, true, &[]);
}

/// Register `reg3210` at 0x3228: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[106]`.
pub mod reg3210 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[106]",
        3210,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3210, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3210, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3210, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3210, 24, 8, false, true, &[]);
}

/// Register `reg3211` at 0x322c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[107]`.
pub mod reg3211 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[107]",
        3211,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3211, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3211, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3211, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3211, 24, 8, false, true, &[]);
}

/// Register `reg3212` at 0x3230: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[108]`.
pub mod reg3212 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[108]",
        3212,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3212, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3212, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3212, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3212, 24, 8, false, true, &[]);
}

/// Register `reg3213` at 0x3234: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[109]`.
pub mod reg3213 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[109]",
        3213,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3213, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3213, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3213, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3213, 24, 8, false, true, &[]);
}

/// Register `reg3214` at 0x3238: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[110]`.
pub mod reg3214 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[110]",
        3214,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3214, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3214, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3214, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3214, 24, 8, false, true, &[]);
}

/// Register `reg3215` at 0x323c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[111]`.
pub mod reg3215 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[111]",
        3215,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3215, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3215, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3215, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3215, 24, 8, false, true, &[]);
}

/// Register `reg3216` at 0x3240: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[112]`.
pub mod reg3216 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[112]",
        3216,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3216, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3216, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3216, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3216, 24, 8, false, true, &[]);
}

/// Register `reg3217` at 0x3244: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[113]`.
pub mod reg3217 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[113]",
        3217,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3217, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3217, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3217, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3217, 24, 8, false, true, &[]);
}

/// Register `reg3218` at 0x3248: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[114]`.
pub mod reg3218 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[114]",
        3218,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3218, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3218, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3218, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3218, 24, 8, false, true, &[]);
}

/// Register `reg3219` at 0x324c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[115]`.
pub mod reg3219 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[115]",
        3219,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3219, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3219, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3219, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3219, 24, 8, false, true, &[]);
}

/// Register `reg3220` at 0x3250: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[116]`.
pub mod reg3220 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[116]",
        3220,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3220, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3220, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3220, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3220, 24, 8, false, true, &[]);
}

/// Register `reg3221` at 0x3254: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[117]`.
pub mod reg3221 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[117]",
        3221,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3221, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3221, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3221, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3221, 24, 8, false, true, &[]);
}

/// Register `reg3222` at 0x3258: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[118]`.
pub mod reg3222 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[118]",
        3222,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3222, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3222, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3222, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3222, 24, 8, false, true, &[]);
}

/// Register `reg3223` at 0x325c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[119]`.
pub mod reg3223 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[119]",
        3223,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3223, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3223, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3223, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3223, 24, 8, false, true, &[]);
}

/// Register `reg3224` at 0x3260: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[120]`.
pub mod reg3224 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[120]",
        3224,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3224, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3224, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3224, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3224, 24, 8, false, true, &[]);
}

/// Register `reg3225` at 0x3264: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[121]`.
pub mod reg3225 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[121]",
        3225,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3225, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3225, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3225, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3225, 24, 8, false, true, &[]);
}

/// Register `reg3226` at 0x3268: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[122]`.
pub mod reg3226 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[122]",
        3226,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3226, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3226, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3226, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3226, 24, 8, false, true, &[]);
}

/// Register `reg3227` at 0x326c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[123]`.
pub mod reg3227 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[123]",
        3227,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3227, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3227, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3227, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3227, 24, 8, false, true, &[]);
}

/// Register `reg3228` at 0x3270: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[124]`.
pub mod reg3228 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[124]",
        3228,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3228, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3228, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3228, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3228, 24, 8, false, true, &[]);
}

/// Register `reg3229` at 0x3274: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[125]`.
pub mod reg3229 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[125]",
        3229,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3229, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3229, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3229, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3229, 24, 8, false, true, &[]);
}

/// Register `reg3230` at 0x3278: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[126]`.
pub mod reg3230 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[126]",
        3230,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3230, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3230, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3230, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3230, 24, 8, false, true, &[]);
}

/// Register `reg3231` at 0x327c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[127]`.
pub mod reg3231 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[127]",
        3231,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3231, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3231, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3231, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3231, 24, 8, false, true, &[]);
}

/// Register `reg3232` at 0x3280: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[128]`.
pub mod reg3232 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[128]",
        3232,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3232, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3232, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3232, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3232, 24, 8, false, true, &[]);
}

/// Register `reg3233` at 0x3284: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[129]`.
pub mod reg3233 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[129]",
        3233,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3233, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3233, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3233, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3233, 24, 8, false, true, &[]);
}

/// Register `reg3234` at 0x3288: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[130]`.
pub mod reg3234 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[130]",
        3234,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3234, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3234, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3234, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3234, 24, 8, false, true, &[]);
}

/// Register `reg3235` at 0x328c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[131]`.
pub mod reg3235 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[131]",
        3235,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3235, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3235, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3235, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3235, 24, 8, false, true, &[]);
}

/// Register `reg3236` at 0x3290: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[132]`.
pub mod reg3236 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[132]",
        3236,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3236, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3236, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3236, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3236, 24, 8, false, true, &[]);
}

/// Register `reg3237` at 0x3294: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[133]`.
pub mod reg3237 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[133]",
        3237,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3237, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3237, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3237, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3237, 24, 8, false, true, &[]);
}

/// Register `reg3238` at 0x3298: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[134]`.
pub mod reg3238 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[134]",
        3238,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3238, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3238, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3238, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3238, 24, 8, false, true, &[]);
}

/// Register `reg3239` at 0x329c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[135]`.
pub mod reg3239 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[135]",
        3239,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3239, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3239, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3239, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3239, 24, 8, false, true, &[]);
}

/// Register `reg3240` at 0x32a0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[136]`.
pub mod reg3240 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[136]",
        3240,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3240, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3240, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3240, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3240, 24, 8, false, true, &[]);
}

/// Register `reg3241` at 0x32a4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[137]`.
pub mod reg3241 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[137]",
        3241,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3241, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3241, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3241, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3241, 24, 8, false, true, &[]);
}

/// Register `reg3242` at 0x32a8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[138]`.
pub mod reg3242 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[138]",
        3242,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3242, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3242, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3242, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3242, 24, 8, false, true, &[]);
}

/// Register `reg3243` at 0x32ac: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[139]`.
pub mod reg3243 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[139]",
        3243,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3243, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3243, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3243, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3243, 24, 8, false, true, &[]);
}

/// Register `reg3244` at 0x32b0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[140]`.
pub mod reg3244 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[140]",
        3244,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3244, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3244, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3244, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3244, 24, 8, false, true, &[]);
}

/// Register `reg3245` at 0x32b4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[141]`.
pub mod reg3245 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[141]",
        3245,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3245, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3245, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3245, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3245, 24, 8, false, true, &[]);
}

/// Register `reg3246` at 0x32b8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[142]`.
pub mod reg3246 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[142]",
        3246,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3246, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3246, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3246, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3246, 24, 8, false, true, &[]);
}

/// Register `reg3247` at 0x32bc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[143]`.
pub mod reg3247 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[143]",
        3247,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3247, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3247, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3247, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3247, 24, 8, false, true, &[]);
}

/// Register `reg3248` at 0x32c0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[144]`.
pub mod reg3248 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[144]",
        3248,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3248, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3248, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3248, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3248, 24, 8, false, true, &[]);
}

/// Register `reg3249` at 0x32c4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[145]`.
pub mod reg3249 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[145]",
        3249,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3249, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3249, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3249, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3249, 24, 8, false, true, &[]);
}

/// Register `reg3250` at 0x32c8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[146]`.
pub mod reg3250 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[146]",
        3250,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3250, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3250, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3250, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3250, 24, 8, false, true, &[]);
}

/// Register `reg3251` at 0x32cc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[147]`.
pub mod reg3251 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[147]",
        3251,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3251, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3251, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3251, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3251, 24, 8, false, true, &[]);
}

/// Register `reg3252` at 0x32d0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[148]`.
pub mod reg3252 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[148]",
        3252,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3252, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3252, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3252, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3252, 24, 8, false, true, &[]);
}

/// Register `reg3253` at 0x32d4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[149]`.
pub mod reg3253 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[149]",
        3253,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3253, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3253, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3253, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3253, 24, 8, false, true, &[]);
}

/// Register `reg3254` at 0x32d8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[150]`.
pub mod reg3254 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[150]",
        3254,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3254, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3254, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3254, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3254, 24, 8, false, true, &[]);
}

/// Register `reg3255` at 0x32dc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[151]`.
pub mod reg3255 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[151]",
        3255,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3255, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3255, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3255, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3255, 24, 8, false, true, &[]);
}

/// Register `reg3256` at 0x32e0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[152]`.
pub mod reg3256 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[152]",
        3256,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3256, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3256, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3256, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3256, 24, 8, false, true, &[]);
}

/// Register `reg3257` at 0x32e4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[153]`.
pub mod reg3257 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[153]",
        3257,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3257, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3257, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3257, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3257, 24, 8, false, true, &[]);
}

/// Register `reg3258` at 0x32e8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[154]`.
pub mod reg3258 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[154]",
        3258,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3258, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3258, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3258, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3258, 24, 8, false, true, &[]);
}

/// Register `reg3259` at 0x32ec: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[155]`.
pub mod reg3259 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[155]",
        3259,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3259, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3259, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3259, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3259, 24, 8, false, true, &[]);
}

/// Register `reg3260` at 0x32f0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[156]`.
pub mod reg3260 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[156]",
        3260,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3260, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3260, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3260, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3260, 24, 8, false, true, &[]);
}

/// Register `reg3261` at 0x32f4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[157]`.
pub mod reg3261 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[157]",
        3261,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3261, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3261, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3261, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3261, 24, 8, false, true, &[]);
}

/// Register `reg3262` at 0x32f8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[158]`.
pub mod reg3262 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[158]",
        3262,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3262, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3262, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3262, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3262, 24, 8, false, true, &[]);
}

/// Register `reg3263` at 0x32fc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[159]`.
pub mod reg3263 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[159]",
        3263,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3263, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3263, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3263, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3263, 24, 8, false, true, &[]);
}

/// Register `reg3264` at 0x3300: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[160]`.
pub mod reg3264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[160]",
        3264,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3264, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3264, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3264, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3264, 24, 8, false, true, &[]);
}

/// Register `reg3265` at 0x3304: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[161]`.
pub mod reg3265 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[161]",
        3265,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3265, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3265, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3265, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3265, 24, 8, false, true, &[]);
}

/// Register `reg3266` at 0x3308: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[162]`.
pub mod reg3266 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[162]",
        3266,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3266, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3266, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3266, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3266, 24, 8, false, true, &[]);
}

/// Register `reg3267` at 0x330c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[163]`.
pub mod reg3267 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[163]",
        3267,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3267, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3267, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3267, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3267, 24, 8, false, true, &[]);
}

/// Register `reg3268` at 0x3310: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[164]`.
pub mod reg3268 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[164]",
        3268,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3268, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3268, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3268, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3268, 24, 8, false, true, &[]);
}

/// Register `reg3269` at 0x3314: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[165]`.
pub mod reg3269 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[165]",
        3269,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3269, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3269, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3269, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3269, 24, 8, false, true, &[]);
}

/// Register `reg3270` at 0x3318: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[166]`.
pub mod reg3270 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[166]",
        3270,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3270, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3270, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3270, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3270, 24, 8, false, true, &[]);
}

/// Register `reg3271` at 0x331c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[167]`.
pub mod reg3271 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[167]",
        3271,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3271, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3271, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3271, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3271, 24, 8, false, true, &[]);
}

/// Register `reg3272` at 0x3320: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[168]`.
pub mod reg3272 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[168]",
        3272,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3272, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3272, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3272, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3272, 24, 8, false, true, &[]);
}

/// Register `reg3273` at 0x3324: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[169]`.
pub mod reg3273 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[169]",
        3273,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3273, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3273, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3273, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3273, 24, 8, false, true, &[]);
}

/// Register `reg3274` at 0x3328: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[170]`.
pub mod reg3274 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[170]",
        3274,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3274, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3274, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3274, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3274, 24, 8, false, true, &[]);
}

/// Register `reg3275` at 0x332c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[171]`.
pub mod reg3275 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[171]",
        3275,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3275, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3275, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3275, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3275, 24, 8, false, true, &[]);
}

/// Register `reg3276` at 0x3330: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[172]`.
pub mod reg3276 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[172]",
        3276,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3276, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3276, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3276, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3276, 24, 8, false, true, &[]);
}

/// Register `reg3277` at 0x3334: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[173]`.
pub mod reg3277 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[173]",
        3277,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3277, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3277, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3277, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3277, 24, 8, false, true, &[]);
}

/// Register `reg3278` at 0x3338: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[174]`.
pub mod reg3278 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[174]",
        3278,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3278, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3278, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3278, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3278, 24, 8, false, true, &[]);
}

/// Register `reg3279` at 0x333c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[175]`.
pub mod reg3279 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[175]",
        3279,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3279, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3279, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3279, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3279, 24, 8, false, true, &[]);
}

/// Register `reg3280` at 0x3340: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[176]`.
pub mod reg3280 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[176]",
        3280,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3280, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3280, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3280, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3280, 24, 8, false, true, &[]);
}

/// Register `reg3281` at 0x3344: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[177]`.
pub mod reg3281 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[177]",
        3281,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3281, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3281, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3281, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3281, 24, 8, false, true, &[]);
}

/// Register `reg3282` at 0x3348: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[178]`.
pub mod reg3282 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[178]",
        3282,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3282, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3282, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3282, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3282, 24, 8, false, true, &[]);
}

/// Register `reg3283` at 0x334c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[179]`.
pub mod reg3283 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[179]",
        3283,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3283, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3283, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3283, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3283, 24, 8, false, true, &[]);
}

/// Register `reg3284` at 0x3350: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[180]`.
pub mod reg3284 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[180]",
        3284,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3284, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3284, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3284, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3284, 24, 8, false, true, &[]);
}

/// Register `reg3285` at 0x3354: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[181]`.
pub mod reg3285 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[181]",
        3285,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3285, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3285, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3285, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3285, 24, 8, false, true, &[]);
}

/// Register `reg3286` at 0x3358: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[182]`.
pub mod reg3286 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[182]",
        3286,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3286, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3286, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3286, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3286, 24, 8, false, true, &[]);
}

/// Register `reg3287` at 0x335c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[183]`.
pub mod reg3287 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[183]",
        3287,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3287, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3287, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3287, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3287, 24, 8, false, true, &[]);
}

/// Register `reg3288` at 0x3360: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[184]`.
pub mod reg3288 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[184]",
        3288,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3288, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3288, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3288, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3288, 24, 8, false, true, &[]);
}

/// Register `reg3289` at 0x3364: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[185]`.
pub mod reg3289 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[185]",
        3289,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3289, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3289, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3289, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3289, 24, 8, false, true, &[]);
}

/// Register `reg3290` at 0x3368: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[186]`.
pub mod reg3290 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[186]",
        3290,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3290, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3290, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3290, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3290, 24, 8, false, true, &[]);
}

/// Register `reg3291` at 0x336c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[187]`.
pub mod reg3291 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[187]",
        3291,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3291, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3291, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3291, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3291, 24, 8, false, true, &[]);
}

/// Register `reg3292` at 0x3370: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[188]`.
pub mod reg3292 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[188]",
        3292,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3292, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3292, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3292, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3292, 24, 8, false, true, &[]);
}

/// Register `reg3293` at 0x3374: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[189]`.
pub mod reg3293 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[189]",
        3293,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3293, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3293, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3293, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3293, 24, 8, false, true, &[]);
}

/// Register `reg3294` at 0x3378: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[190]`.
pub mod reg3294 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[190]",
        3294,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3294, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3294, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3294, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3294, 24, 8, false, true, &[]);
}

/// Register `reg3295` at 0x337c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[191]`.
pub mod reg3295 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[191]",
        3295,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3295, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3295, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3295, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3295, 24, 8, false, true, &[]);
}

/// Register `reg3296` at 0x3380: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[192]`.
pub mod reg3296 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[192]",
        3296,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3296, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3296, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3296, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3296, 24, 8, false, true, &[]);
}

/// Register `reg3297` at 0x3384: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[193]`.
pub mod reg3297 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[193]",
        3297,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3297, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3297, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3297, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3297, 24, 8, false, true, &[]);
}

/// Register `reg3298` at 0x3388: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[194]`.
pub mod reg3298 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[194]",
        3298,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3298, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3298, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3298, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3298, 24, 8, false, true, &[]);
}

/// Register `reg3299` at 0x338c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[195]`.
pub mod reg3299 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[195]",
        3299,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3299, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3299, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3299, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3299, 24, 8, false, true, &[]);
}

/// Register `reg3300` at 0x3390: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[196]`.
pub mod reg3300 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[196]",
        3300,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3300, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3300, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3300, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3300, 24, 8, false, true, &[]);
}

/// Register `reg3301` at 0x3394: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[197]`.
pub mod reg3301 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[197]",
        3301,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3301, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3301, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3301, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3301, 24, 8, false, true, &[]);
}

/// Register `reg3302` at 0x3398: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[198]`.
pub mod reg3302 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[198]",
        3302,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3302, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3302, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3302, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3302, 24, 8, false, true, &[]);
}

/// Register `reg3303` at 0x339c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[199]`.
pub mod reg3303 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[199]",
        3303,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3303, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3303, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3303, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3303, 24, 8, false, true, &[]);
}

/// Register `reg3304` at 0x33a0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[200]`.
pub mod reg3304 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[200]",
        3304,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3304, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3304, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3304, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3304, 24, 8, false, true, &[]);
}

/// Register `reg3305` at 0x33a4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[201]`.
pub mod reg3305 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[201]",
        3305,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3305, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3305, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3305, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3305, 24, 8, false, true, &[]);
}

/// Register `reg3306` at 0x33a8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[202]`.
pub mod reg3306 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[202]",
        3306,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3306, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3306, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3306, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3306, 24, 8, false, true, &[]);
}

/// Register `reg3307` at 0x33ac: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[203]`.
pub mod reg3307 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[203]",
        3307,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3307, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3307, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3307, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3307, 24, 8, false, true, &[]);
}

/// Register `reg3308` at 0x33b0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[204]`.
pub mod reg3308 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[204]",
        3308,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3308, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3308, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3308, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3308, 24, 8, false, true, &[]);
}

/// Register `reg3309` at 0x33b4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[205]`.
pub mod reg3309 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[205]",
        3309,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3309, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3309, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3309, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3309, 24, 8, false, true, &[]);
}

/// Register `reg3310` at 0x33b8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[206]`.
pub mod reg3310 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[206]",
        3310,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3310, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3310, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3310, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3310, 24, 8, false, true, &[]);
}

/// Register `reg3311` at 0x33bc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[207]`.
pub mod reg3311 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[207]",
        3311,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3311, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3311, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3311, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3311, 24, 8, false, true, &[]);
}

/// Register `reg3312` at 0x33c0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[208]`.
pub mod reg3312 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[208]",
        3312,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3312, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3312, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3312, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3312, 24, 8, false, true, &[]);
}

/// Register `reg3313` at 0x33c4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[209]`.
pub mod reg3313 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[209]",
        3313,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3313, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3313, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3313, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3313, 24, 8, false, true, &[]);
}

/// Register `reg3314` at 0x33c8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[210]`.
pub mod reg3314 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[210]",
        3314,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3314, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3314, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3314, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3314, 24, 8, false, true, &[]);
}

/// Register `reg3315` at 0x33cc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[211]`.
pub mod reg3315 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[211]",
        3315,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3315, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3315, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3315, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3315, 24, 8, false, true, &[]);
}

/// Register `reg3316` at 0x33d0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[212]`.
pub mod reg3316 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[212]",
        3316,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3316, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3316, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3316, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3316, 24, 8, false, true, &[]);
}

/// Register `reg3317` at 0x33d4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[213]`.
pub mod reg3317 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[213]",
        3317,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3317, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3317, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3317, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3317, 24, 8, false, true, &[]);
}

/// Register `reg3318` at 0x33d8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[214]`.
pub mod reg3318 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[214]",
        3318,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3318, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3318, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3318, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3318, 24, 8, false, true, &[]);
}

/// Register `reg3319` at 0x33dc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[215]`.
pub mod reg3319 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[215]",
        3319,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3319, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3319, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3319, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3319, 24, 8, false, true, &[]);
}

/// Register `reg3320` at 0x33e0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[216]`.
pub mod reg3320 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[216]",
        3320,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3320, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3320, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3320, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3320, 24, 8, false, true, &[]);
}

/// Register `reg3321` at 0x33e4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[217]`.
pub mod reg3321 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[217]",
        3321,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3321, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3321, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3321, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3321, 24, 8, false, true, &[]);
}

/// Register `reg3322` at 0x33e8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[218]`.
pub mod reg3322 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[218]",
        3322,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3322, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3322, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3322, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3322, 24, 8, false, true, &[]);
}

/// Register `reg3323` at 0x33ec: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[219]`.
pub mod reg3323 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[219]",
        3323,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3323, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3323, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3323, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3323, 24, 8, false, true, &[]);
}

/// Register `reg3324` at 0x33f0: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[220]`.
pub mod reg3324 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[220]",
        3324,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3324, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3324, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3324, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3324, 24, 8, false, true, &[]);
}

/// Register `reg3325` at 0x33f4: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[221]`.
pub mod reg3325 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[221]",
        3325,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3325, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3325, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3325, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3325, 24, 8, false, true, &[]);
}

/// Register `reg3326` at 0x33f8: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[222]`.
pub mod reg3326 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[222]",
        3326,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3326, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3326, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3326, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3326, 24, 8, false, true, &[]);
}

/// Register `reg3327` at 0x33fc: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[223]`.
pub mod reg3327 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[223]",
        3327,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3327, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3327, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3327, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3327, 24, 8, false, true, &[]);
}

/// Register `reg3328` at 0x3400: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[224]`.
pub mod reg3328 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[224]",
        3328,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3328, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3328, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3328, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3328, 24, 8, false, true, &[]);
}

/// Register `reg3329` at 0x3404: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[225]`.
pub mod reg3329 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[225]",
        3329,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3329, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3329, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3329, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3329, 24, 8, false, true, &[]);
}

/// Register `reg3330` at 0x3408: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[226]`.
pub mod reg3330 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[226]",
        3330,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3330, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3330, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3330, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3330, 24, 8, false, true, &[]);
}

/// Register `reg3331` at 0x340c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[227]`.
pub mod reg3331 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[227]",
        3331,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3331, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3331, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3331, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3331, 24, 8, false, true, &[]);
}

/// Register `reg3332` at 0x3410: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[228]`.
pub mod reg3332 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[228]",
        3332,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3332, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3332, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3332, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3332, 24, 8, false, true, &[]);
}

/// Register `reg3333` at 0x3414: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[229]`.
pub mod reg3333 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[229]",
        3333,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3333, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3333, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3333, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3333, 24, 8, false, true, &[]);
}

/// Register `reg3334` at 0x3418: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[230]`.
pub mod reg3334 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[230]",
        3334,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3334, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3334, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3334, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3334, 24, 8, false, true, &[]);
}

/// Register `reg3335` at 0x341c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[231]`.
pub mod reg3335 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[231]",
        3335,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3335, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3335, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3335, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3335, 24, 8, false, true, &[]);
}

/// Register `reg3336` at 0x3420: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[232]`.
pub mod reg3336 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[232]",
        3336,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3336, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3336, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3336, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3336, 24, 8, false, true, &[]);
}

/// Register `reg3337` at 0x3424: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[233]`.
pub mod reg3337 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[233]",
        3337,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3337, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3337, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3337, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3337, 24, 8, false, true, &[]);
}

/// Register `reg3338` at 0x3428: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[234]`.
pub mod reg3338 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[234]",
        3338,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3338, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3338, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3338, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3338, 24, 8, false, true, &[]);
}

/// Register `reg3339` at 0x342c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[235]`.
pub mod reg3339 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[235]",
        3339,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3339, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3339, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3339, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3339, 24, 8, false, true, &[]);
}

/// Register `reg3340` at 0x3430: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[236]`.
pub mod reg3340 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[236]",
        3340,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3340, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3340, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3340, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3340, 24, 8, false, true, &[]);
}

/// Register `reg3341` at 0x3434: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[237]`.
pub mod reg3341 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[237]",
        3341,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3341, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3341, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3341, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3341, 24, 8, false, true, &[]);
}

/// Register `reg3342` at 0x3438: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[238]`.
pub mod reg3342 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[238]",
        3342,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3342, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3342, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3342, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3342, 24, 8, false, true, &[]);
}

/// Register `reg3343` at 0x343c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[239]`.
pub mod reg3343 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[239]",
        3343,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3343, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3343, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3343, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3343, 24, 8, false, true, &[]);
}

/// Register `reg3344` at 0x3440: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[240]`.
pub mod reg3344 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[240]",
        3344,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3344, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3344, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3344, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3344, 24, 8, false, true, &[]);
}

/// Register `reg3345` at 0x3444: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[241]`.
pub mod reg3345 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[241]",
        3345,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3345, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3345, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3345, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3345, 24, 8, false, true, &[]);
}

/// Register `reg3346` at 0x3448: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[242]`.
pub mod reg3346 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[242]",
        3346,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3346, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3346, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3346, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3346, 24, 8, false, true, &[]);
}

/// Register `reg3347` at 0x344c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[243]`.
pub mod reg3347 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[243]",
        3347,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3347, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3347, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3347, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3347, 24, 8, false, true, &[]);
}

/// Register `reg3348` at 0x3450: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[244]`.
pub mod reg3348 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[244]",
        3348,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3348, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3348, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3348, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3348, 24, 8, false, true, &[]);
}

/// Register `reg3349` at 0x3454: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[245]`.
pub mod reg3349 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[245]",
        3349,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3349, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3349, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3349, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3349, 24, 8, false, true, &[]);
}

/// Register `reg3350` at 0x3458: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[246]`.
pub mod reg3350 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[246]",
        3350,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3350, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3350, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3350, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3350, 24, 8, false, true, &[]);
}

/// Register `reg3351` at 0x345c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[247]`.
pub mod reg3351 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[247]",
        3351,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3351, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3351, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3351, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3351, 24, 8, false, true, &[]);
}

/// Register `reg3352` at 0x3460: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[248]`.
pub mod reg3352 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[248]",
        3352,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3352, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3352, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3352, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3352, 24, 8, false, true, &[]);
}

/// Register `reg3353` at 0x3464: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[249]`.
pub mod reg3353 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[249]",
        3353,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3353, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3353, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3353, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3353, 24, 8, false, true, &[]);
}

/// Register `reg3354` at 0x3468: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[250]`.
pub mod reg3354 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[250]",
        3354,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3354, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3354, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3354, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3354, 24, 8, false, true, &[]);
}

/// Register `reg3355` at 0x346c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[251]`.
pub mod reg3355 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[251]",
        3355,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3355, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3355, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3355, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3355, 24, 8, false, true, &[]);
}

/// Register `reg3356` at 0x3470: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[252]`.
pub mod reg3356 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[252]",
        3356,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3356, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3356, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3356, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3356, 24, 8, false, true, &[]);
}

/// Register `reg3357` at 0x3474: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[253]`.
pub mod reg3357 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[253]",
        3357,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3357, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3357, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3357, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3357, 24, 8, false, true, &[]);
}

/// Register `reg3358` at 0x3478: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[254]`.
pub mod reg3358 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[254]",
        3358,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3358, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3358, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3358, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3358, 24, 8, false, true, &[]);
}

/// Register `reg3359` at 0x347c: 1 word, ReadWrite.
///
/// The C path is `reg_osd.plt_data[255]`.
pub mod reg3359 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(6),
        "reg_osd.plt_data[255]",
        3359,
        1,
        Access::ReadWrite,
        1,
    );
    pub const V: Field = Field::new(Region::Class(6), 3359, 0, 8, false, true, &[]);
    pub const U: Field = Field::new(Region::Class(6), 3359, 8, 8, false, true, &[]);
    pub const Y: Field = Field::new(Region::Class(6), 3359, 16, 8, false, true, &[]);
    pub const ALPHA: Field = Field::new(Region::Class(6), 3359, 24, 8, false, true, &[]);
}
