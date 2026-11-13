//! The VEPU541 H.264 second window (its own base in the block), layout only: no job reaches it.

#![allow(clippy::unreadable_literal)]

/// Register `iprd_tthdy4` at 0x0004: 2 words, ReadWrite.
pub mod iprd_tthdy4 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_tthdy4", 1, 2, Access::ReadWrite, 1);
    pub const IPRD_TTHDY4: [Field; 4] = [
        Field::new(Region::L2, 1, 0, 16, false, true, &[]),
        Field::new(Region::L2, 1, 16, 16, false, true, &[]),
        Field::new(Region::L2, 2, 0, 16, false, true, &[]),
        Field::new(Region::L2, 2, 16, 16, false, true, &[]),
    ];
}

/// Register `iprd_tthdc8` at 0x000c: 2 words, ReadWrite.
pub mod iprd_tthdc8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_tthdc8", 3, 2, Access::ReadWrite, 1);
    pub const IPRD_TTHDC8: [Field; 4] = [
        Field::new(Region::L2, 3, 0, 16, false, true, &[]),
        Field::new(Region::L2, 3, 16, 16, false, true, &[]),
        Field::new(Region::L2, 4, 0, 16, false, true, &[]),
        Field::new(Region::L2, 4, 16, 16, false, true, &[]),
    ];
}

/// Register `iprd_tthdy8` at 0x0014: 2 words, ReadWrite.
pub mod iprd_tthdy8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_tthdy8", 5, 2, Access::ReadWrite, 1);
    pub const IPRD_TTHDY8: [Field; 4] = [
        Field::new(Region::L2, 5, 0, 16, false, true, &[]),
        Field::new(Region::L2, 5, 16, 16, false, true, &[]),
        Field::new(Region::L2, 6, 0, 16, false, true, &[]),
        Field::new(Region::L2, 6, 16, 16, false, true, &[]),
    ];
}

/// Register `iprd_tthd_ul` at 0x001c: 1 word, ReadWrite.
pub mod iprd_tthd_ul {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_tthd_ul", 7, 1, Access::ReadWrite, 1);
    pub const IPRD_TTHD_UL: Field = Field::new(Region::L2, 7, 0, 32, false, true, &[]);
}

/// Register `iprd_wgty8` at 0x0020: 1 word, ReadWrite.
pub mod iprd_wgty8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_wgty8", 8, 1, Access::ReadWrite, 1);
    pub const IPRD_WGTY8: [Field; 4] = [
        Field::new(Region::L2, 8, 0, 8, false, true, &[]),
        Field::new(Region::L2, 8, 8, 8, false, true, &[]),
        Field::new(Region::L2, 8, 16, 8, false, true, &[]),
        Field::new(Region::L2, 8, 24, 8, false, true, &[]),
    ];
}

/// Register `iprd_wgty4` at 0x0024: 1 word, ReadWrite.
pub mod iprd_wgty4 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_wgty4", 9, 1, Access::ReadWrite, 1);
    pub const IPRD_WGTY4: [Field; 4] = [
        Field::new(Region::L2, 9, 0, 8, false, true, &[]),
        Field::new(Region::L2, 9, 8, 8, false, true, &[]),
        Field::new(Region::L2, 9, 16, 8, false, true, &[]),
        Field::new(Region::L2, 9, 24, 8, false, true, &[]),
    ];
}

/// Register `iprd_wgty16` at 0x0028: 1 word, ReadWrite.
pub mod iprd_wgty16 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_wgty16", 10, 1, Access::ReadWrite, 1);
    pub const IPRD_WGTY16: [Field; 4] = [
        Field::new(Region::L2, 10, 0, 8, false, true, &[]),
        Field::new(Region::L2, 10, 8, 8, false, true, &[]),
        Field::new(Region::L2, 10, 16, 8, false, true, &[]),
        Field::new(Region::L2, 10, 24, 8, false, true, &[]),
    ];
}

/// Register `iprd_wgtc8` at 0x002c: 1 word, ReadWrite.
pub mod iprd_wgtc8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_wgtc8", 11, 1, Access::ReadWrite, 1);
    pub const IPRD_WGTC8: [Field; 4] = [
        Field::new(Region::L2, 11, 0, 8, false, true, &[]),
        Field::new(Region::L2, 11, 8, 8, false, true, &[]),
        Field::new(Region::L2, 11, 16, 8, false, true, &[]),
        Field::new(Region::L2, 11, 24, 8, false, true, &[]),
    ];
}

/// Register `qnt_bias_comb` at 0x0030: 1 word, ReadWrite.
pub mod qnt_bias_comb {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "qnt_bias_comb", 12, 1, Access::ReadWrite, 1);
    pub const QNT_BIAS_I: Field = Field::new(Region::L2, 12, 0, 10, false, true, &[]);
    pub const QNT_BIAS_P: Field = Field::new(Region::L2, 12, 10, 10, false, true, &[]);
}

/// Register `atr_thd0_h264` at 0x0034: 1 word, ReadWrite.
pub mod atr_thd0_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_thd0_h264", 13, 1, Access::ReadWrite, 1);
    pub const ATR_THD0: Field = Field::new(Region::L2, 13, 0, 12, false, true, &[]);
    pub const ATR_THD1: Field = Field::new(Region::L2, 13, 16, 12, false, true, &[]);
}

