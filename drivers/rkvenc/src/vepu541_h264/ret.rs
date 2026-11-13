//! The VEPU541 H.264 read-back side: status, the stream length and the statistics a finished job reports.

#![allow(clippy::unreadable_literal)]

/// Register `hw_status` at 0x001c: 1 word, WriteOneToClear.
pub mod hw_status {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "hw_status", 7, 1, Access::WriteOneToClear, 1);
    pub const ENC_DONE_STA: Field = Field::new(Region::Ret, 7, 0, 1, false, true, &[]);
    pub const LKT_DONE_STA: Field = Field::new(Region::Ret, 7, 1, 1, false, true, &[]);
    pub const SCLR_DONE_STA: Field = Field::new(Region::Ret, 7, 2, 1, false, true, &[]);
    pub const ENC_SLICE_DONE_STA: Field = Field::new(Region::Ret, 7, 3, 1, false, true, &[]);
    pub const OFLW_DONE_STA: Field = Field::new(Region::Ret, 7, 4, 1, false, true, &[]);
    pub const BRSP_DONE_STA: Field = Field::new(Region::Ret, 7, 5, 1, false, true, &[]);
    pub const BERR_DONE_STA: Field = Field::new(Region::Ret, 7, 6, 1, false, true, &[]);
    pub const RERR_DONE_STA: Field = Field::new(Region::Ret, 7, 7, 1, false, true, &[]);
    pub const WDG_DONE_STA: Field = Field::new(Region::Ret, 7, 8, 1, false, true, &[]);
}

/// Register `st_bsl` at 0x0210: 1 word, ReadOnly.
pub mod st_bsl {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_bsl", 132, 1, Access::ReadOnly, 1);
    pub const BS_LGTH: Field = Field::new(Region::Ret, 132, 0, 27, false, false, &[]);
}

/// Register `st_sse_l32` at 0x0214: 1 word, ReadOnly.
pub mod st_sse_l32 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_sse_l32", 133, 1, Access::ReadOnly, 1);
    pub const SSE_L32: Field = Field::new(Region::Ret, 133, 0, 32, false, false, &[]);
}

/// Register `st_sse_qp` at 0x0218: 1 word, ReadOnly.
pub mod st_sse_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_sse_qp", 134, 1, Access::ReadOnly, 16777216);
    pub const QP_SUM: Field = Field::new(Region::Ret, 134, 0, 22, false, false, &[]);
    pub const SSE_H8: Field = Field::new(Region::Ret, 134, 24, 8, false, false, &[]);
}

/// Register `st_sao` at 0x021c: 1 word, ReadOnly.
pub mod st_sao {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_sao", 135, 1, Access::ReadOnly, 1);
    pub const SAO_CNUM: Field = Field::new(Region::Ret, 135, 0, 12, false, false, &[]);
    pub const SAO_YNUM: Field = Field::new(Region::Ret, 135, 12, 12, false, false, &[]);
}

/// Register `reg_136_137` at 0x0220: 2 words, Unknown.
pub mod reg_136_137 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "reg_136_137", 136, 2, Access::Unknown, 1);
    pub const REG_136_137: [Field; 2] = [
        Field::new(Region::Ret, 136, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 137, 0, 32, false, false, &[]),
    ];
}

/// Register `st_enc` at 0x0228: 1 word, ReadOnly.
pub mod st_enc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_enc", 138, 1, Access::ReadOnly, 1);
    pub const ST_ENC: Field = Field::new(Region::Ret, 138, 0, 2, false, false, &[]);
    pub const ST_SCLR: Field = Field::new(Region::Ret, 138, 2, 1, false, false, &[]);
}

/// Register `st_lkt` at 0x022c: 1 word, ReadOnly.
pub mod st_lkt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_lkt", 139, 1, Access::ReadOnly, 1);
    pub const FNUM_ENC: Field = Field::new(Region::Ret, 139, 0, 8, false, false, &[]);
    pub const FNUM_CFG: Field = Field::new(Region::Ret, 139, 8, 8, false, false, &[]);
    pub const FNUM_INT: Field = Field::new(Region::Ret, 139, 16, 8, false, false, &[]);
}

/// Register `ST_NADR` at 0x0230: 1 word, ReadOnly.
pub mod st_nadr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "ST_NADR", 140, 1, Access::ReadOnly, 1);
    pub const NODE_ADDR: Field = Field::new(Region::Ret, 140, 0, 32, false, false, &[]);
}

/// Register `st_bsb` at 0x0234: 1 word, ReadOnly.
pub mod st_bsb {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_bsb", 141, 1, Access::ReadOnly, 1);
    pub const BSBW_ADDR: Field = Field::new(Region::Ret, 141, 0, 32, false, false, &[]);
}

/// Register `st_bus` at 0x0238: 1 word, ReadOnly.
pub mod st_bus {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_bus", 142, 1, Access::ReadOnly, 1);
    pub const AXIB_IDL: Field = Field::new(Region::Ret, 142, 0, 7, false, false, &[]);
    pub const AXIB_OVFL: Field = Field::new(Region::Ret, 142, 7, 7, false, false, &[]);
    pub const AXIB_ERR: Field = Field::new(Region::Ret, 142, 14, 7, false, false, &[]);
    pub const AXIR_ERR: Field = Field::new(Region::Ret, 142, 21, 6, false, false, &[]);
}

