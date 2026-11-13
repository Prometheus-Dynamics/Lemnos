//! The VEPU541 H.264 write side: every register of the job, as the header lays it out.

#![allow(clippy::unreadable_literal)]

/// Register `reg000` at 0x0000: 1 word, ReadOnly.
pub mod reg000 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg000", 0, 1, Access::ReadOnly, 1);
    pub const SUB_VER: Field = Field::new(Region::Set, 0, 0, 8, false, false, &[]);
    pub const H264_ENC: Field = Field::new(Region::Set, 0, 8, 1, false, false, &[]);
    pub const H265_ENC: Field = Field::new(Region::Set, 0, 9, 1, false, false, &[]);
    pub const PIC_SIZE: Field = Field::new(Region::Set, 0, 12, 4, false, false, &[0, 1]);
    pub const OSD_CAP: Field = Field::new(Region::Set, 0, 16, 2, false, false, &[0, 3]);
    pub const FILTR_CAP: Field = Field::new(Region::Set, 0, 18, 2, false, false, &[0, 3]);
    pub const BFRM_CAP: Field = Field::new(Region::Set, 0, 20, 1, false, false, &[]);
    pub const FBC_CAP: Field = Field::new(Region::Set, 0, 21, 1, false, false, &[]);
    pub const RKVENC_VER: Field = Field::new(Region::Set, 0, 24, 8, false, false, &[]);
}

/// Register `reg001` at 0x0004: 1 word, ReadWrite.
pub mod reg001 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg001", 1, 1, Access::ReadWrite, 1);
    pub const LKT_NUM: Field = Field::new(Region::Set, 1, 0, 8, false, true, &[]);
    pub const RKVENC_CMD: Field = Field::new(Region::Set, 1, 8, 2, false, true, &[]);
    pub const CLK_GATE_EN: Field = Field::new(Region::Set, 1, 16, 1, false, true, &[]);
    pub const RESETN_HW_EN: Field = Field::new(Region::Set, 1, 17, 1, false, true, &[]);
    pub const ENC_DONE_TMVP_EN: Field = Field::new(Region::Set, 1, 18, 1, false, true, &[]);
}

/// Register `reg002` at 0x0008: 1 word, ReadWrite.
pub mod reg002 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg002", 2, 1, Access::ReadWrite, 1);
    pub const SAFE_CLR: Field = Field::new(Region::Set, 2, 0, 1, false, true, &[]);
    pub const FORCE_CLR: Field = Field::new(Region::Set, 2, 1, 1, false, true, &[]);
}

/// Register `reg003` at 0x000c: 1 word, ReadWrite.
pub mod reg003 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg003", 3, 1, Access::ReadWrite, 1);
    pub const LKT_ADDR: AddressReg = AddressReg::new(Region::Set, 3, 16);
}

/// Register `reg004` at 0x0010: 1 word, ReadWrite.
pub mod reg004 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg004", 4, 1, Access::ReadWrite, 1);
    pub const ENC_DONE_EN: Field = Field::new(Region::Set, 4, 0, 1, false, true, &[]);
    pub const LKT_DONE_EN: Field = Field::new(Region::Set, 4, 1, 1, false, true, &[]);
    pub const SCLR_DONE_EN: Field = Field::new(Region::Set, 4, 2, 1, false, true, &[]);
    pub const ENC_SLICE_DONE_EN: Field = Field::new(Region::Set, 4, 3, 1, false, true, &[]);
    pub const OFLW_DONE_EN: Field = Field::new(Region::Set, 4, 4, 1, false, true, &[]);
    pub const BRSP_DONE_EN: Field = Field::new(Region::Set, 4, 5, 1, false, true, &[]);
    pub const BERR_DONE_EN: Field = Field::new(Region::Set, 4, 6, 1, false, true, &[]);
    pub const RERR_DONE_EN: Field = Field::new(Region::Set, 4, 7, 1, false, true, &[]);
    pub const WDG_DONE_EN: Field = Field::new(Region::Set, 4, 8, 1, false, true, &[]);
}

/// Register `reg005` at 0x0014: 1 word, ReadWrite.
pub mod reg005 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg005", 5, 1, Access::ReadWrite, 1);
    pub const ENC_DONE_MSK: Field = Field::new(Region::Set, 5, 0, 1, false, true, &[]);
    pub const LKT_DONE_MSK: Field = Field::new(Region::Set, 5, 1, 1, false, true, &[]);
    pub const SCLR_DONE_MSK: Field = Field::new(Region::Set, 5, 2, 1, false, true, &[]);
    pub const ENC_SLICE_DONE_MSK: Field = Field::new(Region::Set, 5, 3, 1, false, true, &[]);
    pub const OFLW_DONE_MSK: Field = Field::new(Region::Set, 5, 4, 1, false, true, &[]);
    pub const BRSP_DONE_MSK: Field = Field::new(Region::Set, 5, 5, 1, false, true, &[]);
    pub const BERR_DONE_MSK: Field = Field::new(Region::Set, 5, 6, 1, false, true, &[]);
    pub const RERR_DONE_MSK: Field = Field::new(Region::Set, 5, 7, 1, false, true, &[]);
    pub const WDG_DONE_MSK: Field = Field::new(Region::Set, 5, 8, 1, false, true, &[]);
}

/// Register `reg006` at 0x0018: 1 word, WriteOneToClear.
pub mod reg006 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg006", 6, 1, Access::WriteOneToClear, 1);
    pub const ENC_DONE_CLR: Field = Field::new(Region::Set, 6, 0, 1, false, true, &[]);
    pub const LKT_DONE_CLR: Field = Field::new(Region::Set, 6, 1, 1, false, true, &[]);
    pub const SCLR_DONE_CLR: Field = Field::new(Region::Set, 6, 2, 1, false, true, &[]);
    pub const ENC_SLICE_DONE_CLR: Field = Field::new(Region::Set, 6, 3, 1, false, true, &[]);
    pub const OFLW_DONE_CLR: Field = Field::new(Region::Set, 6, 4, 1, false, true, &[]);
    pub const BRSP_DONE_CLR: Field = Field::new(Region::Set, 6, 5, 1, false, true, &[]);
    pub const BERR_DONE_CLR: Field = Field::new(Region::Set, 6, 6, 1, false, true, &[]);
    pub const RERR_DONE_CLR: Field = Field::new(Region::Set, 6, 7, 1, false, true, &[]);
    pub const WDG_DONE_CLR: Field = Field::new(Region::Set, 6, 8, 1, false, true, &[]);
}

/// Register `reg007` at 0x001c: 1 word, WriteOneToClear.
pub mod reg007 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg007", 7, 1, Access::WriteOneToClear, 1);
    pub const ENC_DONE_STA: Field = Field::new(Region::Set, 7, 0, 1, false, true, &[]);
    pub const LKT_DONE_STA: Field = Field::new(Region::Set, 7, 1, 1, false, true, &[]);
    pub const SCLR_DONE_STA: Field = Field::new(Region::Set, 7, 2, 1, false, true, &[]);
    pub const ENC_SLICE_DONE_STA: Field = Field::new(Region::Set, 7, 3, 1, false, true, &[]);
    pub const OFLW_DONE_STA: Field = Field::new(Region::Set, 7, 4, 1, false, true, &[]);
    pub const BRSP_DONE_STA: Field = Field::new(Region::Set, 7, 5, 1, false, true, &[]);
    pub const BERR_DONE_STA: Field = Field::new(Region::Set, 7, 6, 1, false, true, &[]);
    pub const RERR_DONE_STA: Field = Field::new(Region::Set, 7, 7, 1, false, true, &[]);
    pub const WDG_DONE_STA: Field = Field::new(Region::Set, 7, 8, 1, false, true, &[]);
}

/// Register `reg_008_011` at 0x0020: 4 words, Unknown.
pub mod reg_008_011 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg_008_011", 8, 4, Access::Unknown, 1);
    pub const REG_008_011: [Field; 4] = [
        Field::new(Region::Set, 8, 0, 32, false, false, &[]),
        Field::new(Region::Set, 9, 0, 32, false, false, &[]),
        Field::new(Region::Set, 10, 0, 32, false, false, &[]),
        Field::new(Region::Set, 11, 0, 32, false, false, &[]),
    ];
}

