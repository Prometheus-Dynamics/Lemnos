//! The VEPU541 H.265 second window: the words a job sets with `RegisterSet::set_l2`, from
//! [`super::L2_FIRST`]. Offsets are the header's map: its first member is at 0x4.

#![allow(clippy::unreadable_literal)]

/// Register `lvl32_intra_cst_thd0` at 0x0004: 1 word, ReadWrite.
pub mod lvl32_intra_cst_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl32_intra_cst_thd0",
        1,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_THD0: Field = Field::new(Region::L2, 1, 0, 12, false, true, &[]);
    pub const LVL32_INTRA_CST_THD1: Field = Field::new(Region::L2, 1, 16, 12, false, true, &[]);
}

/// Register `lvl32_intra_cst_thd1` at 0x0008: 1 word, ReadWrite.
pub mod lvl32_intra_cst_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl32_intra_cst_thd1",
        2,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_THD2: Field = Field::new(Region::L2, 2, 0, 12, false, true, &[]);
    pub const LVL32_INTRA_CST_THD3: Field = Field::new(Region::L2, 2, 16, 12, false, true, &[]);
}

/// Register `lvl16_intra_cst_thd0` at 0x000c: 1 word, ReadWrite.
pub mod lvl16_intra_cst_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl16_intra_cst_thd0",
        3,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_THD0: Field = Field::new(Region::L2, 3, 0, 12, false, true, &[]);
    pub const LVL16_INTRA_CST_THD1: Field = Field::new(Region::L2, 3, 16, 12, false, true, &[]);
}

/// Register `lvl16_intra_cst_thd1` at 0x0010: 1 word, ReadWrite.
pub mod lvl16_intra_cst_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl16_intra_cst_thd1",
        4,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_THD2: Field = Field::new(Region::L2, 4, 0, 12, false, true, &[]);
    pub const LVL16_INTRA_CST_THD3: Field = Field::new(Region::L2, 4, 16, 12, false, true, &[]);
}

/// Register `lvl8_intra_cst_thd0` at 0x0014: 1 word, ReadWrite.
pub mod lvl8_intra_cst_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl8_intra_cst_thd0",
        5,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CST_THD0: Field = Field::new(Region::L2, 5, 0, 32, false, true, &[]);
}

/// Register `lvl8_intra_cst_thd1` at 0x0018: 1 word, ReadWrite.
pub mod lvl8_intra_cst_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl8_intra_cst_thd1",
        6,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CST_THD1: Field = Field::new(Region::L2, 6, 0, 32, false, true, &[]);
}

/// Register `lvl16_intra_ul_cst_thd` at 0x001c: 1 word, ReadWrite.
pub mod lvl16_intra_ul_cst_thd {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl16_intra_ul_cst_thd",
        7,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_UL_CST_THD: Field = Field::new(Region::L2, 7, 0, 32, false, true, &[]);
}

/// Register `lvl32_intra_cst_wgt0` at 0x0020: 1 word, ReadWrite.
pub mod lvl32_intra_cst_wgt0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl32_intra_cst_wgt0",
        8,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_WGT0: Field = Field::new(Region::L2, 8, 0, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT1: Field = Field::new(Region::L2, 8, 8, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT2: Field = Field::new(Region::L2, 8, 16, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT3: Field = Field::new(Region::L2, 8, 24, 8, false, true, &[]);
}

/// Register `lvl32_intra_cst_wgt1` at 0x0024: 1 word, ReadWrite.
pub mod lvl32_intra_cst_wgt1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl32_intra_cst_wgt1",
        9,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_WGT4: Field = Field::new(Region::L2, 9, 0, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT5: Field = Field::new(Region::L2, 9, 8, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT6: Field = Field::new(Region::L2, 9, 16, 8, false, true, &[]);
}

/// Register `lvl16_intra_cst_wgt0` at 0x0028: 1 word, ReadWrite.
pub mod lvl16_intra_cst_wgt0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl16_intra_cst_wgt0",
        10,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_WGT0: Field = Field::new(Region::L2, 10, 0, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT1: Field = Field::new(Region::L2, 10, 8, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT2: Field = Field::new(Region::L2, 10, 16, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT3: Field = Field::new(Region::L2, 10, 24, 8, false, true, &[]);
}

/// Register `lvl16_intra_cst_wgt1` at 0x002c: 1 word, ReadWrite.
pub mod lvl16_intra_cst_wgt1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "lvl16_intra_cst_wgt1",
        11,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_WGT4: Field = Field::new(Region::L2, 11, 0, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT5: Field = Field::new(Region::L2, 11, 8, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT6: Field = Field::new(Region::L2, 11, 16, 8, false, true, &[]);
}

/// Register `rdo_quant` at 0x0030: 1 word, ReadWrite.
pub mod rdo_quant {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "rdo_quant", 12, 1, Access::ReadWrite, 1);
    pub const QUANT_F_BIAS_I: Field = Field::new(Region::L2, 12, 0, 10, false, true, &[]);
    pub const QUANT_F_BIAS_P: Field = Field::new(Region::L2, 12, 10, 10, false, true, &[]);
}

/// Register `atr_thd0` at 0x0034: 1 word, ReadWrite.
pub mod atr_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_thd0", 13, 1, Access::ReadWrite, 1);
    pub const ATR_THD0: Field = Field::new(Region::L2, 13, 0, 12, false, true, &[]);
    pub const ATR_THD1: Field = Field::new(Region::L2, 13, 16, 12, false, true, &[]);
}

/// Register `atr_thd1` at 0x0038: 1 word, ReadWrite.
pub mod atr_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_thd1", 14, 1, Access::ReadWrite, 1);
    pub const ATR_THD2: Field = Field::new(Region::L2, 14, 0, 12, false, true, &[]);
    pub const ATR_THDQP: Field = Field::new(Region::L2, 14, 16, 6, false, true, &[]);
}

/// Register `lvl16_atr_wgt` at 0x003c: 1 word, ReadWrite.
pub mod lvl16_atr_wgt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lvl16_atr_wgt", 15, 1, Access::ReadWrite, 1);
    pub const LVL16_ATR_WGT0: Field = Field::new(Region::L2, 15, 0, 8, false, true, &[]);
    pub const LVL16_ATR_WGT1: Field = Field::new(Region::L2, 15, 8, 8, false, true, &[]);
    pub const LVL16_ATR_WGT2: Field = Field::new(Region::L2, 15, 16, 8, false, true, &[]);
}

/// Register `lvl8_atr_wgt` at 0x0040: 1 word, ReadWrite.
pub mod lvl8_atr_wgt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lvl8_atr_wgt", 16, 1, Access::ReadWrite, 1);
    pub const LVL8_ATR_WGT0: Field = Field::new(Region::L2, 16, 0, 8, false, true, &[]);
    pub const LVL8_ATR_WGT1: Field = Field::new(Region::L2, 16, 8, 8, false, true, &[]);
    pub const LVL8_ATR_WGT2: Field = Field::new(Region::L2, 16, 16, 8, false, true, &[]);
}

/// Register `lvl4_atr_wgt` at 0x0044: 1 word, ReadWrite.
pub mod lvl4_atr_wgt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lvl4_atr_wgt", 17, 1, Access::ReadWrite, 1);
    pub const LVL4_ATR_WGT0: Field = Field::new(Region::L2, 17, 0, 8, false, true, &[]);
    pub const LVL4_ATR_WGT1: Field = Field::new(Region::L2, 17, 8, 8, false, true, &[]);
    pub const LVL4_ATR_WGT2: Field = Field::new(Region::L2, 17, 16, 8, false, true, &[]);
}