/// Register `atr_thd1_h264` at 0x0038: 1 word, ReadWrite.
pub mod atr_thd1_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_thd1_h264", 14, 1, Access::ReadWrite, 1);
    pub const ATR_THD2: Field = Field::new(Region::L2, 14, 0, 12, false, true, &[]);
    pub const ATR_QP: Field = Field::new(Region::L2, 14, 16, 6, false, true, &[]);
}

/// Register `atr_wgt16_h264` at 0x003c: 1 word, ReadWrite.
pub mod atr_wgt16_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_wgt16_h264", 15, 1, Access::ReadWrite, 1);
    pub const ATR_LV16_WGT0: Field = Field::new(Region::L2, 15, 0, 8, false, true, &[]);
    pub const ATR_LV16_WGT1: Field = Field::new(Region::L2, 15, 8, 8, false, true, &[]);
    pub const ATR_LV16_WGT2: Field = Field::new(Region::L2, 15, 16, 8, false, true, &[]);
}

/// Register `atr_wgt8_h264` at 0x0040: 1 word, ReadWrite.
pub mod atr_wgt8_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_wgt8_h264", 16, 1, Access::ReadWrite, 1);
    pub const ATR_LV8_WGT0: Field = Field::new(Region::L2, 16, 0, 8, false, true, &[]);
    pub const ATR_LV8_WGT1: Field = Field::new(Region::L2, 16, 8, 8, false, true, &[]);
    pub const ATR_LV8_WGT2: Field = Field::new(Region::L2, 16, 16, 8, false, true, &[]);
}

/// Register `atr_wgt4_h264` at 0x0044: 1 word, ReadWrite.
pub mod atr_wgt4_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr_wgt4_h264", 17, 1, Access::ReadWrite, 1);
    pub const ATR_LV4_WGT0: Field = Field::new(Region::L2, 17, 0, 8, false, true, &[]);
    pub const ATR_LV4_WGT1: Field = Field::new(Region::L2, 17, 8, 8, false, true, &[]);
    pub const ATR_LV4_WGT2: Field = Field::new(Region::L2, 17, 16, 8, false, true, &[]);
}

/// Register `atf_tthd` at 0x0048: 2 words, ReadWrite.
pub mod atf_tthd {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_tthd", 18, 2, Access::ReadWrite, 1);
    pub const ATF_TTHD: [Field; 4] = [
        Field::new(Region::L2, 18, 0, 16, false, true, &[]),
        Field::new(Region::L2, 18, 16, 16, false, true, &[]),
        Field::new(Region::L2, 19, 0, 16, false, true, &[]),
        Field::new(Region::L2, 19, 16, 16, false, true, &[]),
    ];
}

/// Register `atf_sthd0_h264` at 0x0050: 1 word, ReadWrite.
pub mod atf_sthd0_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sthd0_h264", 20, 1, Access::ReadWrite, 1);
    pub const ATF_STHD_10: Field = Field::new(Region::L2, 20, 0, 14, false, true, &[]);
    pub const ATF_STHD_MAX: Field = Field::new(Region::L2, 20, 16, 14, false, true, &[]);
}

/// Register `atf_sthd1_h264` at 0x0054: 1 word, ReadWrite.
pub mod atf_sthd1_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_sthd1_h264", 21, 1, Access::ReadWrite, 1);
    pub const ATF_STHD_11: Field = Field::new(Region::L2, 21, 0, 14, false, true, &[]);
    pub const ATF_STHD_20: Field = Field::new(Region::L2, 21, 16, 14, false, true, &[]);
}

/// Register `atf_wgt0_h264` at 0x0058: 1 word, ReadWrite.
pub mod atf_wgt0_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_wgt0_h264", 22, 1, Access::ReadWrite, 1);
    pub const ATF_WGT10: Field = Field::new(Region::L2, 22, 0, 9, false, true, &[]);
    pub const ATF_WGT11: Field = Field::new(Region::L2, 22, 16, 9, false, true, &[]);
}

/// Register `atf_wgt1_h264` at 0x005c: 1 word, ReadWrite.
pub mod atf_wgt1_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_wgt1_h264", 23, 1, Access::ReadWrite, 1);
    pub const ATF_WGT12: Field = Field::new(Region::L2, 23, 0, 9, false, true, &[]);
    pub const ATF_WGT20: Field = Field::new(Region::L2, 23, 16, 9, false, true, &[]);
}

/// Register `atf_wgt2_h264` at 0x0060: 1 word, ReadWrite.
pub mod atf_wgt2_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_wgt2_h264", 24, 1, Access::ReadWrite, 1);
    pub const ATF_WGT21: Field = Field::new(Region::L2, 24, 0, 9, false, true, &[]);
    pub const ATF_WGT30: Field = Field::new(Region::L2, 24, 16, 9, false, true, &[]);
}