/// Register `reg012` at 0x0030: 1 word, ReadWrite.
pub mod reg012 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg012", 12, 1, Access::ReadWrite, 1);
    pub const PIC_WD8_M1: Field = Field::new(Region::Set, 12, 0, 9, false, true, &[]);
    pub const PIC_WFILL: Field = Field::new(Region::Set, 12, 10, 6, false, true, &[]);
    pub const PIC_HD8_M1: Field = Field::new(Region::Set, 12, 16, 9, false, true, &[]);
    pub const PIC_HFILL: Field = Field::new(Region::Set, 12, 26, 6, false, true, &[]);
}

/// Register `reg013` at 0x0034: 1 word, ReadWrite.
pub mod reg013 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg013", 13, 1, Access::ReadWrite, 1);
    pub const ENC_STND: Field = Field::new(Region::Set, 13, 0, 1, false, true, &[]);
    pub const ROI_ENC: Field = Field::new(Region::Set, 13, 1, 1, false, true, &[]);
    pub const CUR_FRM_REF: Field = Field::new(Region::Set, 13, 2, 1, false, true, &[]);
    pub const MEI_STOR: Field = Field::new(Region::Set, 13, 3, 1, false, true, &[]);
    pub const BS_SCP: Field = Field::new(Region::Set, 13, 4, 1, false, true, &[]);
    pub const LAMB_MOD_SEL: Field = Field::new(Region::Set, 13, 5, 1, false, true, &[]);
    pub const PIC_QP: Field = Field::new(Region::Set, 13, 8, 6, false, true, &[]);
    pub const TOT_POC_NUM: Field = Field::new(Region::Set, 13, 14, 5, false, true, &[]);
    pub const LOG2_CTU_NUM: Field = Field::new(Region::Set, 13, 19, 4, false, true, &[]);
    pub const ATR_THD_SEL: Field = Field::new(Region::Set, 13, 23, 1, false, true, &[]);
    pub const DCHS_RXID: Field = Field::new(Region::Set, 13, 24, 2, false, true, &[]);
    pub const DCHS_TXID: Field = Field::new(Region::Set, 13, 26, 2, false, true, &[]);
    pub const DCHS_RXE: Field = Field::new(Region::Set, 13, 28, 1, false, true, &[]);
    pub const SATD_BYPS_EN: Field = Field::new(Region::Set, 13, 29, 1, false, true, &[]);
    pub const SLEN_FIFO: Field = Field::new(Region::Set, 13, 30, 1, false, true, &[]);
    pub const NODE_INT: Field = Field::new(Region::Set, 13, 31, 1, false, true, &[]);
}

/// Register `reg014` at 0x0038: 1 word, ReadWrite.
pub mod reg014 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg014", 14, 1, Access::ReadWrite, 1);
    pub const VS_LOAD_THD: Field = Field::new(Region::Set, 14, 0, 24, false, true, &[]);
    pub const RFP_LOAD_THRD: Field = Field::new(Region::Set, 14, 24, 8, false, true, &[]);
}

/// Register `reg015` at 0x003c: 1 word, ReadWrite.
pub mod reg015 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg015", 15, 1, Access::ReadWrite, 1);
    pub const LPFW_BUS_ORDR: Field = Field::new(Region::Set, 15, 0, 1, false, true, &[]);
    pub const CMVW_BUS_ORDR: Field = Field::new(Region::Set, 15, 1, 1, false, true, &[]);
    pub const DSPW_BUS_ORDR: Field = Field::new(Region::Set, 15, 2, 1, false, true, &[]);
    pub const RFPW_BUS_ORDR: Field = Field::new(Region::Set, 15, 3, 1, false, true, &[]);
    pub const SRC_BUS_EDIN: Field = Field::new(Region::Set, 15, 4, 4, false, true, &[]);
    pub const MEIW_BUS_EDIN: Field = Field::new(Region::Set, 15, 8, 4, false, true, &[]);
    pub const BSW_BUS_EDIN: Field = Field::new(Region::Set, 15, 12, 3, false, true, &[]);
    pub const LKTR_BUS_EDIN: Field = Field::new(Region::Set, 15, 15, 4, false, true, &[]);
    pub const ROIR_BUS_EDIN: Field = Field::new(Region::Set, 15, 19, 4, false, true, &[]);
    pub const LKTW_BUS_EDIN: Field = Field::new(Region::Set, 15, 23, 4, false, true, &[]);
    pub const AFBC_BSIZE: Field = Field::new(Region::Set, 15, 27, 1, false, true, &[]);
}

/// Register `reg016` at 0x0040: 1 word, ReadWrite.
pub mod reg016 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg016", 16, 1, Access::ReadWrite, 1);
    pub const VEPU541_AXI_BRSP_CKE: Field = Field::new(Region::Set, 16, 0, 7, false, true, &[]);
    pub const VEPU541_DSPR_OTSD: Field = Field::new(Region::Set, 16, 7, 1, false, true, &[]);
    pub const VEPU540_DSPR_OTSD: Field = Field::new(Region::Set, 16, 7, 1, false, true, &[]);
    pub const VEPU540_AXI_BRSP_CKE: Field = Field::new(Region::Set, 16, 16, 8, false, true, &[]);
}

/// Register `reg017` at 0x0044: 1 word, ReadWrite.
pub mod reg017 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg017", 17, 1, Access::ReadWrite, 1);
    pub const ALPHA_SWAP: Field = Field::new(Region::Set, 17, 0, 1, false, true, &[]);
    pub const RBUV_SWAP: Field = Field::new(Region::Set, 17, 1, 1, false, true, &[]);
    pub const SRC_CFMT: Field = Field::new(
        Region::Set,
        17,
        2,
        4,
        false,
        true,
        &[0, 1, 2, 4, 5, 6, 7, 8, 9],
    );
    pub const SRC_RANGE: Field = Field::new(Region::Set, 17, 6, 1, false, true, &[]);
    pub const OUT_FMT_CFG: Field = Field::new(Region::Set, 17, 7, 1, false, true, &[]);
}

/// Register `reg018` at 0x0048: 1 word, ReadWrite.
pub mod reg018 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg018", 18, 1, Access::ReadWrite, 1);
    pub const CSC_WGT_B2Y: Field = Field::new(Region::Set, 18, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2Y: Field = Field::new(Region::Set, 18, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2Y: Field = Field::new(Region::Set, 18, 18, 9, false, true, &[]);
}

/// Register `reg019` at 0x004c: 1 word, ReadWrite.
pub mod reg019 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg019", 19, 1, Access::ReadWrite, 1);
    pub const CSC_WGT_B2U: Field = Field::new(Region::Set, 19, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2U: Field = Field::new(Region::Set, 19, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2U: Field = Field::new(Region::Set, 19, 18, 9, false, true, &[]);
}

/// Register `reg020` at 0x0050: 1 word, ReadWrite.
pub mod reg020 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg020", 20, 1, Access::ReadWrite, 1);
    pub const CSC_WGT_B2V: Field = Field::new(Region::Set, 20, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2V: Field = Field::new(Region::Set, 20, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2V: Field = Field::new(Region::Set, 20, 18, 9, false, true, &[]);
}

/// Register `reg021` at 0x0054: 1 word, ReadWrite.
pub mod reg021 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg021", 21, 1, Access::ReadWrite, 1);
    pub const CSC_OFST_V: Field = Field::new(Region::Set, 21, 0, 8, false, true, &[]);
    pub const CSC_OFST_U: Field = Field::new(Region::Set, 21, 8, 8, false, true, &[]);
    pub const CSC_OFST_Y: Field = Field::new(Region::Set, 21, 16, 5, false, true, &[]);
}

/// Register `reg022` at 0x0058: 1 word, ReadWrite.
pub mod reg022 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg022", 22, 1, Access::ReadWrite, 1);
    pub const SRC_MIRR: Field = Field::new(Region::Set, 22, 26, 1, false, true, &[]);
    pub const SRC_ROT: Field = Field::new(Region::Set, 22, 27, 2, false, true, &[]);
    pub const TXA_EN: Field = Field::new(Region::Set, 22, 29, 1, false, true, &[]);
    pub const AFBCD_EN: Field = Field::new(Region::Set, 22, 30, 1, false, true, &[]);
}

/// Register `reg023` at 0x005c: 1 word, ReadWrite.
pub mod reg023 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg023", 23, 1, Access::ReadWrite, 1);
    pub const SLI_CRS_EN: Field = Field::new(Region::Set, 23, 31, 1, false, true, &[]);
}