/// Register `st_slice_number` at 0x023c: 1 word, ReadOnly.
pub mod st_slice_number {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_slice_number", 143, 1, Access::ReadOnly, 1);
    pub const ST_SLICE_NUMBER: Field = Field::new(Region::Ret, 143, 0, 32, false, false, &[]);
}

/// Register `st_slice_length` at 0x0240: 1 word, ReadOnly.
pub mod st_slice_length {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_slice_length", 144, 1, Access::ReadOnly, 1);
    pub const ST_SLICE_LENGTH: Field = Field::new(Region::Ret, 144, 0, 32, false, false, &[]);
}

/// Register `st_lvl64_inter_num` at 0x0244: 1 word, ReadOnly.
pub mod st_lvl64_inter_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl64_inter_num",
        145,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL64_INTER_NUM: Field = Field::new(Region::Ret, 145, 0, 32, false, false, &[]);
}

/// Register `st_lvl32_inter_num` at 0x0248: 1 word, ReadOnly.
pub mod st_lvl32_inter_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl32_inter_num",
        146,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL32_INTER_NUM: Field = Field::new(Region::Ret, 146, 0, 32, false, false, &[]);
}

/// Register `st_lvl16_inter_num` at 0x024c: 1 word, ReadOnly.
pub mod st_lvl16_inter_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl16_inter_num",
        147,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL16_INTER_NUM: Field = Field::new(Region::Ret, 147, 0, 32, false, false, &[]);
}

/// Register `st_lvl8_inter_num` at 0x0250: 1 word, ReadOnly.
pub mod st_lvl8_inter_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl8_inter_num",
        148,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL8_INTER_NUM: Field = Field::new(Region::Ret, 148, 0, 32, false, false, &[]);
}

/// Register `st_lvl32_intra_num` at 0x0254: 1 word, ReadOnly.
pub mod st_lvl32_intra_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl32_intra_num",
        149,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL32_INTRA_NUM: Field = Field::new(Region::Ret, 149, 0, 32, false, false, &[]);
}

/// Register `st_lvl16_intra_num` at 0x0258: 1 word, ReadOnly.
pub mod st_lvl16_intra_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl16_intra_num",
        150,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL16_INTRA_NUM: Field = Field::new(Region::Ret, 150, 0, 32, false, false, &[]);
}

/// Register `st_lvl8_intra_num` at 0x025c: 1 word, ReadOnly.
pub mod st_lvl8_intra_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl8_intra_num",
        151,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL8_INTRA_NUM: Field = Field::new(Region::Ret, 151, 0, 32, false, false, &[]);
}

/// Register `st_lvl4_intra_num` at 0x0260: 1 word, ReadOnly.
pub mod st_lvl4_intra_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Ret,
        "st_lvl4_intra_num",
        152,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ST_LVL4_INTRA_NUM: Field = Field::new(Region::Ret, 152, 0, 32, false, false, &[]);
}

/// Register `st_cu_num_qp` at 0x0264: 52 words, ReadOnly.
pub mod st_cu_num_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_cu_num_qp", 153, 52, Access::ReadOnly, 1);
    pub const ST_CU_NUM_QP: [Field; 52] = [
        Field::new(Region::Ret, 153, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 154, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 155, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 156, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 157, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 158, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 159, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 160, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 161, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 162, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 163, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 164, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 165, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 166, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 167, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 168, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 169, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 170, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 171, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 172, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 173, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 174, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 175, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 176, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 177, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 178, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 179, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 180, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 181, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 182, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 183, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 184, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 185, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 186, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 187, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 188, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 189, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 190, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 191, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 192, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 193, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 194, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 195, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 196, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 197, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 198, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 199, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 200, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 201, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 202, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 203, 0, 32, false, false, &[]),
        Field::new(Region::Ret, 204, 0, 32, false, false, &[]),
    ];
}

/// Register `st_madp` at 0x0334: 1 word, ReadOnly.
pub mod st_madp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_madp", 205, 1, Access::ReadOnly, 1);
    pub const ST_MADP: Field = Field::new(Region::Ret, 205, 0, 32, false, false, &[]);
}

/// Register `st_ctu_num` at 0x0338: 1 word, ReadOnly.
pub mod st_ctu_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_ctu_num", 206, 1, Access::ReadOnly, 1);
    pub const ST_CTU_NUM: Field = Field::new(Region::Ret, 206, 0, 32, false, false, &[]);
}

/// Register `st_madi` at 0x033c: 1 word, ReadOnly.
pub mod st_madi {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_madi", 207, 1, Access::ReadOnly, 1);
    pub const ST_MADI: Field = Field::new(Region::Ret, 207, 0, 32, false, false, &[]);
}

/// Register `st_mb_num` at 0x0340: 1 word, ReadOnly.
pub mod st_mb_num {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Ret, "st_mb_num", 208, 1, Access::ReadOnly, 1);
    pub const ST_MB_NUM: Field = Field::new(Region::Ret, 208, 0, 32, false, false, &[]);
}