/// Register `thd_541_atf_thd0` at 0x0048: 1 word, ReadWrite.
pub mod thd_541_atf_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "thd_541_atf_thd0", 18, 1, Access::ReadWrite, 1);
    pub const ATF_THD0_I32: Field = Field::new(Region::L2, 18, 0, 6, false, true, &[]);
    pub const ATF_THD1_I32: Field = Field::new(Region::L2, 18, 16, 6, false, true, &[]);
}

/// Register `thd_541_atf_thd1` at 0x004c: 1 word, ReadWrite.
pub mod thd_541_atf_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "thd_541_atf_thd1", 19, 1, Access::ReadWrite, 1);
    pub const ATF_THD0_I16: Field = Field::new(Region::L2, 19, 0, 6, false, true, &[]);
    pub const ATF_THD1_I16: Field = Field::new(Region::L2, 19, 16, 6, false, true, &[]);
}

/// Register `thd_541_atf_sad_thd0` at 0x0050: 1 word, ReadWrite.
pub mod thd_541_atf_sad_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "thd_541_atf_sad_thd0",
        20,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATF_THD0_P64: Field = Field::new(Region::L2, 20, 0, 6, false, true, &[]);
    pub const ATF_THD1_P64: Field = Field::new(Region::L2, 20, 16, 6, false, true, &[]);
}

/// Register `thd_541_atf_sad_thd1` at 0x0054: 1 word, ReadWrite.
pub mod thd_541_atf_sad_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "thd_541_atf_sad_thd1",
        21,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATF_THD0_P32: Field = Field::new(Region::L2, 21, 0, 6, false, true, &[]);
    pub const ATF_THD1_P32: Field = Field::new(Region::L2, 21, 16, 6, false, true, &[]);
}

/// Register `thd_541_atf_sad_wgt0` at 0x0058: 1 word, ReadWrite.
pub mod thd_541_atf_sad_wgt0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "thd_541_atf_sad_wgt0",
        22,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATF_THD0_P16: Field = Field::new(Region::L2, 22, 0, 6, false, true, &[]);
    pub const ATF_THD1_P16: Field = Field::new(Region::L2, 22, 16, 6, false, true, &[]);
}

/// Register `atf_sad_wgt1` at 0x005c: 1 word, ReadWrite.
pub mod atf_sad_wgt1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sad_wgt1", 23, 1, Access::ReadWrite, 1);
    pub const ATF_WGT_I16: Field = Field::new(Region::L2, 23, 0, 6, false, true, &[]);
    pub const ATF_WGT_I32: Field = Field::new(Region::L2, 23, 16, 6, false, true, &[]);
}

/// Register `atf_sad_wgt2` at 0x0060: 1 word, ReadWrite.
pub mod atf_sad_wgt2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sad_wgt2", 24, 1, Access::ReadWrite, 1);
    pub const ATF_WGT_P32: Field = Field::new(Region::L2, 24, 0, 6, false, true, &[]);
    pub const ATF_WGT_P64: Field = Field::new(Region::L2, 24, 16, 6, false, true, &[]);
}

/// Register `atf_sad_ofst0` at 0x0064: 1 word, ReadWrite.
pub mod atf_sad_ofst0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sad_ofst0", 25, 1, Access::ReadWrite, 1);
    pub const ATF_WGT_P16: Field = Field::new(Region::L2, 25, 0, 6, false, true, &[]);
}

/// Register `atf_sad_ofst1` at 0x0068: 1 word, ReadWrite.
pub mod atf_sad_ofst1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sad_ofst1", 26, 1, Access::ReadWrite, 1);
    pub const ATF_SAD_OFST12: Field = Field::new(Region::L2, 26, 0, 14, false, true, &[]);
    pub const ATF_SAD_OFST20: Field = Field::new(Region::L2, 26, 16, 14, false, true, &[]);
}

/// Register `atf_sad_ofst2` at 0x006c: 1 word, ReadWrite.
pub mod atf_sad_ofst2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sad_ofst2", 27, 1, Access::ReadWrite, 1);
    pub const ATF_SAD_OFST21: Field = Field::new(Region::L2, 27, 0, 14, false, true, &[]);
    pub const ATF_SAD_OFST30: Field = Field::new(Region::L2, 27, 16, 14, false, true, &[]);
}

/// Register `lamd_satd_qp` at 0x0070: 52 words, ReadWrite.
pub mod lamd_satd_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lamd_satd_qp", 28, 52, Access::ReadWrite, 1);
    pub const LAMD_SATD_QP: [Field; 52] = [
        Field::new(Region::L2, 28, 0, 32, false, true, &[]),
        Field::new(Region::L2, 29, 0, 32, false, true, &[]),
        Field::new(Region::L2, 30, 0, 32, false, true, &[]),
        Field::new(Region::L2, 31, 0, 32, false, true, &[]),
        Field::new(Region::L2, 32, 0, 32, false, true, &[]),
        Field::new(Region::L2, 33, 0, 32, false, true, &[]),
        Field::new(Region::L2, 34, 0, 32, false, true, &[]),
        Field::new(Region::L2, 35, 0, 32, false, true, &[]),
        Field::new(Region::L2, 36, 0, 32, false, true, &[]),
        Field::new(Region::L2, 37, 0, 32, false, true, &[]),
        Field::new(Region::L2, 38, 0, 32, false, true, &[]),
        Field::new(Region::L2, 39, 0, 32, false, true, &[]),
        Field::new(Region::L2, 40, 0, 32, false, true, &[]),
        Field::new(Region::L2, 41, 0, 32, false, true, &[]),
        Field::new(Region::L2, 42, 0, 32, false, true, &[]),
        Field::new(Region::L2, 43, 0, 32, false, true, &[]),
        Field::new(Region::L2, 44, 0, 32, false, true, &[]),
        Field::new(Region::L2, 45, 0, 32, false, true, &[]),
        Field::new(Region::L2, 46, 0, 32, false, true, &[]),
        Field::new(Region::L2, 47, 0, 32, false, true, &[]),
        Field::new(Region::L2, 48, 0, 32, false, true, &[]),
        Field::new(Region::L2, 49, 0, 32, false, true, &[]),
        Field::new(Region::L2, 50, 0, 32, false, true, &[]),
        Field::new(Region::L2, 51, 0, 32, false, true, &[]),
        Field::new(Region::L2, 52, 0, 32, false, true, &[]),
        Field::new(Region::L2, 53, 0, 32, false, true, &[]),
        Field::new(Region::L2, 54, 0, 32, false, true, &[]),
        Field::new(Region::L2, 55, 0, 32, false, true, &[]),
        Field::new(Region::L2, 56, 0, 32, false, true, &[]),
        Field::new(Region::L2, 57, 0, 32, false, true, &[]),
        Field::new(Region::L2, 58, 0, 32, false, true, &[]),
        Field::new(Region::L2, 59, 0, 32, false, true, &[]),
        Field::new(Region::L2, 60, 0, 32, false, true, &[]),
        Field::new(Region::L2, 61, 0, 32, false, true, &[]),
        Field::new(Region::L2, 62, 0, 32, false, true, &[]),
        Field::new(Region::L2, 63, 0, 32, false, true, &[]),
        Field::new(Region::L2, 64, 0, 32, false, true, &[]),
        Field::new(Region::L2, 65, 0, 32, false, true, &[]),
        Field::new(Region::L2, 66, 0, 32, false, true, &[]),
        Field::new(Region::L2, 67, 0, 32, false, true, &[]),
        Field::new(Region::L2, 68, 0, 32, false, true, &[]),
        Field::new(Region::L2, 69, 0, 32, false, true, &[]),
        Field::new(Region::L2, 70, 0, 32, false, true, &[]),
        Field::new(Region::L2, 71, 0, 32, false, true, &[]),
        Field::new(Region::L2, 72, 0, 32, false, true, &[]),
        Field::new(Region::L2, 73, 0, 32, false, true, &[]),
        Field::new(Region::L2, 74, 0, 32, false, true, &[]),
        Field::new(Region::L2, 75, 0, 32, false, true, &[]),
        Field::new(Region::L2, 76, 0, 32, false, true, &[]),
        Field::new(Region::L2, 77, 0, 32, false, true, &[]),
        Field::new(Region::L2, 78, 0, 32, false, true, &[]),
        Field::new(Region::L2, 79, 0, 32, false, true, &[]),
    ];
}