/// Register `reg_024` at 0x0060: 1 word, Unknown.
pub mod reg_024 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "reg_024", 24, 1, Access::Unknown, 1);
    pub const REG_024: Field = Field::new(Region::Set, 24, 0, 32, false, false, &[]);
}

/// Register `reg025` at 0x0064: 1 word, ReadWrite.
pub mod reg025 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg025", 25, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_OFST: Field = Field::new(Region::Set, 25, 0, 3, false, true, &[]);
}

/// Register `reg026` at 0x0068: 1 word, ReadWrite.
pub mod reg026 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg026", 26, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT0: Field = Field::new(Region::Set, 26, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT1_L9: Field = Field::new(Region::Set, 26, 23, 9, false, true, &[]);
}

/// Register `reg027` at 0x006c: 1 word, ReadWrite.
pub mod reg027 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg027", 27, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT1_H9: Field = Field::new(Region::Set, 27, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT2: Field = Field::new(Region::Set, 27, 14, 18, false, true, &[]);
}

/// Register `reg028` at 0x0070: 1 word, ReadWrite.
pub mod reg028 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg028", 28, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT3: Field = Field::new(Region::Set, 28, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT4_L9: Field = Field::new(Region::Set, 28, 23, 9, false, true, &[]);
}

/// Register `reg029` at 0x0074: 1 word, ReadWrite.
pub mod reg029 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg029", 29, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT4_H9: Field = Field::new(Region::Set, 29, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT5: Field = Field::new(Region::Set, 29, 14, 18, false, true, &[]);
}

/// Register `reg030` at 0x0078: 1 word, ReadWrite.
pub mod reg030 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg030", 30, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT6: Field = Field::new(Region::Set, 30, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT7_L9: Field = Field::new(Region::Set, 30, 23, 9, false, true, &[]);
}

/// Register `reg031` at 0x007c: 1 word, ReadWrite.
pub mod reg031 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg031", 31, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT7_H9: Field = Field::new(Region::Set, 31, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT8: Field = Field::new(Region::Set, 31, 14, 18, false, true, &[]);
}

/// Register `reg032` at 0x0080: 1 word, ReadWrite.
pub mod reg032 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg032", 32, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT9: Field = Field::new(Region::Set, 32, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT10_L9: Field = Field::new(Region::Set, 32, 23, 9, false, true, &[]);
}

/// Register `reg033` at 0x0084: 1 word, ReadWrite.
pub mod reg033 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg033", 33, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT10_H9: Field = Field::new(Region::Set, 33, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT11: Field = Field::new(Region::Set, 33, 14, 18, false, true, &[]);
}

/// Register `reg034` at 0x0088: 1 word, ReadWrite.
pub mod reg034 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg034", 34, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT12: Field = Field::new(Region::Set, 34, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT13_L9: Field = Field::new(Region::Set, 34, 23, 9, false, true, &[]);
}

/// Register `reg035` at 0x008c: 1 word, ReadWrite.
pub mod reg035 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg035", 35, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT13_H9: Field = Field::new(Region::Set, 35, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT14: Field = Field::new(Region::Set, 35, 14, 18, false, true, &[]);
}

/// Register `reg036` at 0x0090: 1 word, ReadWrite.
pub mod reg036 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg036", 36, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT15: Field = Field::new(Region::Set, 36, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT16_L9: Field = Field::new(Region::Set, 36, 23, 9, false, true, &[]);
}

/// Register `reg037` at 0x0094: 1 word, ReadWrite.
pub mod reg037 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg037", 37, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT16_H9: Field = Field::new(Region::Set, 37, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT17: Field = Field::new(Region::Set, 37, 14, 18, false, true, &[]);
}

/// Register `reg038` at 0x0098: 1 word, ReadWrite.
pub mod reg038 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg038", 38, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT18: Field = Field::new(Region::Set, 38, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT19_L9: Field = Field::new(Region::Set, 38, 23, 9, false, true, &[]);
}

/// Register `reg039` at 0x009c: 1 word, ReadWrite.
pub mod reg039 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg039", 39, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT19_H9: Field = Field::new(Region::Set, 39, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT20: Field = Field::new(Region::Set, 39, 14, 18, false, true, &[]);
}

/// Register `reg040` at 0x00a0: 1 word, ReadWrite.
pub mod reg040 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg040", 40, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT21: Field = Field::new(Region::Set, 40, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT22_L9: Field = Field::new(Region::Set, 40, 23, 9, false, true, &[]);
}

/// Register `reg041` at 0x00a4: 1 word, ReadWrite.
pub mod reg041 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg041", 41, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT22_H9: Field = Field::new(Region::Set, 41, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT23: Field = Field::new(Region::Set, 41, 14, 18, false, true, &[]);
}

/// Register `reg042` at 0x00a8: 1 word, ReadWrite.
pub mod reg042 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg042", 42, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT24: Field = Field::new(Region::Set, 42, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT25_L9: Field = Field::new(Region::Set, 42, 23, 9, false, true, &[]);
}

/// Register `reg043` at 0x00ac: 1 word, ReadWrite.
pub mod reg043 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg043", 43, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT25_H9: Field = Field::new(Region::Set, 43, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT26: Field = Field::new(Region::Set, 43, 14, 18, false, true, &[]);
}

/// Register `reg044` at 0x00b0: 1 word, ReadWrite.
pub mod reg044 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg044", 44, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT27: Field = Field::new(Region::Set, 44, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT28_L9: Field = Field::new(Region::Set, 44, 23, 9, false, true, &[]);
}

/// Register `reg045` at 0x00b4: 1 word, ReadWrite.
pub mod reg045 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg045", 45, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT28_H9: Field = Field::new(Region::Set, 45, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT29: Field = Field::new(Region::Set, 45, 14, 18, false, true, &[]);
}

/// Register `reg046` at 0x00b8: 1 word, ReadWrite.
pub mod reg046 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg046", 46, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT30: Field = Field::new(Region::Set, 46, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT31_L9: Field = Field::new(Region::Set, 46, 23, 9, false, true, &[]);
}

/// Register `reg047` at 0x00bc: 1 word, ReadWrite.
pub mod reg047 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg047", 47, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT31_H9: Field = Field::new(Region::Set, 47, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT32: Field = Field::new(Region::Set, 47, 14, 18, false, true, &[]);
}

/// Register `reg048` at 0x00c0: 1 word, ReadWrite.
pub mod reg048 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg048", 48, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT33: Field = Field::new(Region::Set, 48, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT34_L9: Field = Field::new(Region::Set, 48, 23, 9, false, true, &[]);
}

/// Register `reg049` at 0x00c4: 1 word, ReadWrite.
pub mod reg049 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg049", 49, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT34_H9: Field = Field::new(Region::Set, 49, 0, 9, false, true, &[]);
}

/// Register `reg050` at 0x00c8: 1 word, ReadWrite.
pub mod reg050 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg050", 50, 1, Access::ReadWrite, 1);
    pub const RC_EN: Field = Field::new(Region::Set, 50, 0, 1, false, true, &[]);
    pub const AQ_EN: Field = Field::new(Region::Set, 50, 1, 1, false, true, &[]);
    pub const AQ_MODE: Field = Field::new(Region::Set, 50, 2, 1, false, true, &[]);
    pub const RC_CTU_NUM: Field = Field::new(Region::Set, 50, 16, 16, false, true, &[]);
}

/// Register `reg051` at 0x00cc: 1 word, ReadWrite.
pub mod reg051 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg051", 51, 1, Access::ReadWrite, 1);
    pub const RC_QP_RANGE: Field = Field::new(Region::Set, 51, 16, 4, true, true, &[]);
    pub const RC_MAX_QP: Field = Field::new(Region::Set, 51, 20, 6, false, true, &[]);
    pub const RC_MIN_QP: Field = Field::new(Region::Set, 51, 26, 6, false, true, &[]);
}

/// Register `reg052` at 0x00d0: 1 word, ReadWrite.
pub mod reg052 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg052", 52, 1, Access::ReadWrite, 1);
    pub const CTU_EBIT: Field = Field::new(Region::Set, 52, 0, 20, false, true, &[]);
}

