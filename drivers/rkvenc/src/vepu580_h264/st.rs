//! The VEPU580 H.264 ST (kernel class 7): the status registers, read only (reg_st).
//!
//! Words 4096 to 4275 of the register block; read only. Generated from the vendor header; see [`super`].

/// Register `reg4096` at 0x4000: 1 word, ReadOnly.
///
/// The C path is `reg_st.bs_lgth_l32`.
pub mod reg4096 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.bs_lgth_l32",
        4096,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4097` at 0x4004: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_sse_bsl`.
pub mod reg4097 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_sse_bsl",
        4097,
        1,
        Access::ReadOnly,
        1,
    );
    pub const BS_LGTH_H8: Field = Field::new(Region::Class(7), 4097, 0, 8, false, false, &[]);
    pub const SSE_L16: Field = Field::new(Region::Class(7), 4097, 16, 16, false, false, &[]);
}

/// Register `reg4098` at 0x4008: 1 word, ReadOnly.
///
/// The C path is `reg_st.sse_h32`.
pub mod reg4098 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.sse_h32",
        4098,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4099` at 0x400c: 1 word, ReadOnly.
///
/// The C path is `reg_st.qp_sum`.
pub mod reg4099 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.qp_sum",
        4099,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4100` at 0x4010: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_sao`.
pub mod reg4100 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_sao",
        4100,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SAO_CNUM: Field = Field::new(Region::Class(7), 4100, 0, 16, false, false, &[]);
    pub const SAO_YNUM: Field = Field::new(Region::Class(7), 4100, 16, 16, false, false, &[]);
}

/// Register `reg4101` at 0x4014: 1 word, ReadOnly.
///
/// The C path is `reg_st.rdo_head_bits`.
pub mod reg4101 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.rdo_head_bits",
        4101,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4102` at 0x4018: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_head_res_bl`.
pub mod reg4102 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_head_res_bl",
        4102,
        1,
        Access::ReadOnly,
        1,
    );
    pub const RDO_HEAD_BITS_H8: Field = Field::new(Region::Class(7), 4102, 0, 8, false, false, &[]);
    pub const RDO_RES_BITS_L16: Field =
        Field::new(Region::Class(7), 4102, 16, 16, false, false, &[]);
}

/// Register `reg4103` at 0x401c: 1 word, ReadOnly.
///
/// The C path is `reg_st.rdo_res_bits_h24`.
pub mod reg4103 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.rdo_res_bits_h24",
        4103,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4104` at 0x4020: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_enc`.
pub mod reg4104 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_enc",
        4104,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_ENC: Field = Field::new(Region::Class(7), 4104, 0, 2, false, false, &[]);
    pub const ST_SCLR: Field = Field::new(Region::Class(7), 4104, 2, 1, false, false, &[]);
}

/// Register `reg4105` at 0x4024: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_lkt`.
pub mod reg4105 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_lkt",
        4105,
        1,
        Access::ReadOnly,
        1,
    );
    pub const FNUM_CFG_DONE: Field = Field::new(Region::Class(7), 4105, 0, 8, false, false, &[]);
    pub const FNUM_CFG: Field = Field::new(Region::Class(7), 4105, 8, 8, false, false, &[]);
    pub const FNUM_INT: Field = Field::new(Region::Class(7), 4105, 16, 8, false, false, &[]);
    pub const FNUM_ENC_DONE: Field = Field::new(Region::Class(7), 4105, 24, 8, false, false, &[]);
}

/// Register `reg4106` at 0x4028: 1 word, ReadOnly.
///
/// The C path is `reg_st.node_addr`.
pub mod reg4106 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.node_addr",
        4106,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4107` at 0x402c: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_bsb`.
pub mod reg4107 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_bsb",
        4107,
        1,
        Access::ReadOnly,
        1,
    );
    pub const BSBW_OVFL: Field = Field::new(Region::Class(7), 4107, 0, 1, false, false, &[]);
    pub const BSBW_ADDR: Field = Field::new(Region::Class(7), 4107, 3, 28, false, false, &[]);
}

/// Register `reg4108` at 0x4030: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_bus`.
pub mod reg4108 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_bus",
        4108,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIB_IDL: Field = Field::new(Region::Class(7), 4108, 0, 8, false, false, &[]);
    pub const AXIB_OVFL: Field = Field::new(Region::Class(7), 4108, 8, 8, false, false, &[]);
    pub const AXIB_ERR: Field = Field::new(Region::Class(7), 4108, 16, 8, false, false, &[]);
    pub const AXIR_ERR: Field = Field::new(Region::Class(7), 4108, 24, 7, false, false, &[]);
}

/// Register `reg4109` at 0x4034: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_snum`.
pub mod reg4109 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_snum",
        4109,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SLI_NUM: Field = Field::new(Region::Class(7), 4109, 0, 6, false, false, &[]);
}