/// Register `lamd_moda_qp` at 0x0140: 52 words, ReadWrite.
pub mod lamd_moda_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lamd_moda_qp", 80, 52, Access::ReadWrite, 1);
    pub const LAMD_MODA_QP: [Field; 52] = [
        Field::new(Region::L2, 80, 0, 32, false, true, &[]),
        Field::new(Region::L2, 81, 0, 32, false, true, &[]),
        Field::new(Region::L2, 82, 0, 32, false, true, &[]),
        Field::new(Region::L2, 83, 0, 32, false, true, &[]),
        Field::new(Region::L2, 84, 0, 32, false, true, &[]),
        Field::new(Region::L2, 85, 0, 32, false, true, &[]),
        Field::new(Region::L2, 86, 0, 32, false, true, &[]),
        Field::new(Region::L2, 87, 0, 32, false, true, &[]),
        Field::new(Region::L2, 88, 0, 32, false, true, &[]),
        Field::new(Region::L2, 89, 0, 32, false, true, &[]),
        Field::new(Region::L2, 90, 0, 32, false, true, &[]),
        Field::new(Region::L2, 91, 0, 32, false, true, &[]),
        Field::new(Region::L2, 92, 0, 32, false, true, &[]),
        Field::new(Region::L2, 93, 0, 32, false, true, &[]),
        Field::new(Region::L2, 94, 0, 32, false, true, &[]),
        Field::new(Region::L2, 95, 0, 32, false, true, &[]),
        Field::new(Region::L2, 96, 0, 32, false, true, &[]),
        Field::new(Region::L2, 97, 0, 32, false, true, &[]),
        Field::new(Region::L2, 98, 0, 32, false, true, &[]),
        Field::new(Region::L2, 99, 0, 32, false, true, &[]),
        Field::new(Region::L2, 100, 0, 32, false, true, &[]),
        Field::new(Region::L2, 101, 0, 32, false, true, &[]),
        Field::new(Region::L2, 102, 0, 32, false, true, &[]),
        Field::new(Region::L2, 103, 0, 32, false, true, &[]),
        Field::new(Region::L2, 104, 0, 32, false, true, &[]),
        Field::new(Region::L2, 105, 0, 32, false, true, &[]),
        Field::new(Region::L2, 106, 0, 32, false, true, &[]),
        Field::new(Region::L2, 107, 0, 32, false, true, &[]),
        Field::new(Region::L2, 108, 0, 32, false, true, &[]),
        Field::new(Region::L2, 109, 0, 32, false, true, &[]),
        Field::new(Region::L2, 110, 0, 32, false, true, &[]),
        Field::new(Region::L2, 111, 0, 32, false, true, &[]),
        Field::new(Region::L2, 112, 0, 32, false, true, &[]),
        Field::new(Region::L2, 113, 0, 32, false, true, &[]),
        Field::new(Region::L2, 114, 0, 32, false, true, &[]),
        Field::new(Region::L2, 115, 0, 32, false, true, &[]),
        Field::new(Region::L2, 116, 0, 32, false, true, &[]),
        Field::new(Region::L2, 117, 0, 32, false, true, &[]),
        Field::new(Region::L2, 118, 0, 32, false, true, &[]),
        Field::new(Region::L2, 119, 0, 32, false, true, &[]),
        Field::new(Region::L2, 120, 0, 32, false, true, &[]),
        Field::new(Region::L2, 121, 0, 32, false, true, &[]),
        Field::new(Region::L2, 122, 0, 32, false, true, &[]),
        Field::new(Region::L2, 123, 0, 32, false, true, &[]),
        Field::new(Region::L2, 124, 0, 32, false, true, &[]),
        Field::new(Region::L2, 125, 0, 32, false, true, &[]),
        Field::new(Region::L2, 126, 0, 32, false, true, &[]),
        Field::new(Region::L2, 127, 0, 32, false, true, &[]),
        Field::new(Region::L2, 128, 0, 32, false, true, &[]),
        Field::new(Region::L2, 129, 0, 32, false, true, &[]),
        Field::new(Region::L2, 130, 0, 32, false, true, &[]),
        Field::new(Region::L2, 131, 0, 32, false, true, &[]),
    ];
}