/// Register `reg053` at 0x00d4: 1 word, ReadWrite.
pub mod reg053 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg053", 53, 1, Access::ReadWrite, 1);
    pub const QP_ADJ0: Field = Field::new(Region::Set, 53, 0, 5, true, true, &[]);
    pub const QP_ADJ1: Field = Field::new(Region::Set, 53, 5, 5, true, true, &[]);
    pub const QP_ADJ2: Field = Field::new(Region::Set, 53, 10, 5, true, true, &[]);
    pub const QP_ADJ3: Field = Field::new(Region::Set, 53, 15, 5, true, true, &[]);
    pub const QP_ADJ4: Field = Field::new(Region::Set, 53, 20, 5, true, true, &[]);
}

/// Register `reg054` at 0x00d8: 1 word, ReadWrite.
pub mod reg054 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg054", 54, 1, Access::ReadWrite, 1);
    pub const QP_ADJ5: Field = Field::new(Region::Set, 54, 0, 5, true, true, &[]);
    pub const QP_ADJ6: Field = Field::new(Region::Set, 54, 5, 5, true, true, &[]);
    pub const QP_ADJ7: Field = Field::new(Region::Set, 54, 10, 5, true, true, &[]);
    pub const QP_ADJ8: Field = Field::new(Region::Set, 54, 15, 5, true, true, &[]);
}

/// Register `reg055_063` at 0x00dc: 9 words, ReadWrite.
pub mod reg055_063 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg055_063", 55, 9, Access::ReadWrite, 1);
    pub const RC_DTHD: [Field; 9] = [
        Field::new(Region::Set, 55, 0, 32, false, true, &[]),
        Field::new(Region::Set, 56, 0, 32, false, true, &[]),
        Field::new(Region::Set, 57, 0, 32, false, true, &[]),
        Field::new(Region::Set, 58, 0, 32, false, true, &[]),
        Field::new(Region::Set, 59, 0, 32, false, true, &[]),
        Field::new(Region::Set, 60, 0, 32, false, true, &[]),
        Field::new(Region::Set, 61, 0, 32, false, true, &[]),
        Field::new(Region::Set, 62, 0, 32, false, true, &[]),
        Field::new(Region::Set, 63, 0, 32, false, true, &[]),
    ];
}

/// Register `reg064` at 0x0100: 1 word, ReadWrite.
pub mod reg064 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg064", 64, 1, Access::ReadWrite, 1);
    pub const QPMIN_AREA0: Field = Field::new(Region::Set, 64, 0, 6, false, true, &[]);
    pub const QPMAX_AREA0: Field = Field::new(Region::Set, 64, 6, 6, false, true, &[]);
    pub const QPMIN_AREA1: Field = Field::new(Region::Set, 64, 12, 6, false, true, &[]);
    pub const QPMAX_AREA1: Field = Field::new(Region::Set, 64, 18, 6, false, true, &[]);
    pub const QPMIN_AREA2: Field = Field::new(Region::Set, 64, 24, 6, false, true, &[]);
}

/// Register `reg065` at 0x0104: 1 word, ReadWrite.
pub mod reg065 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg065", 65, 1, Access::ReadWrite, 1);
    pub const QPMAX_AREA2: Field = Field::new(Region::Set, 65, 0, 6, false, true, &[]);
    pub const QPMIN_AREA3: Field = Field::new(Region::Set, 65, 6, 6, false, true, &[]);
    pub const QPMAX_AREA3: Field = Field::new(Region::Set, 65, 12, 6, false, true, &[]);
    pub const QPMIN_AREA4: Field = Field::new(Region::Set, 65, 18, 6, false, true, &[]);
    pub const QPMAX_AREA4: Field = Field::new(Region::Set, 65, 24, 6, false, true, &[]);
}

/// Register `reg066` at 0x0108: 1 word, ReadWrite.
pub mod reg066 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg066", 66, 1, Access::ReadWrite, 1);
    pub const QPMIN_AREA5: Field = Field::new(Region::Set, 66, 0, 6, false, true, &[]);
    pub const QPMAX_AREA5: Field = Field::new(Region::Set, 66, 6, 6, false, true, &[]);
    pub const QPMIN_AREA6: Field = Field::new(Region::Set, 66, 12, 6, false, true, &[]);
    pub const QPMAX_AREA6: Field = Field::new(Region::Set, 66, 18, 6, false, true, &[]);
    pub const QPMIN_AREA7: Field = Field::new(Region::Set, 66, 24, 6, false, true, &[]);
}

/// Register `reg067` at 0x010c: 1 word, ReadWrite.
pub mod reg067 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg067", 67, 1, Access::ReadWrite, 1);
    pub const QPMAX_AREA7: Field = Field::new(Region::Set, 67, 0, 6, false, true, &[]);
    pub const QPMAP_MODE: Field = Field::new(Region::Set, 67, 30, 2, false, true, &[]);
}

/// Register `reg068` at 0x0110: 1 word, ReadWrite.
pub mod reg068 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg068", 68, 1, Access::ReadWrite, 1);
    pub const PIC_OFST_Y: Field = Field::new(Region::Set, 68, 0, 13, false, true, &[]);
    pub const PIC_OFST_X: Field = Field::new(Region::Set, 68, 16, 13, false, true, &[]);
}

/// Register `reg069` at 0x0114: 1 word, ReadWrite.
pub mod reg069 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg069", 69, 1, Access::ReadWrite, 1);
    pub const SRC_STRD0: Field = Field::new(Region::Set, 69, 0, 16, false, true, &[]);
    pub const SRC_STRD1: Field = Field::new(Region::Set, 69, 16, 16, false, true, &[]);
}

/// Register `reg070` at 0x0118: 1 word, ReadWrite.
pub mod reg070 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg070", 70, 1, Access::ReadWrite, 1);
    pub const ADR_SRC0: AddressReg = AddressReg::new(Region::Set, 70, 1);
}

/// Register `reg071` at 0x011c: 1 word, ReadWrite.
pub mod reg071 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg071", 71, 1, Access::ReadWrite, 1);
    pub const ADR_SRC1: AddressReg = AddressReg::new(Region::Set, 71, 1);
}

/// Register `reg072` at 0x0120: 1 word, ReadWrite.
pub mod reg072 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg072", 72, 1, Access::ReadWrite, 1);
    pub const ADR_SRC2: AddressReg = AddressReg::new(Region::Set, 72, 1);
}

/// Register `reg073` at 0x0124: 1 word, ReadWrite.
pub mod reg073 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg073", 73, 1, Access::ReadWrite, 1);
    pub const ROI_ADDR: AddressReg = AddressReg::new(Region::Set, 73, 16);
}

/// Register `reg074` at 0x0128: 1 word, ReadWrite.
pub mod reg074 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg074", 74, 1, Access::ReadWrite, 1);
    pub const RFPW_H_ADDR: AddressReg = AddressReg::new(Region::Set, 74, 4096);
}

/// Register `reg075` at 0x012c: 1 word, ReadWrite.
pub mod reg075 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg075", 75, 1, Access::ReadWrite, 1);
    pub const RFPW_B_ADDR: AddressReg = AddressReg::new(Region::Set, 75, 4096);
}

/// Register `reg076` at 0x0130: 1 word, ReadWrite.
pub mod reg076 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg076", 76, 1, Access::ReadWrite, 1);
    pub const RFPR_H_ADDR: AddressReg = AddressReg::new(Region::Set, 76, 4096);
}

/// Register `reg077` at 0x0134: 1 word, ReadWrite.
pub mod reg077 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg077", 77, 1, Access::ReadWrite, 1);
    pub const RFPR_B_ADDR: AddressReg = AddressReg::new(Region::Set, 77, 4096);
}

/// Register `reg078` at 0x0138: 1 word, ReadWrite.
pub mod reg078 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg078", 78, 1, Access::ReadWrite, 1024);
    pub const CMVW_ADDR: AddressReg = AddressReg::new(Region::Set, 78, 1024);
}

/// Register `reg079` at 0x013c: 1 word, ReadWrite.
pub mod reg079 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg079", 79, 1, Access::ReadWrite, 1024);
    pub const CMVR_ADDR: AddressReg = AddressReg::new(Region::Set, 79, 1024);
}