/// Register `reg4110` at 0x4038: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_slen`.
pub mod reg4110 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_slen",
        4110,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SLI_LEN: Field = Field::new(Region::Class(7), 4110, 0, 25, false, false, &[]);
}

/// Register `reg4160` at 0x4100: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_p64`.
pub mod reg4160 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_p64",
        4160,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_P64: Field = Field::new(Region::Class(7), 4160, 0, 17, false, false, &[]);
}

/// Register `reg4161` at 0x4104: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_p32`.
pub mod reg4161 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_p32",
        4161,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_P32: Field = Field::new(Region::Class(7), 4161, 0, 19, false, false, &[]);
}

/// Register `reg4162` at 0x4108: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_p16`.
pub mod reg4162 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_p16",
        4162,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_P16: Field = Field::new(Region::Class(7), 4162, 0, 21, false, false, &[]);
}

/// Register `reg4163` at 0x410c: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_p8`.
pub mod reg4163 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_p8",
        4163,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_P8: Field = Field::new(Region::Class(7), 4163, 0, 23, false, false, &[]);
}

/// Register `reg4164` at 0x4110: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_i32`.
pub mod reg4164 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_i32",
        4164,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_I32: Field = Field::new(Region::Class(7), 4164, 0, 19, false, false, &[]);
}

/// Register `reg4165` at 0x4114: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_i16`.
pub mod reg4165 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_i16",
        4165,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_I16: Field = Field::new(Region::Class(7), 4165, 0, 21, false, false, &[]);
}

/// Register `reg4166` at 0x4118: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_i8`.
pub mod reg4166 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_i8",
        4166,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_I8: Field = Field::new(Region::Class(7), 4166, 0, 23, false, false, &[]);
}

/// Register `reg4167` at 0x411c: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_pnum_i4`.
pub mod reg4167 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_pnum_i4",
        4167,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PNUM_I4: Field = Field::new(Region::Class(7), 4167, 0, 23, false, false, &[]);
}

/// Register `reg4168` at 0x4120: 1 word, ReadOnly.
///
/// The C path is `reg_st.madp`.
pub mod reg4168 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madp",
        4168,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4169` at 0x4124: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_bnum_cme`.
pub mod reg4169 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_bnum_cme",
        4169,
        1,
        Access::ReadOnly,
        1,
    );
    pub const NUM_CTU: Field = Field::new(Region::Class(7), 4169, 0, 21, false, false, &[]);
}

/// Register `reg4170` at 0x4128: 1 word, ReadOnly.
///
/// The C path is `reg_st.madi`.
pub mod reg4170 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madi",
        4170,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4171` at 0x412c: 1 word, ReadOnly.
///
/// The C path is `reg_st.st_bnum_b16`.
pub mod reg4171 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_bnum_b16",
        4171,
        1,
        Access::ReadOnly,
        1,
    );
    pub const NUM_B16: Field = Field::new(Region::Class(7), 4171, 0, 23, false, false, &[]);
}

/// Register `reg4172` at 0x4130: 1 word, ReadOnly.
///
/// The C path is `reg_st.num_madi_max_b16`.
pub mod reg4172 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.num_madi_max_b16",
        4172,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4173` at 0x4134: 1 word, ReadOnly.
///
/// The C path is `reg_st.md_sad_b16num0`.
pub mod reg4173 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.md_sad_b16num0",
        4173,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4174` at 0x4138: 1 word, ReadOnly.
///
/// The C path is `reg_st.md_sad_b16num1`.
pub mod reg4174 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.md_sad_b16num1",
        4174,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4175` at 0x413c: 1 word, ReadOnly.
///
/// The C path is `reg_st.md_sad_b16num2`.
pub mod reg4175 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.md_sad_b16num2",
        4175,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4176` at 0x4140: 1 word, ReadOnly.
///
/// The C path is `reg_st.md_sad_b16num3`.
pub mod reg4176 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.md_sad_b16num3",
        4176,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4177` at 0x4144: 1 word, ReadOnly.
///
/// The C path is `reg_st.madi_b16num0`.
pub mod reg4177 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madi_b16num0",
        4177,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4178` at 0x4148: 1 word, ReadOnly.
///
/// The C path is `reg_st.madi_b16num1`.
pub mod reg4178 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madi_b16num1",
        4178,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4179` at 0x414c: 1 word, ReadOnly.
///
/// The C path is `reg_st.madi_b16num2`.
pub mod reg4179 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madi_b16num2",
        4179,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4180` at 0x4150: 1 word, ReadOnly.
///
/// The C path is `reg_st.madi_b16num3`.
pub mod reg4180 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.madi_b16num3",
        4180,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg4224` at 0x4200: 52 words, ReadOnly.
///
/// The C path is `reg_st.st_b8_qp`. An array of 52 RK_U32 entries, one per word.
pub mod reg4224 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(7),
        "reg_st.st_b8_qp",
        4224,
        52,
        Access::ReadOnly,
        1,
    );
}