/// Register `lamd_modb_qp` at 0x0210: 52 words, ReadWrite.
pub mod lamd_modb_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "lamd_modb_qp", 132, 52, Access::ReadWrite, 1);
    pub const LAMD_MODB_QP: [Field; 52] = [
        Field::new(Region::L2, 132, 0, 32, false, true, &[]),
        Field::new(Region::L2, 133, 0, 32, false, true, &[]),
        Field::new(Region::L2, 134, 0, 32, false, true, &[]),
        Field::new(Region::L2, 135, 0, 32, false, true, &[]),
        Field::new(Region::L2, 136, 0, 32, false, true, &[]),
        Field::new(Region::L2, 137, 0, 32, false, true, &[]),
        Field::new(Region::L2, 138, 0, 32, false, true, &[]),
        Field::new(Region::L2, 139, 0, 32, false, true, &[]),
        Field::new(Region::L2, 140, 0, 32, false, true, &[]),
        Field::new(Region::L2, 141, 0, 32, false, true, &[]),
        Field::new(Region::L2, 142, 0, 32, false, true, &[]),
        Field::new(Region::L2, 143, 0, 32, false, true, &[]),
        Field::new(Region::L2, 144, 0, 32, false, true, &[]),
        Field::new(Region::L2, 145, 0, 32, false, true, &[]),
        Field::new(Region::L2, 146, 0, 32, false, true, &[]),
        Field::new(Region::L2, 147, 0, 32, false, true, &[]),
        Field::new(Region::L2, 148, 0, 32, false, true, &[]),
        Field::new(Region::L2, 149, 0, 32, false, true, &[]),
        Field::new(Region::L2, 150, 0, 32, false, true, &[]),
        Field::new(Region::L2, 151, 0, 32, false, true, &[]),
        Field::new(Region::L2, 152, 0, 32, false, true, &[]),
        Field::new(Region::L2, 153, 0, 32, false, true, &[]),
        Field::new(Region::L2, 154, 0, 32, false, true, &[]),
        Field::new(Region::L2, 155, 0, 32, false, true, &[]),
        Field::new(Region::L2, 156, 0, 32, false, true, &[]),
        Field::new(Region::L2, 157, 0, 32, false, true, &[]),
        Field::new(Region::L2, 158, 0, 32, false, true, &[]),
        Field::new(Region::L2, 159, 0, 32, false, true, &[]),
        Field::new(Region::L2, 160, 0, 32, false, true, &[]),
        Field::new(Region::L2, 161, 0, 32, false, true, &[]),
        Field::new(Region::L2, 162, 0, 32, false, true, &[]),
        Field::new(Region::L2, 163, 0, 32, false, true, &[]),
        Field::new(Region::L2, 164, 0, 32, false, true, &[]),
        Field::new(Region::L2, 165, 0, 32, false, true, &[]),
        Field::new(Region::L2, 166, 0, 32, false, true, &[]),
        Field::new(Region::L2, 167, 0, 32, false, true, &[]),
        Field::new(Region::L2, 168, 0, 32, false, true, &[]),
        Field::new(Region::L2, 169, 0, 32, false, true, &[]),
        Field::new(Region::L2, 170, 0, 32, false, true, &[]),
        Field::new(Region::L2, 171, 0, 32, false, true, &[]),
        Field::new(Region::L2, 172, 0, 32, false, true, &[]),
        Field::new(Region::L2, 173, 0, 32, false, true, &[]),
        Field::new(Region::L2, 174, 0, 32, false, true, &[]),
        Field::new(Region::L2, 175, 0, 32, false, true, &[]),
        Field::new(Region::L2, 176, 0, 32, false, true, &[]),
        Field::new(Region::L2, 177, 0, 32, false, true, &[]),
        Field::new(Region::L2, 178, 0, 32, false, true, &[]),
        Field::new(Region::L2, 179, 0, 32, false, true, &[]),
        Field::new(Region::L2, 180, 0, 32, false, true, &[]),
        Field::new(Region::L2, 181, 0, 32, false, true, &[]),
        Field::new(Region::L2, 182, 0, 32, false, true, &[]),
        Field::new(Region::L2, 183, 0, 32, false, true, &[]),
    ];
}

/// Register `madi_cfg` at 0x02e0: 1 word, ReadWrite.
pub mod madi_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "madi_cfg", 184, 1, Access::ReadWrite, 1);
    pub const MADI_THD: Field = Field::new(Region::L2, 184, 16, 8, false, true, &[]);
}

/// Register `aq_tthd` at 0x02e4: 4 words, ReadWrite.
pub mod aq_tthd {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "aq_tthd", 185, 4, Access::ReadWrite, 1);
    pub const AQ_TTHD: [Field; 16] = [
        Field::new(Region::L2, 185, 0, 8, false, true, &[]),
        Field::new(Region::L2, 185, 8, 8, false, true, &[]),
        Field::new(Region::L2, 185, 16, 8, false, true, &[]),
        Field::new(Region::L2, 185, 24, 8, false, true, &[]),
        Field::new(Region::L2, 186, 0, 8, false, true, &[]),
        Field::new(Region::L2, 186, 8, 8, false, true, &[]),
        Field::new(Region::L2, 186, 16, 8, false, true, &[]),
        Field::new(Region::L2, 186, 24, 8, false, true, &[]),
        Field::new(Region::L2, 187, 0, 8, false, true, &[]),
        Field::new(Region::L2, 187, 8, 8, false, true, &[]),
        Field::new(Region::L2, 187, 16, 8, false, true, &[]),
        Field::new(Region::L2, 187, 24, 8, false, true, &[]),
        Field::new(Region::L2, 188, 0, 8, false, true, &[]),
        Field::new(Region::L2, 188, 8, 8, false, true, &[]),
        Field::new(Region::L2, 188, 16, 8, false, true, &[]),
        Field::new(Region::L2, 188, 24, 8, false, true, &[]),
    ];
}

/// Register `aq_step` at 0x02f4: 4 words, ReadWrite.
pub mod aq_step {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "aq_step", 189, 4, Access::ReadWrite, 1);
    pub const AQ_STEP: [Field; 16] = [
        Field::new(Region::L2, 189, 0, 8, false, true, &[]),
        Field::new(Region::L2, 189, 8, 8, false, true, &[]),
        Field::new(Region::L2, 189, 16, 8, false, true, &[]),
        Field::new(Region::L2, 189, 24, 8, false, true, &[]),
        Field::new(Region::L2, 190, 0, 8, false, true, &[]),
        Field::new(Region::L2, 190, 8, 8, false, true, &[]),
        Field::new(Region::L2, 190, 16, 8, false, true, &[]),
        Field::new(Region::L2, 190, 24, 8, false, true, &[]),
        Field::new(Region::L2, 191, 0, 8, false, true, &[]),
        Field::new(Region::L2, 191, 8, 8, false, true, &[]),
        Field::new(Region::L2, 191, 16, 8, false, true, &[]),
        Field::new(Region::L2, 191, 24, 8, false, true, &[]),
        Field::new(Region::L2, 192, 0, 8, false, true, &[]),
        Field::new(Region::L2, 192, 8, 8, false, true, &[]),
        Field::new(Region::L2, 192, 16, 8, false, true, &[]),
        Field::new(Region::L2, 192, 24, 8, false, true, &[]),
    ];
}