/// Register `reg080` at 0x0140: 1 word, ReadWrite.
pub mod reg080 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg080", 80, 1, Access::ReadWrite, 1024);
    pub const DSPW_ADDR: AddressReg = AddressReg::new(Region::Set, 80, 1024);
}

/// Register `reg081` at 0x0144: 1 word, ReadWrite.
pub mod reg081 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg081", 81, 1, Access::ReadWrite, 1024);
    pub const DSPR_ADDR: AddressReg = AddressReg::new(Region::Set, 81, 1024);
}

/// Register `reg082` at 0x0148: 1 word, ReadWrite.
pub mod reg082 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg082", 82, 1, Access::ReadWrite, 1024);
    pub const MEIW_ADDR: AddressReg = AddressReg::new(Region::Set, 82, 1024);
}

/// Register `reg083` at 0x014c: 1 word, ReadWrite.
pub mod reg083 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg083", 83, 1, Access::ReadWrite, 128);
    pub const BSBT_ADDR: AddressReg = AddressReg::new(Region::Set, 83, 128);
}

/// Register `reg084` at 0x0150: 1 word, ReadWrite.
pub mod reg084 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg084", 84, 1, Access::ReadWrite, 128);
    pub const BSBB_ADDR: AddressReg = AddressReg::new(Region::Set, 84, 128);
}

/// Register `reg085` at 0x0154: 1 word, ReadWrite.
pub mod reg085 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg085", 85, 1, Access::ReadWrite, 128);
    pub const BSBR_ADDR: AddressReg = AddressReg::new(Region::Set, 85, 128);
}

/// Register `reg086` at 0x0158: 1 word, ReadWrite.
pub mod reg086 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg086", 86, 1, Access::ReadWrite, 1);
    pub const ADR_BSBS: AddressReg = AddressReg::new(Region::Set, 86, 1);
}

/// Register `reg087` at 0x015c: 1 word, ReadWrite.
pub mod reg087 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg087", 87, 1, Access::ReadWrite, 1);
    pub const SLI_SPLT: Field = Field::new(Region::Set, 87, 0, 1, false, true, &[]);
    pub const SLI_SPLT_MODE: Field = Field::new(Region::Set, 87, 1, 1, false, true, &[]);
    pub const SLI_SPLT_CPST: Field = Field::new(Region::Set, 87, 2, 1, false, true, &[]);
    pub const SLI_MAX_NUM_M1: Field = Field::new(Region::Set, 87, 3, 10, false, true, &[]);
    pub const SLI_FLSH: Field = Field::new(Region::Set, 87, 13, 1, false, true, &[]);
    pub const SLI_SPLT_CNUM_M1: Field = Field::new(Region::Set, 87, 16, 16, false, true, &[]);
}

/// Register `reg088` at 0x0160: 1 word, ReadWrite.
pub mod reg088 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg088", 88, 1, Access::ReadWrite, 1);
    pub const SLI_SPLT_BYTE: Field = Field::new(Region::Set, 88, 0, 18, false, true, &[]);
}

/// Register `reg089` at 0x0164: 1 word, ReadWrite.
pub mod reg089 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg089", 89, 1, Access::ReadWrite, 1);
    pub const CME_SRCH_H: Field = Field::new(Region::Set, 89, 0, 4, false, true, &[]);
    pub const CME_SRCH_V: Field = Field::new(Region::Set, 89, 4, 4, false, true, &[]);
    pub const RME_SRCH_H: Field = Field::new(Region::Set, 89, 8, 3, false, true, &[]);
    pub const RME_SRCH_V: Field = Field::new(Region::Set, 89, 11, 3, false, true, &[]);
    pub const DLT_FRM_NUM: Field = Field::new(Region::Set, 89, 16, 16, false, true, &[]);
}

/// Register `reg090` at 0x0168: 1 word, ReadWrite.
pub mod reg090 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg090", 90, 1, Access::ReadWrite, 1);
    pub const PMV_MDST_H: Field = Field::new(Region::Set, 90, 0, 8, false, true, &[]);
    pub const PMV_MDST_V: Field = Field::new(Region::Set, 90, 8, 8, false, true, &[]);
    pub const MV_LIMIT: Field = Field::new(Region::Set, 90, 16, 2, false, true, &[]);
    pub const PMV_NUM: Field = Field::new(Region::Set, 90, 18, 2, false, true, &[]);
    pub const COLMV_STOR: Field = Field::new(Region::Set, 90, 20, 1, false, true, &[]);
    pub const COLMV_LOAD: Field = Field::new(Region::Set, 90, 21, 1, false, true, &[]);
    pub const RME_DIS: Field = Field::new(Region::Set, 90, 22, 5, false, true, &[]);
    pub const FME_DIS: Field = Field::new(Region::Set, 90, 27, 5, false, true, &[]);
}

/// Register `reg091` at 0x016c: 1 word, ReadWrite.
pub mod reg091 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg091", 91, 1, Access::ReadWrite, 1);
    pub const CME_RAMA_MAX: Field = Field::new(Region::Set, 91, 0, 11, false, true, &[]);
    pub const CME_RAMA_H: Field = Field::new(Region::Set, 91, 11, 5, false, true, &[]);
    pub const CACH_L2_MAP: Field = Field::new(Region::Set, 91, 16, 2, false, true, &[]);
    pub const CME_LINEBUF_W: Field = Field::new(Region::Set, 91, 18, 8, false, true, &[]);
}

/// Register `reg092` at 0x0170: 1 word, ReadWrite.
pub mod reg092 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg092", 92, 1, Access::ReadWrite, 1);
    pub const POC_LSB_LT1: Field = Field::new(Region::Set, 92, 0, 16, false, true, &[]);
    pub const POC_LSB_LT2: Field = Field::new(Region::Set, 92, 16, 16, false, true, &[]);
}

/// Register `reg093` at 0x0174: 1 word, ReadWrite.
pub mod reg093 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg093", 93, 1, Access::ReadWrite, 1);
    pub const DLT_POC_MSB_CYCL1: Field = Field::new(Region::Set, 93, 0, 16, false, true, &[]);
    pub const DLT_POC_MSB_CYCL2: Field = Field::new(Region::Set, 93, 16, 16, false, true, &[]);
}

/// Register `reg094` at 0x0178: 1 word, ReadWrite.
pub mod reg094 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg094", 94, 1, Access::ReadWrite, 1);
    pub const OSD_CH_INV_EN: Field = Field::new(Region::Set, 94, 0, 8, false, true, &[]);
    pub const OSD_ITYPE: Field = Field::new(Region::Set, 94, 8, 8, false, true, &[]);
    pub const OSD_LU_INV_MSK: Field = Field::new(Region::Set, 94, 16, 8, false, true, &[]);
    pub const OSD_CH_INV_MSK: Field = Field::new(Region::Set, 94, 24, 8, false, true, &[]);
}

/// Register `reg_095_100` at 0x017c: 6 words, Unknown.
pub mod reg_095_100 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg_095_100", 95, 6, Access::Unknown, 1);
    pub const REG_095_100: [Field; 6] = [
        Field::new(Region::Set, 95, 0, 32, false, false, &[]),
        Field::new(Region::Set, 96, 0, 32, false, false, &[]),
        Field::new(Region::Set, 97, 0, 32, false, false, &[]),
        Field::new(Region::Set, 98, 0, 32, false, false, &[]),
        Field::new(Region::Set, 99, 0, 32, false, false, &[]),
        Field::new(Region::Set, 100, 0, 32, false, false, &[]),
    ];
}

/// Register `reg101` at 0x0194: 1 word, ReadWrite.
pub mod reg101 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg101", 101, 1, Access::ReadWrite, 1);
    pub const VTHD_Y: Field = Field::new(Region::Set, 101, 0, 12, false, true, &[]);
    pub const VTHD_C: Field = Field::new(Region::Set, 101, 16, 12, false, true, &[]);
}