/// Register `atf_ofst0_h264` at 0x0064: 1 word, ReadWrite.
pub mod atf_ofst0_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_ofst0_h264", 25, 1, Access::ReadWrite, 1);
    pub const ATF_OFST10: Field = Field::new(Region::L2, 25, 0, 14, false, true, &[]);
    pub const ATF_OFST11: Field = Field::new(Region::L2, 25, 16, 14, false, true, &[]);
}

/// Register `atf_ofst1_h264` at 0x0068: 1 word, ReadWrite.
pub mod atf_ofst1_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_ofst1_h264", 26, 1, Access::ReadWrite, 1);
    pub const ATF_OFST12: Field = Field::new(Region::L2, 26, 0, 14, false, true, &[]);
    pub const ATF_OFST20: Field = Field::new(Region::L2, 26, 16, 14, false, true, &[]);
}

/// Register `atf_ofst2_h264` at 0x006c: 1 word, ReadWrite.
pub mod atf_ofst2_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atf_ofst2_h264", 27, 1, Access::ReadWrite, 1);
    pub const ATF_OFST21: Field = Field::new(Region::L2, 27, 0, 14, false, true, &[]);
    pub const ATF_OFST30: Field = Field::new(Region::L2, 27, 16, 14, false, true, &[]);
}

/// Register `iprd_wgt_qp` at 0x0070: 52 words, ReadWrite.
pub mod iprd_wgt_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "iprd_wgt_qp", 28, 52, Access::ReadWrite, 1);
    pub const IPRD_WGT_QP: [Field; 52] = [
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

/// Register `wgt_qp_grpa` at 0x0140: 52 words, ReadWrite.
pub mod wgt_qp_grpa {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "wgt_qp_grpa", 80, 52, Access::ReadWrite, 1);
    pub const WGT_QP_GRPA: [Field; 52] = [
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

/// Register `wgt_qp_grpb` at 0x0210: 52 words, ReadWrite.
pub mod wgt_qp_grpb {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "wgt_qp_grpb", 132, 52, Access::ReadWrite, 1);
    pub const WGT_QP_GRPB: [Field; 52] = [
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

/// Register `madi_mode` at 0x02e0: 1 word, ReadWrite.
pub mod madi_mode {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "madi_mode", 184, 1, Access::ReadWrite, 1);
    pub const MADI_MODE: Field = Field::new(Region::L2, 184, 0, 32, false, true, &[]);
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
        Field::new(Region::L2, 189, 0, 8, true, true, &[]),
        Field::new(Region::L2, 189, 8, 8, true, true, &[]),
        Field::new(Region::L2, 189, 16, 8, true, true, &[]),
        Field::new(Region::L2, 189, 24, 8, true, true, &[]),
        Field::new(Region::L2, 190, 0, 8, true, true, &[]),
        Field::new(Region::L2, 190, 8, 8, true, true, &[]),
        Field::new(Region::L2, 190, 16, 8, true, true, &[]),
        Field::new(Region::L2, 190, 24, 8, true, true, &[]),
        Field::new(Region::L2, 191, 0, 8, true, true, &[]),
        Field::new(Region::L2, 191, 8, 8, true, true, &[]),
        Field::new(Region::L2, 191, 16, 8, true, true, &[]),
        Field::new(Region::L2, 191, 24, 8, true, true, &[]),
        Field::new(Region::L2, 192, 0, 8, true, true, &[]),
        Field::new(Region::L2, 192, 8, 8, true, true, &[]),
        Field::new(Region::L2, 192, 16, 8, true, true, &[]),
        Field::new(Region::L2, 192, 24, 8, true, true, &[]),
    ];
}

/// Register `rme_mvd_penalty` at 0x0304: 1 word, ReadWrite.
pub mod rme_mvd_penalty {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "rme_mvd_penalty", 193, 1, Access::ReadWrite, 1);
    pub const MVD_PNLT_E: Field = Field::new(Region::L2, 193, 0, 1, false, true, &[]);
    pub const MVD_PNLT_COEF: Field = Field::new(Region::L2, 193, 1, 5, false, true, &[]);
    pub const MVD_PNLT_CNST: Field = Field::new(Region::L2, 193, 6, 14, false, true, &[]);
    pub const MVD_PNLT_LTHD: Field = Field::new(Region::L2, 193, 20, 4, false, true, &[]);
    pub const MVD_PNLT_HTHD: Field = Field::new(Region::L2, 193, 24, 4, false, true, &[]);
}

/// Register `atr1_thd0_h264` at 0x0308: 1 word, ReadWrite.
pub mod atr1_thd0_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr1_thd0_h264", 194, 1, Access::ReadWrite, 1);
    pub const ATR1_THD0: Field = Field::new(Region::L2, 194, 0, 12, false, true, &[]);
    pub const ATR1_THD1: Field = Field::new(Region::L2, 194, 16, 12, false, true, &[]);
}

/// Register `atr1_thd1_h264` at 0x030c: 1 word, ReadWrite.
pub mod atr1_thd1_h264 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::L2, "atr1_thd1_h264", 195, 1, Access::ReadWrite, 1);
    pub const ATR1_THD2: Field = Field::new(Region::L2, 195, 0, 12, false, true, &[]);
}