/// Register `pre_intra_cla0_b0` at 0x0310: 1 word, ReadWrite.
pub mod pre_intra_cla0_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla0_b0",
        196,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA0_M0: Field = Field::new(Region::L2, 196, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M1: Field = Field::new(Region::L2, 196, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M2: Field = Field::new(Region::L2, 196, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M3: Field = Field::new(Region::L2, 196, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M4: Field = Field::new(Region::L2, 196, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla0_b1` at 0x0314: 1 word, ReadWrite.
pub mod pre_intra_cla0_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla0_b1",
        197,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA0_M5: Field = Field::new(Region::L2, 197, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M6: Field = Field::new(Region::L2, 197, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M7: Field = Field::new(Region::L2, 197, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M8: Field = Field::new(Region::L2, 197, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M9: Field = Field::new(Region::L2, 197, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla1_b0` at 0x0318: 1 word, ReadWrite.
pub mod pre_intra_cla1_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla1_b0",
        198,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA1_M0: Field = Field::new(Region::L2, 198, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M1: Field = Field::new(Region::L2, 198, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M2: Field = Field::new(Region::L2, 198, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M3: Field = Field::new(Region::L2, 198, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M4: Field = Field::new(Region::L2, 198, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla1_b1` at 0x031c: 1 word, ReadWrite.
pub mod pre_intra_cla1_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla1_b1",
        199,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA1_M5: Field = Field::new(Region::L2, 199, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M6: Field = Field::new(Region::L2, 199, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M7: Field = Field::new(Region::L2, 199, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M8: Field = Field::new(Region::L2, 199, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M9: Field = Field::new(Region::L2, 199, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla2_b0` at 0x0320: 1 word, ReadWrite.
pub mod pre_intra_cla2_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla2_b0",
        200,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA2_M0: Field = Field::new(Region::L2, 200, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M1: Field = Field::new(Region::L2, 200, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M2: Field = Field::new(Region::L2, 200, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M3: Field = Field::new(Region::L2, 200, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M4: Field = Field::new(Region::L2, 200, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla2_b1` at 0x0324: 1 word, ReadWrite.
pub mod pre_intra_cla2_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla2_b1",
        201,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA2_M5: Field = Field::new(Region::L2, 201, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M6: Field = Field::new(Region::L2, 201, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M7: Field = Field::new(Region::L2, 201, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M8: Field = Field::new(Region::L2, 201, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M9: Field = Field::new(Region::L2, 201, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla3_b0` at 0x0328: 1 word, ReadWrite.
pub mod pre_intra_cla3_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla3_b0",
        202,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA3_M0: Field = Field::new(Region::L2, 202, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M1: Field = Field::new(Region::L2, 202, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M2: Field = Field::new(Region::L2, 202, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M3: Field = Field::new(Region::L2, 202, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M4: Field = Field::new(Region::L2, 202, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla3_b1` at 0x032c: 1 word, ReadWrite.
pub mod pre_intra_cla3_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla3_b1",
        203,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA3_M5: Field = Field::new(Region::L2, 203, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M6: Field = Field::new(Region::L2, 203, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M7: Field = Field::new(Region::L2, 203, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M8: Field = Field::new(Region::L2, 203, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M9: Field = Field::new(Region::L2, 203, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla4_b0` at 0x0330: 1 word, ReadWrite.
pub mod pre_intra_cla4_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla4_b0",
        204,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA4_M0: Field = Field::new(Region::L2, 204, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M1: Field = Field::new(Region::L2, 204, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M2: Field = Field::new(Region::L2, 204, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M3: Field = Field::new(Region::L2, 204, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M4: Field = Field::new(Region::L2, 204, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla4_b1` at 0x0334: 1 word, ReadWrite.
pub mod pre_intra_cla4_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla4_b1",
        205,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA4_M5: Field = Field::new(Region::L2, 205, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M6: Field = Field::new(Region::L2, 205, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M7: Field = Field::new(Region::L2, 205, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M8: Field = Field::new(Region::L2, 205, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M9: Field = Field::new(Region::L2, 205, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla5_b0` at 0x0338: 1 word, ReadWrite.
pub mod pre_intra_cla5_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla5_b0",
        206,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA5_M0: Field = Field::new(Region::L2, 206, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M1: Field = Field::new(Region::L2, 206, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M2: Field = Field::new(Region::L2, 206, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M3: Field = Field::new(Region::L2, 206, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M4: Field = Field::new(Region::L2, 206, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla5_b1` at 0x033c: 1 word, ReadWrite.
pub mod pre_intra_cla5_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla5_b1",
        207,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA5_M5: Field = Field::new(Region::L2, 207, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M6: Field = Field::new(Region::L2, 207, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M7: Field = Field::new(Region::L2, 207, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M8: Field = Field::new(Region::L2, 207, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M9: Field = Field::new(Region::L2, 207, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla6_b0` at 0x0340: 1 word, ReadWrite.
pub mod pre_intra_cla6_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla6_b0",
        208,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA6_M0: Field = Field::new(Region::L2, 208, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M1: Field = Field::new(Region::L2, 208, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M2: Field = Field::new(Region::L2, 208, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M3: Field = Field::new(Region::L2, 208, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M4: Field = Field::new(Region::L2, 208, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla6_b1` at 0x0344: 1 word, ReadWrite.
pub mod pre_intra_cla6_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla6_b1",
        209,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA6_M5: Field = Field::new(Region::L2, 209, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M6: Field = Field::new(Region::L2, 209, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M7: Field = Field::new(Region::L2, 209, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M8: Field = Field::new(Region::L2, 209, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M9: Field = Field::new(Region::L2, 209, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla7_b0` at 0x0348: 1 word, ReadWrite.
pub mod pre_intra_cla7_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla7_b0",
        210,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA7_M0: Field = Field::new(Region::L2, 210, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M1: Field = Field::new(Region::L2, 210, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M2: Field = Field::new(Region::L2, 210, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M3: Field = Field::new(Region::L2, 210, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M4: Field = Field::new(Region::L2, 210, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla7_b1` at 0x034c: 1 word, ReadWrite.
pub mod pre_intra_cla7_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla7_b1",
        211,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA7_M5: Field = Field::new(Region::L2, 211, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M6: Field = Field::new(Region::L2, 211, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M7: Field = Field::new(Region::L2, 211, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M8: Field = Field::new(Region::L2, 211, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M9: Field = Field::new(Region::L2, 211, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla8_b0` at 0x0350: 1 word, ReadWrite.
pub mod pre_intra_cla8_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla8_b0",
        212,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA8_M0: Field = Field::new(Region::L2, 212, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M1: Field = Field::new(Region::L2, 212, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M2: Field = Field::new(Region::L2, 212, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M3: Field = Field::new(Region::L2, 212, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M4: Field = Field::new(Region::L2, 212, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla8_b1` at 0x0354: 1 word, ReadWrite.
pub mod pre_intra_cla8_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla8_b1",
        213,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA8_M5: Field = Field::new(Region::L2, 213, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M6: Field = Field::new(Region::L2, 213, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M7: Field = Field::new(Region::L2, 213, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M8: Field = Field::new(Region::L2, 213, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M9: Field = Field::new(Region::L2, 213, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla9_b0` at 0x0358: 1 word, ReadWrite.
pub mod pre_intra_cla9_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla9_b0",
        214,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA9_M0: Field = Field::new(Region::L2, 214, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M1: Field = Field::new(Region::L2, 214, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M2: Field = Field::new(Region::L2, 214, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M3: Field = Field::new(Region::L2, 214, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M4: Field = Field::new(Region::L2, 214, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla9_b1` at 0x035c: 1 word, ReadWrite.
pub mod pre_intra_cla9_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla9_b1",
        215,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA9_M5: Field = Field::new(Region::L2, 215, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M6: Field = Field::new(Region::L2, 215, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M7: Field = Field::new(Region::L2, 215, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M8: Field = Field::new(Region::L2, 215, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M9: Field = Field::new(Region::L2, 215, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla10_b0` at 0x0360: 1 word, ReadWrite.
pub mod pre_intra_cla10_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla10_b0",
        216,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA10_M0: Field = Field::new(Region::L2, 216, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M1: Field = Field::new(Region::L2, 216, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M2: Field = Field::new(Region::L2, 216, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M3: Field = Field::new(Region::L2, 216, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M4: Field = Field::new(Region::L2, 216, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla10_b1` at 0x0364: 1 word, ReadWrite.
pub mod pre_intra_cla10_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla10_b1",
        217,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA10_M5: Field = Field::new(Region::L2, 217, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M6: Field = Field::new(Region::L2, 217, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M7: Field = Field::new(Region::L2, 217, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M8: Field = Field::new(Region::L2, 217, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M9: Field = Field::new(Region::L2, 217, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla11_b0` at 0x0368: 1 word, ReadWrite.
pub mod pre_intra_cla11_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla11_b0",
        218,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA11_M0: Field = Field::new(Region::L2, 218, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M1: Field = Field::new(Region::L2, 218, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M2: Field = Field::new(Region::L2, 218, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M3: Field = Field::new(Region::L2, 218, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M4: Field = Field::new(Region::L2, 218, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla11_b1` at 0x036c: 1 word, ReadWrite.
pub mod pre_intra_cla11_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla11_b1",
        219,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA11_M5: Field = Field::new(Region::L2, 219, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M6: Field = Field::new(Region::L2, 219, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M7: Field = Field::new(Region::L2, 219, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M8: Field = Field::new(Region::L2, 219, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M9: Field = Field::new(Region::L2, 219, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla12_b0` at 0x0370: 1 word, ReadWrite.
pub mod pre_intra_cla12_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla12_b0",
        220,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA12_M0: Field = Field::new(Region::L2, 220, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M1: Field = Field::new(Region::L2, 220, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M2: Field = Field::new(Region::L2, 220, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M3: Field = Field::new(Region::L2, 220, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M4: Field = Field::new(Region::L2, 220, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla12_b1` at 0x0374: 1 word, ReadWrite.
pub mod pre_intra_cla12_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla12_b1",
        221,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA12_M5: Field = Field::new(Region::L2, 221, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M6: Field = Field::new(Region::L2, 221, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M7: Field = Field::new(Region::L2, 221, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M8: Field = Field::new(Region::L2, 221, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M9: Field = Field::new(Region::L2, 221, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla13_b0` at 0x0378: 1 word, ReadWrite.
pub mod pre_intra_cla13_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla13_b0",
        222,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA13_M0: Field = Field::new(Region::L2, 222, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M1: Field = Field::new(Region::L2, 222, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M2: Field = Field::new(Region::L2, 222, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M3: Field = Field::new(Region::L2, 222, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M4: Field = Field::new(Region::L2, 222, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla13_b1` at 0x037c: 1 word, ReadWrite.
pub mod pre_intra_cla13_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla13_b1",
        223,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA13_M5: Field = Field::new(Region::L2, 223, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M6: Field = Field::new(Region::L2, 223, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M7: Field = Field::new(Region::L2, 223, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M8: Field = Field::new(Region::L2, 223, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M9: Field = Field::new(Region::L2, 223, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla14_b0` at 0x0380: 1 word, ReadWrite.
pub mod pre_intra_cla14_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla14_b0",
        224,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA14_M0: Field = Field::new(Region::L2, 224, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M1: Field = Field::new(Region::L2, 224, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M2: Field = Field::new(Region::L2, 224, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M3: Field = Field::new(Region::L2, 224, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M4: Field = Field::new(Region::L2, 224, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla14_b1` at 0x0384: 1 word, ReadWrite.
pub mod pre_intra_cla14_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla14_b1",
        225,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA14_M5: Field = Field::new(Region::L2, 225, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M6: Field = Field::new(Region::L2, 225, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M7: Field = Field::new(Region::L2, 225, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M8: Field = Field::new(Region::L2, 225, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M9: Field = Field::new(Region::L2, 225, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla15_b0` at 0x0388: 1 word, ReadWrite.
pub mod pre_intra_cla15_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla15_b0",
        226,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA15_M0: Field = Field::new(Region::L2, 226, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M1: Field = Field::new(Region::L2, 226, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M2: Field = Field::new(Region::L2, 226, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M3: Field = Field::new(Region::L2, 226, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M4: Field = Field::new(Region::L2, 226, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla15_b1` at 0x038c: 1 word, ReadWrite.
pub mod pre_intra_cla15_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla15_b1",
        227,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA15_M5: Field = Field::new(Region::L2, 227, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M6: Field = Field::new(Region::L2, 227, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M7: Field = Field::new(Region::L2, 227, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M8: Field = Field::new(Region::L2, 227, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M9: Field = Field::new(Region::L2, 227, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla16_b0` at 0x0390: 1 word, ReadWrite.
pub mod pre_intra_cla16_b0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla16_b0",
        228,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA16_M0: Field = Field::new(Region::L2, 228, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M1: Field = Field::new(Region::L2, 228, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M2: Field = Field::new(Region::L2, 228, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M3: Field = Field::new(Region::L2, 228, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M4: Field = Field::new(Region::L2, 228, 24, 6, false, true, &[]);
}

/// Register `pre_intra_cla16_b1` at 0x0394: 1 word, ReadWrite.
pub mod pre_intra_cla16_b1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "pre_intra_cla16_b1",
        229,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA16_M5: Field = Field::new(Region::L2, 229, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M6: Field = Field::new(Region::L2, 229, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M7: Field = Field::new(Region::L2, 229, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M8: Field = Field::new(Region::L2, 229, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M9: Field = Field::new(Region::L2, 229, 24, 6, false, true, &[]);
}

/// Register `reg_l2reserved1` at 0x0398: 26 words, ReadWrite.
pub mod reg_l2reserved1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "reg_l2reserved1", 230, 26, Access::ReadWrite, 1);
    pub const REG_L2RESERVED1: [Field; 26] = [
        Field::new(Region::L2, 230, 0, 32, false, true, &[]),
        Field::new(Region::L2, 231, 0, 32, false, true, &[]),
        Field::new(Region::L2, 232, 0, 32, false, true, &[]),
        Field::new(Region::L2, 233, 0, 32, false, true, &[]),
        Field::new(Region::L2, 234, 0, 32, false, true, &[]),
        Field::new(Region::L2, 235, 0, 32, false, true, &[]),
        Field::new(Region::L2, 236, 0, 32, false, true, &[]),
        Field::new(Region::L2, 237, 0, 32, false, true, &[]),
        Field::new(Region::L2, 238, 0, 32, false, true, &[]),
        Field::new(Region::L2, 239, 0, 32, false, true, &[]),
        Field::new(Region::L2, 240, 0, 32, false, true, &[]),
        Field::new(Region::L2, 241, 0, 32, false, true, &[]),
        Field::new(Region::L2, 242, 0, 32, false, true, &[]),
        Field::new(Region::L2, 243, 0, 32, false, true, &[]),
        Field::new(Region::L2, 244, 0, 32, false, true, &[]),
        Field::new(Region::L2, 245, 0, 32, false, true, &[]),
        Field::new(Region::L2, 246, 0, 32, false, true, &[]),
        Field::new(Region::L2, 247, 0, 32, false, true, &[]),
        Field::new(Region::L2, 248, 0, 32, false, true, &[]),
        Field::new(Region::L2, 249, 0, 32, false, true, &[]),
        Field::new(Region::L2, 250, 0, 32, false, true, &[]),
        Field::new(Region::L2, 251, 0, 32, false, true, &[]),
        Field::new(Region::L2, 252, 0, 32, false, true, &[]),
        Field::new(Region::L2, 253, 0, 32, false, true, &[]),
        Field::new(Region::L2, 254, 0, 32, false, true, &[]),
        Field::new(Region::L2, 255, 0, 32, false, true, &[]),
    ];
}

/// Register `reg_rdo_ckg_hevc` at 0x0400: 1 word, ReadWrite.
pub mod reg_rdo_ckg_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "reg_rdo_ckg_hevc", 256, 1, Access::ReadWrite, 1);
    pub const REG_RDO_CKG_HEVC: Field = Field::new(Region::L2, 256, 0, 32, false, true, &[]);
}

/// Register `reg_l2reserved2` at 0x0404: 3 words, ReadWrite.
pub mod reg_l2reserved2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "reg_l2reserved2", 257, 3, Access::ReadWrite, 1);
    pub const REG_L2RESERVED2: [Field; 3] = [
        Field::new(Region::L2, 257, 0, 32, false, true, &[]),
        Field::new(Region::L2, 258, 0, 32, false, true, &[]),
        Field::new(Region::L2, 259, 0, 32, false, true, &[]),
    ];
}

/// Register `i16_sobel_t_hevc` at 0x0410: 1 word, ReadWrite.
pub mod i16_sobel_t_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "i16_sobel_t_hevc", 260, 1, Access::ReadWrite, 1);
    pub const INTRA_L16_SOBEL_T0: Field = Field::new(Region::L2, 260, 0, 12, false, true, &[]);
    pub const INTRA_L16_SOBEL_T1: Field = Field::new(Region::L2, 260, 16, 12, false, true, &[]);
}

/// Register `i16_sobel_a_00_hevc` at 0x0414: 1 word, ReadWrite.
pub mod i16_sobel_a_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_a_00_hevc",
        261,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_A0_QP0: Field = Field::new(Region::L2, 261, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP1: Field = Field::new(Region::L2, 261, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP2: Field = Field::new(Region::L2, 261, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP3: Field = Field::new(Region::L2, 261, 18, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP4: Field = Field::new(Region::L2, 261, 24, 6, false, true, &[]);
}

/// Register `i16_sobel_a_01_hevc` at 0x0418: 1 word, ReadWrite.
pub mod i16_sobel_a_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_a_01_hevc",
        262,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_A0_QP5: Field = Field::new(Region::L2, 262, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP6: Field = Field::new(Region::L2, 262, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP7: Field = Field::new(Region::L2, 262, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP8: Field = Field::new(Region::L2, 262, 18, 6, false, true, &[]);
}

/// Register `i16_sobel_b_00_hevc` at 0x041c: 1 word, ReadWrite.
pub mod i16_sobel_b_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_b_00_hevc",
        263,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP0: Field = Field::new(Region::L2, 263, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP1: Field = Field::new(Region::L2, 263, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_b_01_hevc` at 0x0420: 1 word, ReadWrite.
pub mod i16_sobel_b_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_b_01_hevc",
        264,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP2: Field = Field::new(Region::L2, 264, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP3: Field = Field::new(Region::L2, 264, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_b_02_hevc` at 0x0424: 1 word, ReadWrite.
pub mod i16_sobel_b_02_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_b_02_hevc",
        265,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP4: Field = Field::new(Region::L2, 265, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP5: Field = Field::new(Region::L2, 265, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_b_03_hevc` at 0x0428: 1 word, ReadWrite.
pub mod i16_sobel_b_03_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_b_03_hevc",
        266,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP6: Field = Field::new(Region::L2, 266, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP7: Field = Field::new(Region::L2, 266, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_b_04_hevc` at 0x042c: 1 word, ReadWrite.
pub mod i16_sobel_b_04_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_b_04_hevc",
        267,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP8: Field = Field::new(Region::L2, 267, 0, 15, false, true, &[]);
}

/// Register `i16_sobel_c_00_hevc` at 0x0430: 1 word, ReadWrite.
pub mod i16_sobel_c_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_c_00_hevc",
        268,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_C0_QP0: Field = Field::new(Region::L2, 268, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP1: Field = Field::new(Region::L2, 268, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP2: Field = Field::new(Region::L2, 268, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP3: Field = Field::new(Region::L2, 268, 18, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP4: Field = Field::new(Region::L2, 268, 24, 6, false, true, &[]);
}

/// Register `i16_sobel_c_01_hevc` at 0x0434: 1 word, ReadWrite.
pub mod i16_sobel_c_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_c_01_hevc",
        269,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_C0_QP5: Field = Field::new(Region::L2, 269, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP6: Field = Field::new(Region::L2, 269, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP7: Field = Field::new(Region::L2, 269, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP8: Field = Field::new(Region::L2, 269, 18, 6, false, true, &[]);
}

/// Register `i16_sobel_d_00_hevc` at 0x0438: 1 word, ReadWrite.
pub mod i16_sobel_d_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_d_00_hevc",
        270,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP0: Field = Field::new(Region::L2, 270, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP1: Field = Field::new(Region::L2, 270, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_d_01_hevc` at 0x043c: 1 word, ReadWrite.
pub mod i16_sobel_d_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_d_01_hevc",
        271,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP2: Field = Field::new(Region::L2, 271, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP3: Field = Field::new(Region::L2, 271, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_d_02_hevc` at 0x0440: 1 word, ReadWrite.
pub mod i16_sobel_d_02_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_d_02_hevc",
        272,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP4: Field = Field::new(Region::L2, 272, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP5: Field = Field::new(Region::L2, 272, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_d_03_hevc` at 0x0444: 1 word, ReadWrite.
pub mod i16_sobel_d_03_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_d_03_hevc",
        273,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP6: Field = Field::new(Region::L2, 273, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP7: Field = Field::new(Region::L2, 273, 16, 15, false, true, &[]);
}

/// Register `i16_sobel_d_04_hevc` at 0x0448: 1 word, ReadWrite.
pub mod i16_sobel_d_04_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_d_04_hevc",
        274,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP8: Field = Field::new(Region::L2, 274, 0, 15, false, true, &[]);
}

/// Register `i16_sobel_e_00_17_hevc` at 0x044c: 18 words, ReadWrite.
pub mod i16_sobel_e_00_17_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i16_sobel_e_00_17_hevc",
        275,
        18,
        Access::ReadWrite,
        1,
    );
    pub const I16_SOBEL_E_00_17_HEVC: [Field; 18] = [
        Field::new(Region::L2, 275, 0, 32, false, true, &[]),
        Field::new(Region::L2, 276, 0, 32, false, true, &[]),
        Field::new(Region::L2, 277, 0, 32, false, true, &[]),
        Field::new(Region::L2, 278, 0, 32, false, true, &[]),
        Field::new(Region::L2, 279, 0, 32, false, true, &[]),
        Field::new(Region::L2, 280, 0, 32, false, true, &[]),
        Field::new(Region::L2, 281, 0, 32, false, true, &[]),
        Field::new(Region::L2, 282, 0, 32, false, true, &[]),
        Field::new(Region::L2, 283, 0, 32, false, true, &[]),
        Field::new(Region::L2, 284, 0, 32, false, true, &[]),
        Field::new(Region::L2, 285, 0, 32, false, true, &[]),
        Field::new(Region::L2, 286, 0, 32, false, true, &[]),
        Field::new(Region::L2, 287, 0, 32, false, true, &[]),
        Field::new(Region::L2, 288, 0, 32, false, true, &[]),
        Field::new(Region::L2, 289, 0, 32, false, true, &[]),
        Field::new(Region::L2, 290, 0, 32, false, true, &[]),
        Field::new(Region::L2, 291, 0, 32, false, true, &[]),
        Field::new(Region::L2, 292, 0, 32, false, true, &[]),
    ];
}

/// Register `i32_sobel_t_00_hevc` at 0x0494: 1 word, ReadWrite.
pub mod i32_sobel_t_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_t_00_hevc",
        293,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T2: Field = Field::new(Region::L2, 293, 0, 12, false, true, &[]);
    pub const INTRA_L32_SOBEL_T3: Field = Field::new(Region::L2, 293, 16, 12, false, true, &[]);
}

/// Register `i32_sobel_t_01_hevc` at 0x0498: 1 word, ReadWrite.
pub mod i32_sobel_t_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_t_01_hevc",
        294,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T4: Field = Field::new(Region::L2, 294, 0, 6, false, true, &[]);
}

/// Register `i32_sobel_t_02_hevc` at 0x049c: 1 word, ReadWrite.
pub mod i32_sobel_t_02_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_t_02_hevc",
        295,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T5: Field = Field::new(Region::L2, 295, 0, 12, false, true, &[]);
    pub const INTRA_L32_SOBEL_T6: Field = Field::new(Region::L2, 295, 16, 12, false, true, &[]);
}

/// Register `i32_sobel_a_hevc` at 0x04a0: 1 word, ReadWrite.
pub mod i32_sobel_a_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "i32_sobel_a_hevc", 296, 1, Access::ReadWrite, 1);
    pub const INTRA_L32_SOBEL_A1_QP0: Field = Field::new(Region::L2, 296, 0, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP1: Field = Field::new(Region::L2, 296, 6, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP2: Field = Field::new(Region::L2, 296, 12, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP3: Field = Field::new(Region::L2, 296, 18, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP4: Field = Field::new(Region::L2, 296, 24, 6, false, true, &[]);
}

/// Register `i32_sobel_b_00_hevc` at 0x04a4: 1 word, ReadWrite.
pub mod i32_sobel_b_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_b_00_hevc",
        297,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP0: Field = Field::new(Region::L2, 297, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_B1_QP1: Field = Field::new(Region::L2, 297, 16, 15, false, true, &[]);
}

/// Register `i32_sobel_b_01_hevc` at 0x04a8: 1 word, ReadWrite.
pub mod i32_sobel_b_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_b_01_hevc",
        298,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP2: Field = Field::new(Region::L2, 298, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_B1_QP3: Field = Field::new(Region::L2, 298, 16, 15, false, true, &[]);
}

/// Register `i32_sobel_b_02_hevc` at 0x04ac: 1 word, ReadWrite.
pub mod i32_sobel_b_02_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_b_02_hevc",
        299,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP4: Field = Field::new(Region::L2, 299, 0, 15, false, true, &[]);
}

/// Register `i32_sobel_c_hevc` at 0x04b0: 1 word, ReadWrite.
pub mod i32_sobel_c_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "i32_sobel_c_hevc", 300, 1, Access::ReadWrite, 1);
    pub const INTRA_L32_SOBEL_C1_QP0: Field = Field::new(Region::L2, 300, 0, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP1: Field = Field::new(Region::L2, 300, 6, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP2: Field = Field::new(Region::L2, 300, 12, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP3: Field = Field::new(Region::L2, 300, 18, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP4: Field = Field::new(Region::L2, 300, 24, 6, false, true, &[]);
}

/// Register `i32_sobel_d_00_hevc` at 0x04b4: 1 word, ReadWrite.
pub mod i32_sobel_d_00_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_d_00_hevc",
        301,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP0: Field = Field::new(Region::L2, 301, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_D1_QP1: Field = Field::new(Region::L2, 301, 16, 15, false, true, &[]);
}

/// Register `i32_sobel_d_01_hevc` at 0x04b8: 1 word, ReadWrite.
pub mod i32_sobel_d_01_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_d_01_hevc",
        302,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP2: Field = Field::new(Region::L2, 302, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_D1_QP3: Field = Field::new(Region::L2, 302, 16, 15, false, true, &[]);
}

/// Register `i32_sobel_d_02_hevc` at 0x04bc: 1 word, ReadWrite.
pub mod i32_sobel_d_02_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_d_02_hevc",
        303,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP4: Field = Field::new(Region::L2, 303, 0, 15, false, true, &[]);
}

/// Register `i32_sobel_e_00_09_hevc` at 0x04c0: 10 words, ReadWrite.
pub mod i32_sobel_e_00_09_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::L2,
        "i32_sobel_e_00_09_hevc",
        304,
        10,
        Access::ReadWrite,
        1,
    );
    pub const I32_SOBEL_E_00_09_HEVC: [Field; 10] = [
        Field::new(Region::L2, 304, 0, 32, false, true, &[]),
        Field::new(Region::L2, 305, 0, 32, false, true, &[]),
        Field::new(Region::L2, 306, 0, 32, false, true, &[]),
        Field::new(Region::L2, 307, 0, 32, false, true, &[]),
        Field::new(Region::L2, 308, 0, 32, false, true, &[]),
        Field::new(Region::L2, 309, 0, 32, false, true, &[]),
        Field::new(Region::L2, 310, 0, 32, false, true, &[]),
        Field::new(Region::L2, 311, 0, 32, false, true, &[]),
        Field::new(Region::L2, 312, 0, 32, false, true, &[]),
        Field::new(Region::L2, 313, 0, 32, false, true, &[]),
    ];
}