/// Register `reg102` at 0x0198: 1 word, ReadWrite.
pub mod reg102 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg102", 102, 1, Access::ReadWrite, 1);
    pub const RECT_SIZE: Field = Field::new(Region::Set, 102, 0, 1, false, true, &[]);
    pub const INTER_4X4: Field = Field::new(Region::Set, 102, 1, 1, false, true, &[]);
    pub const ARB_SEL: Field = Field::new(Region::Set, 102, 2, 1, false, true, &[]);
    pub const VLC_LMT: Field = Field::new(Region::Set, 102, 3, 1, false, true, &[]);
    pub const CHRM_SPCL: Field = Field::new(Region::Set, 102, 4, 1, false, true, &[]);
    pub const RDO_MASK: Field = Field::new(Region::Set, 102, 5, 8, false, true, &[]);
    pub const CCWA_E: Field = Field::new(Region::Set, 102, 13, 1, false, true, &[]);
    pub const SCL_LST_SEL: Field = Field::new(Region::Set, 102, 14, 1, false, true, &[]);
    pub const ATR_E: Field = Field::new(Region::Set, 102, 15, 1, false, true, &[]);
    pub const ATF_EDG: Field = Field::new(Region::Set, 102, 16, 2, false, true, &[]);
    pub const ATF_LVL_E: Field = Field::new(Region::Set, 102, 18, 1, false, true, &[]);
    pub const ATF_INTRA_E: Field = Field::new(Region::Set, 102, 19, 1, false, true, &[]);
    pub const SCL_LST_SEL_: Field = Field::new(Region::Set, 102, 20, 2, false, true, &[]);
    pub const SATD_BYPS_FLG: Field = Field::new(Region::Set, 102, 31, 1, false, true, &[]);
}

/// Register `reg103` at 0x019c: 1 word, ReadWrite.
pub mod reg103 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg103", 103, 1, Access::ReadWrite, 1);
    pub const NAL_REF_IDC: Field = Field::new(Region::Set, 103, 0, 2, false, true, &[]);
    pub const NAL_UNIT_TYPE: Field = Field::new(Region::Set, 103, 2, 5, false, true, &[]);
}

/// Register `reg104` at 0x01a0: 1 word, ReadWrite.
pub mod reg104 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg104", 104, 1, Access::ReadWrite, 1);
    pub const MAX_FNUM: Field = Field::new(Region::Set, 104, 0, 4, false, true, &[]);
    pub const DRCT_8X8: Field = Field::new(Region::Set, 104, 4, 1, false, true, &[]);
    pub const MPOC_LM4: Field = Field::new(Region::Set, 104, 5, 4, false, true, &[]);
}

/// Register `reg105` at 0x01a4: 1 word, ReadWrite.
pub mod reg105 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg105", 105, 1, Access::ReadWrite, 1);
    pub const ETPY_MODE: Field = Field::new(Region::Set, 105, 0, 1, false, true, &[]);
    pub const TRNS_8X8: Field = Field::new(Region::Set, 105, 1, 1, false, true, &[]);
    pub const CSIP_FLAG: Field = Field::new(Region::Set, 105, 2, 1, false, true, &[]);
    pub const NUM_REF0_IDX: Field = Field::new(Region::Set, 105, 3, 2, false, true, &[]);
    pub const NUM_REF1_IDX: Field = Field::new(Region::Set, 105, 5, 2, false, true, &[]);
    pub const PIC_INIT_QP: Field = Field::new(Region::Set, 105, 7, 6, false, true, &[]);
    pub const CB_OFST: Field = Field::new(Region::Set, 105, 13, 5, false, true, &[]);
    pub const CR_OFST: Field = Field::new(Region::Set, 105, 18, 5, false, true, &[]);
    pub const WGHT_PRED: Field = Field::new(Region::Set, 105, 23, 1, false, true, &[]);
    pub const DBF_CP_FLG: Field = Field::new(Region::Set, 105, 24, 1, false, true, &[]);
}

/// Register `reg106` at 0x01a8: 1 word, ReadWrite.
pub mod reg106 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg106", 106, 1, Access::ReadWrite, 1);
    pub const SLI_TYPE: Field = Field::new(Region::Set, 106, 0, 2, false, true, &[]);
    pub const PPS_ID: Field = Field::new(Region::Set, 106, 2, 8, false, true, &[]);
    pub const DRCT_SMVP: Field = Field::new(Region::Set, 106, 10, 1, false, true, &[]);
    pub const NUM_REF_OVRD: Field = Field::new(Region::Set, 106, 11, 1, false, true, &[]);
    pub const CBC_INIT_IDC: Field = Field::new(Region::Set, 106, 12, 2, false, true, &[]);
    pub const FRM_NUM: Field = Field::new(Region::Set, 106, 16, 16, false, true, &[]);
}

/// Register `reg107` at 0x01ac: 1 word, ReadWrite.
pub mod reg107 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg107", 107, 1, Access::ReadWrite, 1);
    pub const IDR_PIC_ID: Field = Field::new(Region::Set, 107, 0, 16, false, true, &[]);
    pub const POC_LSB: Field = Field::new(Region::Set, 107, 16, 16, false, true, &[]);
}

/// Register `reg108` at 0x01b0: 1 word, ReadWrite.
pub mod reg108 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg108", 108, 1, Access::ReadWrite, 1);
    pub const RODR_PIC_IDX: Field = Field::new(Region::Set, 108, 0, 2, false, true, &[]);
    pub const REF_LIST0_RODR: Field = Field::new(Region::Set, 108, 2, 1, false, true, &[]);
    pub const SLI_BETA_OFST: Field = Field::new(Region::Set, 108, 3, 4, false, true, &[]);
    pub const SLI_ALPH_OFST: Field = Field::new(Region::Set, 108, 7, 4, false, true, &[]);
    pub const DIS_DBLK_IDC: Field = Field::new(Region::Set, 108, 11, 2, false, true, &[]);
    pub const RODR_PIC_NUM: Field = Field::new(Region::Set, 108, 16, 16, false, true, &[]);
}

/// Register `reg109` at 0x01b4: 1 word, ReadWrite.
pub mod reg109 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg109", 109, 1, Access::ReadWrite, 1);
    pub const NOPP_FLG: Field = Field::new(Region::Set, 109, 0, 1, false, true, &[]);
    pub const LTRF_FLG: Field = Field::new(Region::Set, 109, 1, 1, false, true, &[]);
    pub const ARPM_FLG: Field = Field::new(Region::Set, 109, 2, 1, false, true, &[]);
    pub const MMCO4_PRE: Field = Field::new(Region::Set, 109, 3, 1, false, true, &[]);
    pub const MMCO_TYPE0: Field = Field::new(Region::Set, 109, 4, 3, false, true, &[]);
    pub const MMCO_PARM0: Field = Field::new(Region::Set, 109, 7, 16, false, true, &[]);
    pub const MMCO_TYPE1: Field = Field::new(Region::Set, 109, 23, 3, false, true, &[]);
    pub const MMCO_TYPE2: Field = Field::new(Region::Set, 109, 26, 3, false, true, &[]);
}

/// Register `reg110` at 0x01b8: 1 word, ReadWrite.
pub mod reg110 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg110", 110, 1, Access::ReadWrite, 1);
    pub const MMCO_PARM1: Field = Field::new(Region::Set, 110, 0, 16, false, true, &[]);
    pub const MMCO_PARM2: Field = Field::new(Region::Set, 110, 16, 16, false, true, &[]);
}

/// Register `reg_111` at 0x01bc: 1 word, Unknown.
pub mod reg_111 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg_111", 111, 1, Access::Unknown, 1);
    pub const REG_111: Field = Field::new(Region::Set, 111, 0, 32, false, false, &[]);
}

/// Register `reg112` at 0x01c0: 1 word, ReadWrite.
pub mod reg112 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg112", 112, 1, Access::ReadWrite, 1);
    pub const OSD_E: Field = Field::new(Region::Set, 112, 0, 8, false, true, &[]);
    pub const OSD_INV_E: Field = Field::new(Region::Set, 112, 8, 8, false, true, &[]);
    pub const OSD_PLT_CKS: Field = Field::new(Region::Set, 112, 16, 1, false, true, &[]);
    pub const OSD_PLT_TYP: Field = Field::new(Region::Set, 112, 17, 1, false, true, &[]);
}

/// Register `reg113` at 0x01c4: 1 word, ReadWrite.
pub mod reg113 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg113", 113, 1, Access::ReadWrite, 1);
    pub const OSD_ITHD_R0: Field = Field::new(Region::Set, 113, 0, 4, false, true, &[]);
    pub const OSD_ITHD_R1: Field = Field::new(Region::Set, 113, 4, 4, false, true, &[]);
    pub const OSD_ITHD_R2: Field = Field::new(Region::Set, 113, 8, 4, false, true, &[]);
    pub const OSD_ITHD_R3: Field = Field::new(Region::Set, 113, 12, 4, false, true, &[]);
    pub const OSD_ITHD_R4: Field = Field::new(Region::Set, 113, 16, 4, false, true, &[]);
    pub const OSD_ITHD_R5: Field = Field::new(Region::Set, 113, 20, 4, false, true, &[]);
    pub const OSD_ITHD_R6: Field = Field::new(Region::Set, 113, 24, 4, false, true, &[]);
    pub const OSD_ITHD_R7: Field = Field::new(Region::Set, 113, 28, 4, false, true, &[]);
}

/// Register `reg114` at 0x01c8: 1 word, ReadWrite.
pub mod reg114 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg114", 114, 1, Access::ReadWrite, 1);
    pub const LONG_TERM_FRAME_IDX0: Field = Field::new(Region::Set, 114, 0, 4, false, true, &[]);
    pub const LONG_TERM_FRAME_IDX1: Field = Field::new(Region::Set, 114, 4, 4, false, true, &[]);
    pub const LONG_TERM_FRAME_IDX2: Field = Field::new(Region::Set, 114, 8, 4, false, true, &[]);
}

/// Register `reg115` at 0x01cc: 1 word, ReadWrite.
pub mod reg115 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg115", 115, 1, Access::ReadWrite, 1);
    pub const REG115: Field = Field::new(Region::Set, 115, 0, 32, false, true, &[]);
}

/// Register `reg116_123` at 0x01d0: 8 words, ReadWrite.
pub mod reg116_123 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg116_123", 116, 8, Access::ReadWrite, 1);
    pub const OSD_POS_OSD_LT_X: [Field; 8] = [
        Field::new(Region::Set, 116, 0, 8, false, true, &[]),
        Field::new(Region::Set, 117, 0, 8, false, true, &[]),
        Field::new(Region::Set, 118, 0, 8, false, true, &[]),
        Field::new(Region::Set, 119, 0, 8, false, true, &[]),
        Field::new(Region::Set, 120, 0, 8, false, true, &[]),
        Field::new(Region::Set, 121, 0, 8, false, true, &[]),
        Field::new(Region::Set, 122, 0, 8, false, true, &[]),
        Field::new(Region::Set, 123, 0, 8, false, true, &[]),
    ];
    pub const OSD_POS_OSD_LT_Y: [Field; 8] = [
        Field::new(Region::Set, 116, 8, 8, false, true, &[]),
        Field::new(Region::Set, 117, 8, 8, false, true, &[]),
        Field::new(Region::Set, 118, 8, 8, false, true, &[]),
        Field::new(Region::Set, 119, 8, 8, false, true, &[]),
        Field::new(Region::Set, 120, 8, 8, false, true, &[]),
        Field::new(Region::Set, 121, 8, 8, false, true, &[]),
        Field::new(Region::Set, 122, 8, 8, false, true, &[]),
        Field::new(Region::Set, 123, 8, 8, false, true, &[]),
    ];
    pub const OSD_POS_OSD_RB_X: [Field; 8] = [
        Field::new(Region::Set, 116, 16, 8, false, true, &[]),
        Field::new(Region::Set, 117, 16, 8, false, true, &[]),
        Field::new(Region::Set, 118, 16, 8, false, true, &[]),
        Field::new(Region::Set, 119, 16, 8, false, true, &[]),
        Field::new(Region::Set, 120, 16, 8, false, true, &[]),
        Field::new(Region::Set, 121, 16, 8, false, true, &[]),
        Field::new(Region::Set, 122, 16, 8, false, true, &[]),
        Field::new(Region::Set, 123, 16, 8, false, true, &[]),
    ];
    pub const OSD_POS_OSD_RB_Y: [Field; 8] = [
        Field::new(Region::Set, 116, 24, 8, false, true, &[]),
        Field::new(Region::Set, 117, 24, 8, false, true, &[]),
        Field::new(Region::Set, 118, 24, 8, false, true, &[]),
        Field::new(Region::Set, 119, 24, 8, false, true, &[]),
        Field::new(Region::Set, 120, 24, 8, false, true, &[]),
        Field::new(Region::Set, 121, 24, 8, false, true, &[]),
        Field::new(Region::Set, 122, 24, 8, false, true, &[]),
        Field::new(Region::Set, 123, 24, 8, false, true, &[]),
    ];
}

/// Register `reg124_131` at 0x01f0: 8 words, ReadWrite.
pub mod reg124_131 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg124_131", 124, 8, Access::ReadWrite, 16);
    pub const OSD_ADDR: [AddressReg; 8] = [
        AddressReg::new(Region::Set, 124, 16),
        AddressReg::new(Region::Set, 125, 16),
        AddressReg::new(Region::Set, 126, 16),
        AddressReg::new(Region::Set, 127, 16),
        AddressReg::new(Region::Set, 128, 16),
        AddressReg::new(Region::Set, 129, 16),
        AddressReg::new(Region::Set, 130, 16),
        AddressReg::new(Region::Set, 131, 16),
    ];
}

/// Register `reg132` at 0x0210: 1 word, ReadOnly.
pub mod reg132 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg132", 132, 1, Access::ReadOnly, 1);
    pub const BS_LGTH: Field = Field::new(Region::Set, 132, 0, 27, false, false, &[]);
}

/// Register `reg133` at 0x0214: 1 word, ReadOnly.
pub mod reg133 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg133", 133, 1, Access::ReadOnly, 1);
    pub const SSE_L32: Field = Field::new(Region::Set, 133, 0, 32, false, false, &[]);
}

/// Register `reg134` at 0x0218: 1 word, ReadOnly.
pub mod reg134 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg134", 134, 1, Access::ReadOnly, 16777216);
    pub const QP_SUM: Field = Field::new(Region::Set, 134, 0, 22, false, false, &[]);
    pub const SSE_H8: Field = Field::new(Region::Set, 134, 24, 8, false, false, &[]);
}

/// Register `reg135` at 0x021c: 1 word, ReadOnly.
pub mod reg135 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg135", 135, 1, Access::ReadOnly, 1);
    pub const SAO_CNUM: Field = Field::new(Region::Set, 135, 0, 12, false, false, &[]);
    pub const SAO_YNUM: Field = Field::new(Region::Set, 135, 12, 12, false, false, &[]);
}

/// Register `reg_136_137` at 0x0220: 2 words, Unknown.
pub mod reg_136_137 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg_136_137", 136, 2, Access::Unknown, 1);
    pub const REG_136_137: [Field; 2] = [
        Field::new(Region::Set, 136, 0, 32, false, false, &[]),
        Field::new(Region::Set, 137, 0, 32, false, false, &[]),
    ];
}

/// Register `reg138` at 0x0228: 1 word, ReadOnly.
pub mod reg138 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg138", 138, 1, Access::ReadOnly, 1);
    pub const ST_ENC: Field = Field::new(Region::Set, 138, 0, 2, false, false, &[]);
    pub const ST_SCLR: Field = Field::new(Region::Set, 138, 2, 1, false, false, &[]);
}

/// Register `reg139` at 0x022c: 1 word, ReadOnly.
pub mod reg139 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg139", 139, 1, Access::ReadOnly, 1);
    pub const FNUM_ENC: Field = Field::new(Region::Set, 139, 0, 8, false, false, &[]);
    pub const FNUM_CFG: Field = Field::new(Region::Set, 139, 8, 8, false, false, &[]);
    pub const FNUM_INT: Field = Field::new(Region::Set, 139, 16, 8, false, false, &[]);
}

/// Register `reg140` at 0x0230: 1 word, ReadOnly.
pub mod reg140 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg140", 140, 1, Access::ReadOnly, 1);
    pub const NODE_ADDR: Field = Field::new(Region::Set, 140, 0, 32, false, false, &[]);
}

/// Register `reg141` at 0x0234: 1 word, ReadOnly.
pub mod reg141 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg141", 141, 1, Access::ReadOnly, 1);
    pub const BSBW_ADDR: Field = Field::new(Region::Set, 141, 0, 32, false, false, &[]);
}

/// Register `reg142` at 0x0238: 1 word, ReadOnly.
pub mod reg142 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg142", 142, 1, Access::ReadOnly, 1);
    pub const AXIB_IDL: Field = Field::new(Region::Set, 142, 0, 7, false, false, &[]);
    pub const AXIB_OVFL: Field = Field::new(Region::Set, 142, 7, 7, false, false, &[]);
    pub const AXIB_ERR: Field = Field::new(Region::Set, 142, 14, 7, false, false, &[]);
    pub const AXIR_ERR: Field = Field::new(Region::Set, 142, 21, 6, false, false, &[]);
}

/// Register `reg143` at 0x023c: 1 word, ReadOnly.
pub mod reg143 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg143", 143, 1, Access::ReadOnly, 1);
    pub const SLI_NUM: Field = Field::new(Region::Set, 143, 0, 6, false, false, &[]);
}

/// Register `reg144` at 0x0240: 1 word, ReadOnly.
pub mod reg144 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg144", 144, 1, Access::ReadOnly, 1);
    pub const SLI_LEN: Field = Field::new(Region::Set, 144, 0, 25, false, false, &[]);
}

/// Register `reg145` at 0x0244: 1 word, ReadOnly.
pub mod reg145 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg145", 145, 1, Access::ReadOnly, 1);
    pub const PNUM_P64: Field = Field::new(Region::Set, 145, 0, 12, false, false, &[]);
}

/// Register `reg146` at 0x0248: 1 word, ReadOnly.
pub mod reg146 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg146", 146, 1, Access::ReadOnly, 1);
    pub const PNUM_P32: Field = Field::new(Region::Set, 146, 0, 14, false, false, &[]);
}

/// Register `reg147` at 0x024c: 1 word, ReadOnly.
pub mod reg147 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg147", 147, 1, Access::ReadOnly, 1);
    pub const PNUM_P16: Field = Field::new(Region::Set, 147, 0, 16, false, false, &[]);
}

/// Register `reg148` at 0x0250: 1 word, ReadOnly.
pub mod reg148 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg148", 148, 1, Access::ReadOnly, 1);
    pub const PNUM_P8: Field = Field::new(Region::Set, 148, 0, 18, false, false, &[]);
}

/// Register `reg149` at 0x0254: 1 word, ReadOnly.
pub mod reg149 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg149", 149, 1, Access::ReadOnly, 1);
    pub const PNUM_I32: Field = Field::new(Region::Set, 149, 0, 14, false, false, &[]);
}

/// Register `reg150` at 0x0258: 1 word, ReadOnly.
pub mod reg150 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg150", 150, 1, Access::ReadOnly, 1);
    pub const PNUM_I16: Field = Field::new(Region::Set, 150, 0, 16, false, false, &[]);
}

/// Register `reg151` at 0x025c: 1 word, ReadOnly.
pub mod reg151 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg151", 151, 1, Access::ReadOnly, 1);
    pub const PNUM_I8: Field = Field::new(Region::Set, 151, 0, 18, false, false, &[]);
}

/// Register `reg152` at 0x0260: 1 word, ReadOnly.
pub mod reg152 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg152", 152, 1, Access::ReadOnly, 1);
    pub const PNUM_I4: Field = Field::new(Region::Set, 152, 0, 20, false, false, &[]);
}

/// Register `reg153_204` at 0x0264: 52 words, ReadOnly.
pub mod reg153_204 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg153_204", 153, 52, Access::ReadOnly, 1);
    pub const NUM_QP_B8NUM_QP: [Field; 52] = [
        Field::new(Region::Set, 153, 0, 18, false, false, &[]),
        Field::new(Region::Set, 154, 0, 18, false, false, &[]),
        Field::new(Region::Set, 155, 0, 18, false, false, &[]),
        Field::new(Region::Set, 156, 0, 18, false, false, &[]),
        Field::new(Region::Set, 157, 0, 18, false, false, &[]),
        Field::new(Region::Set, 158, 0, 18, false, false, &[]),
        Field::new(Region::Set, 159, 0, 18, false, false, &[]),
        Field::new(Region::Set, 160, 0, 18, false, false, &[]),
        Field::new(Region::Set, 161, 0, 18, false, false, &[]),
        Field::new(Region::Set, 162, 0, 18, false, false, &[]),
        Field::new(Region::Set, 163, 0, 18, false, false, &[]),
        Field::new(Region::Set, 164, 0, 18, false, false, &[]),
        Field::new(Region::Set, 165, 0, 18, false, false, &[]),
        Field::new(Region::Set, 166, 0, 18, false, false, &[]),
        Field::new(Region::Set, 167, 0, 18, false, false, &[]),
        Field::new(Region::Set, 168, 0, 18, false, false, &[]),
        Field::new(Region::Set, 169, 0, 18, false, false, &[]),
        Field::new(Region::Set, 170, 0, 18, false, false, &[]),
        Field::new(Region::Set, 171, 0, 18, false, false, &[]),
        Field::new(Region::Set, 172, 0, 18, false, false, &[]),
        Field::new(Region::Set, 173, 0, 18, false, false, &[]),
        Field::new(Region::Set, 174, 0, 18, false, false, &[]),
        Field::new(Region::Set, 175, 0, 18, false, false, &[]),
        Field::new(Region::Set, 176, 0, 18, false, false, &[]),
        Field::new(Region::Set, 177, 0, 18, false, false, &[]),
        Field::new(Region::Set, 178, 0, 18, false, false, &[]),
        Field::new(Region::Set, 179, 0, 18, false, false, &[]),
        Field::new(Region::Set, 180, 0, 18, false, false, &[]),
        Field::new(Region::Set, 181, 0, 18, false, false, &[]),
        Field::new(Region::Set, 182, 0, 18, false, false, &[]),
        Field::new(Region::Set, 183, 0, 18, false, false, &[]),
        Field::new(Region::Set, 184, 0, 18, false, false, &[]),
        Field::new(Region::Set, 185, 0, 18, false, false, &[]),
        Field::new(Region::Set, 186, 0, 18, false, false, &[]),
        Field::new(Region::Set, 187, 0, 18, false, false, &[]),
        Field::new(Region::Set, 188, 0, 18, false, false, &[]),
        Field::new(Region::Set, 189, 0, 18, false, false, &[]),
        Field::new(Region::Set, 190, 0, 18, false, false, &[]),
        Field::new(Region::Set, 191, 0, 18, false, false, &[]),
        Field::new(Region::Set, 192, 0, 18, false, false, &[]),
        Field::new(Region::Set, 193, 0, 18, false, false, &[]),
        Field::new(Region::Set, 194, 0, 18, false, false, &[]),
        Field::new(Region::Set, 195, 0, 18, false, false, &[]),
        Field::new(Region::Set, 196, 0, 18, false, false, &[]),
        Field::new(Region::Set, 197, 0, 18, false, false, &[]),
        Field::new(Region::Set, 198, 0, 18, false, false, &[]),
        Field::new(Region::Set, 199, 0, 18, false, false, &[]),
        Field::new(Region::Set, 200, 0, 18, false, false, &[]),
        Field::new(Region::Set, 201, 0, 18, false, false, &[]),
        Field::new(Region::Set, 202, 0, 18, false, false, &[]),
        Field::new(Region::Set, 203, 0, 18, false, false, &[]),
        Field::new(Region::Set, 204, 0, 18, false, false, &[]),
    ];
}

/// Register `reg205` at 0x0334: 1 word, ReadOnly.
pub mod reg205 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg205", 205, 1, Access::ReadOnly, 1);
    pub const MADP: Field = Field::new(Region::Set, 205, 0, 32, false, false, &[]);
}

/// Register `reg206` at 0x0338: 1 word, ReadOnly.
pub mod reg206 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg206", 206, 1, Access::ReadOnly, 1);
    pub const NUM_CTU: Field = Field::new(Region::Set, 206, 0, 16, false, false, &[]);
}

/// Register `reg207` at 0x033c: 1 word, ReadOnly.
pub mod reg207 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg207", 207, 1, Access::ReadOnly, 1);
    pub const MADI: Field = Field::new(Region::Set, 207, 0, 32, false, false, &[]);
}

/// Register `reg208` at 0x0340: 1 word, ReadOnly.
pub mod reg208 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "reg208", 208, 1, Access::ReadOnly, 1);
    pub const NUM_B16: Field = Field::new(Region::Set, 208, 0, 32, false, false, &[]);
}
