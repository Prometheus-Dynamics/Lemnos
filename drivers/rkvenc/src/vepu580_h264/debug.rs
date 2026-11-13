//! The VEPU580 H.264 DEBUG (kernel class 8): the debug registers, read only (reg_dbg).
//!
//! Words 5120 to 5333 of the register block; read only. Generated from the vendor header; see [`super`].

/// Register `reg5120` at 0x5000: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_wdg`.
pub mod reg5120 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_wdg",
        5120,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PP_TOUT: Field = Field::new(Region::Class(8), 5120, 0, 1, false, false, &[]);
    pub const CME_TOUT: Field = Field::new(Region::Class(8), 5120, 1, 1, false, false, &[]);
    pub const SWN_TOUT: Field = Field::new(Region::Class(8), 5120, 2, 1, false, false, &[]);
    pub const RME_TOUT: Field = Field::new(Region::Class(8), 5120, 3, 1, false, false, &[]);
    pub const FME_TOUT: Field = Field::new(Region::Class(8), 5120, 4, 1, false, false, &[]);
    pub const RDO_TOUT: Field = Field::new(Region::Class(8), 5120, 5, 1, false, false, &[]);
    pub const LPF_TOUT: Field = Field::new(Region::Class(8), 5120, 6, 1, false, false, &[]);
    pub const ETPY_TOUT: Field = Field::new(Region::Class(8), 5120, 7, 1, false, false, &[]);
    pub const FRM_TOUT: Field = Field::new(Region::Class(8), 5120, 8, 1, false, false, &[]);
}

/// Register `reg5121` at 0x5004: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl`.
pub mod reg5121 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl",
        5121,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PP_WRK: Field = Field::new(Region::Class(8), 5121, 0, 1, false, false, &[]);
    pub const CME_WRK: Field = Field::new(Region::Class(8), 5121, 1, 1, false, false, &[]);
    pub const SWN_WRK: Field = Field::new(Region::Class(8), 5121, 2, 1, false, false, &[]);
    pub const RME_WRK: Field = Field::new(Region::Class(8), 5121, 3, 1, false, false, &[]);
    pub const FME_WRK: Field = Field::new(Region::Class(8), 5121, 4, 1, false, false, &[]);
    pub const RDO_WRK: Field = Field::new(Region::Class(8), 5121, 5, 1, false, false, &[]);
    pub const LPF_WRK: Field = Field::new(Region::Class(8), 5121, 6, 1, false, false, &[]);
    pub const ETPY_WRK: Field = Field::new(Region::Class(8), 5121, 7, 1, false, false, &[]);
    pub const FRM_WRK: Field = Field::new(Region::Class(8), 5121, 8, 1, false, false, &[]);
}

/// Register `reg5122` at 0x5008: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_pp`.
pub mod reg5122 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_pp",
        5122,
        1,
        Access::ReadOnly,
        1,
    );
    pub const PP_POS_X: Field = Field::new(Region::Class(8), 5122, 0, 16, false, false, &[]);
    pub const PP_POS_Y: Field = Field::new(Region::Class(8), 5122, 16, 16, false, false, &[]);
}

/// Register `reg5123` at 0x500c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_cme`.
pub mod reg5123 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_cme",
        5123,
        1,
        Access::ReadOnly,
        1,
    );
    pub const CME_POS_X: Field = Field::new(Region::Class(8), 5123, 0, 16, false, false, &[]);
    pub const CME_POS_Y: Field = Field::new(Region::Class(8), 5123, 16, 16, false, false, &[]);
}

/// Register `reg5124` at 0x5010: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_swin`.
pub mod reg5124 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_swin",
        5124,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SWIN_POS_X: Field = Field::new(Region::Class(8), 5124, 0, 16, false, false, &[]);
    pub const SWIN_POS_Y: Field = Field::new(Region::Class(8), 5124, 16, 16, false, false, &[]);
}

/// Register `reg5125` at 0x5014: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_rme`.
pub mod reg5125 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_rme",
        5125,
        1,
        Access::ReadOnly,
        1,
    );
    pub const RME_POS_X: Field = Field::new(Region::Class(8), 5125, 0, 16, false, false, &[]);
    pub const RME_POS_Y: Field = Field::new(Region::Class(8), 5125, 16, 16, false, false, &[]);
}

/// Register `reg5126` at 0x5018: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_fme`.
pub mod reg5126 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_fme",
        5126,
        1,
        Access::ReadOnly,
        1,
    );
    pub const FME_POS_X: Field = Field::new(Region::Class(8), 5126, 0, 16, false, false, &[]);
    pub const FME_POS_Y: Field = Field::new(Region::Class(8), 5126, 16, 16, false, false, &[]);
}

/// Register `reg5127` at 0x501c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_rdo`.
pub mod reg5127 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_rdo",
        5127,
        1,
        Access::ReadOnly,
        1,
    );
    pub const RDO_POS_X: Field = Field::new(Region::Class(8), 5127, 0, 16, false, false, &[]);
    pub const RDO_POS_Y: Field = Field::new(Region::Class(8), 5127, 16, 16, false, false, &[]);
}

/// Register `reg5128` at 0x5020: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_lpf`.
pub mod reg5128 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_lpf",
        5128,
        1,
        Access::ReadOnly,
        1,
    );
    pub const LPF_POS_X: Field = Field::new(Region::Class(8), 5128, 0, 16, false, false, &[]);
    pub const LPF_POS_Y: Field = Field::new(Region::Class(8), 5128, 16, 16, false, false, &[]);
}

/// Register `reg5129` at 0x5024: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_ppl_pos_etpy`.
pub mod reg5129 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_ppl_pos_etpy",
        5129,
        1,
        Access::ReadOnly,
        1,
    );
    pub const ETPY_POS_X: Field = Field::new(Region::Class(8), 5129, 0, 16, false, false, &[]);
    pub const ETPY_POS_Y: Field = Field::new(Region::Class(8), 5129, 16, 16, false, false, &[]);
}

/// Register `reg5130` at 0x5028: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_sli_num`.
pub mod reg5130 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_sli_num",
        5130,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SLI_NUM: Field = Field::new(Region::Class(8), 5130, 0, 15, false, false, &[]);
}

/// Register `reg5131` at 0x502c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_lkt_err`.
pub mod reg5131 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_lkt_err",
        5131,
        1,
        Access::ReadOnly,
        1,
    );
    pub const LKT_ERR: Field = Field::new(Region::Class(8), 5131, 0, 3, false, false, &[]);
}

/// Register `reg5184` at 0x5100: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_pp_st`.
pub mod reg5184 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_pp_st",
        5184,
        1,
        Access::ReadOnly,
        1,
    );
    pub const EMPTY_OAFIFO: Field = Field::new(Region::Class(8), 5184, 0, 1, false, false, &[]);
    pub const FULL_CMD_OAFIFO: Field = Field::new(Region::Class(8), 5184, 1, 1, false, false, &[]);
    pub const FULL_DATA_OAFIFO: Field = Field::new(Region::Class(8), 5184, 2, 1, false, false, &[]);
    pub const EMPTY_IAFIFO: Field = Field::new(Region::Class(8), 5184, 3, 1, false, false, &[]);
    pub const FULL_CMD_IAFIFO: Field = Field::new(Region::Class(8), 5184, 4, 1, false, false, &[]);
    pub const FULL_INFO_IAFIFO: Field = Field::new(Region::Class(8), 5184, 5, 1, false, false, &[]);
    pub const FBD_BRQ_ST: Field = Field::new(Region::Class(8), 5184, 6, 4, false, false, &[]);
    pub const FBD_HDR_VLD: Field = Field::new(Region::Class(8), 5184, 10, 1, false, false, &[]);
    pub const FBD_BMNG_END: Field = Field::new(Region::Class(8), 5184, 11, 1, false, false, &[]);
    pub const NFBD_REQ_ST: Field = Field::new(Region::Class(8), 5184, 12, 4, false, false, &[]);
    pub const ACC_AXI_CMD: Field = Field::new(Region::Class(8), 5184, 16, 8, false, false, &[]);
}

/// Register `reg5185` at 0x5104: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_cime_st`.
pub mod reg5185 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_cime_st",
        5185,
        1,
        Access::ReadOnly,
        1,
    );
    pub const CUR_STATE_CIME: Field = Field::new(Region::Class(8), 5185, 0, 2, false, false, &[]);
    pub const CUR_STATE_DS: Field = Field::new(Region::Class(8), 5185, 2, 3, false, false, &[]);
    pub const CUR_STATE_REF: Field = Field::new(Region::Class(8), 5185, 5, 2, false, false, &[]);
    pub const CUR_STATE_CST: Field = Field::new(Region::Class(8), 5185, 7, 2, false, false, &[]);
}

/// Register `reg5186` at 0x5108: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.swin_dbg_inf`.
pub mod reg5186 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.swin_dbg_inf",
        5186,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5187` at 0x510c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_fbd_hhit0`.
pub mod reg5187 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_fbd_hhit0",
        5187,
        1,
        Access::ReadOnly,
        1,
    );
    pub const BBRQ_CMPS_LEFT_LEN2: Field =
        Field::new(Region::Class(8), 5187, 0, 1, false, false, &[]);
    pub const BBRQ_CMPS_LEFT_LEN1: Field =
        Field::new(Region::Class(8), 5187, 1, 1, false, false, &[]);
    pub const CMPS_LEFT_LEN0: Field = Field::new(Region::Class(8), 5187, 2, 1, false, false, &[]);
    pub const BBRQ_RDY2: Field = Field::new(Region::Class(8), 5187, 3, 1, false, false, &[]);
    pub const DCPS_VLD2: Field = Field::new(Region::Class(8), 5187, 4, 1, false, false, &[]);
    pub const BBRQ_RDY1: Field = Field::new(Region::Class(8), 5187, 5, 1, false, false, &[]);
    pub const DCPS_VLD1: Field = Field::new(Region::Class(8), 5187, 6, 1, false, false, &[]);
    pub const BBRQ_RDY0: Field = Field::new(Region::Class(8), 5187, 7, 1, false, false, &[]);
    pub const DCPS_VLD0: Field = Field::new(Region::Class(8), 5187, 8, 1, false, false, &[]);
    pub const HB_RDY2: Field = Field::new(Region::Class(8), 5187, 9, 1, false, false, &[]);
    pub const BBRQ_VLD2: Field = Field::new(Region::Class(8), 5187, 10, 1, false, false, &[]);
    pub const HB_RDY1: Field = Field::new(Region::Class(8), 5187, 11, 1, false, false, &[]);
    pub const BBRQ_VLD1: Field = Field::new(Region::Class(8), 5187, 12, 1, false, false, &[]);
    pub const HB_RDY0: Field = Field::new(Region::Class(8), 5187, 13, 1, false, false, &[]);
    pub const BBRQ_VLD0: Field = Field::new(Region::Class(8), 5187, 14, 1, false, false, &[]);
    pub const IDLE_MSB2: Field = Field::new(Region::Class(8), 5187, 15, 1, false, false, &[]);
    pub const IDLE_MSB1: Field = Field::new(Region::Class(8), 5187, 16, 1, false, false, &[]);
    pub const IDLE_MSB0: Field = Field::new(Region::Class(8), 5187, 17, 1, false, false, &[]);
    pub const CUR_STATE_DCPS: Field = Field::new(Region::Class(8), 5187, 18, 1, false, false, &[]);
    pub const CUR_STATE_BBRQ: Field = Field::new(Region::Class(8), 5187, 19, 1, false, false, &[]);
    pub const CUR_STATE_HB: Field = Field::new(Region::Class(8), 5187, 20, 1, false, false, &[]);
    pub const CKE_BBRQ_DCPS: Field = Field::new(Region::Class(8), 5187, 21, 1, false, false, &[]);
    pub const CKE_DCPS: Field = Field::new(Region::Class(8), 5187, 22, 1, false, false, &[]);
    pub const CKE_BBRQ: Field = Field::new(Region::Class(8), 5187, 23, 1, false, false, &[]);
    pub const RDY_LWCD_RSP: Field = Field::new(Region::Class(8), 5187, 24, 1, false, false, &[]);
    pub const VLD_LWCD_RSP: Field = Field::new(Region::Class(8), 5187, 25, 1, false, false, &[]);
    pub const RDY_LWCD_REQ: Field = Field::new(Region::Class(8), 5187, 26, 1, false, false, &[]);
    pub const VLD_LWCD_REQ: Field = Field::new(Region::Class(8), 5187, 27, 1, false, false, &[]);
    pub const RDY_LWRSP: Field = Field::new(Region::Class(8), 5187, 28, 1, false, false, &[]);
    pub const VLD_LWRSP: Field = Field::new(Region::Class(8), 5187, 29, 1, false, false, &[]);
    pub const RDY_LWREQ: Field = Field::new(Region::Class(8), 5187, 30, 1, false, false, &[]);
    pub const VLD_LWREQ: Field = Field::new(Region::Class(8), 5187, 31, 1, false, false, &[]);
}

/// Register `reg5189` at 0x5114: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_cach_clr`.
pub mod reg5189 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_cach_clr",
        5189,
        1,
        Access::ReadOnly,
        1,
    );
    pub const MSCNT_CLR: Field = Field::new(Region::Class(8), 5189, 0, 1, false, false, &[]);
}

/// Register `reg5190` at 0x5118: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.l1_mis`.
pub mod reg5190 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.l1_mis",
        5190,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5191` at 0x511c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.l2_mis`.
pub mod reg5191 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.l2_mis",
        5191,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5192` at 0x5120: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.rdo_st`.
pub mod reg5192 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.rdo_st",
        5192,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5193` at 0x5124: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.rdo_if`.
pub mod reg5193 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.rdo_if",
        5193,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5194` at 0x5128: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_etpy`.
pub mod reg5194 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_etpy",
        5194,
        1,
        Access::ReadOnly,
        1,
    );
    pub const SH_ST_CS: Field = Field::new(Region::Class(8), 5194, 0, 4, false, false, &[]);
    pub const RSD_ST_CS: Field = Field::new(Region::Class(8), 5194, 4, 4, false, false, &[]);
    pub const SD_ST_CS: Field = Field::new(Region::Class(8), 5194, 8, 5, false, false, &[]);
    pub const ETPY_RDY: Field = Field::new(Region::Class(8), 5194, 13, 1, false, false, &[]);
}

/// Register `reg5195` at 0x512c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_dma_pp`.
pub mod reg5195 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_dma_pp",
        5195,
        1,
        Access::ReadOnly,
        1,
    );
    pub const CRDY_PPR: Field = Field::new(Region::Class(8), 5195, 0, 1, false, false, &[]);
    pub const CVLD_PPR: Field = Field::new(Region::Class(8), 5195, 1, 1, false, false, &[]);
    pub const DRDY_PPW: Field = Field::new(Region::Class(8), 5195, 2, 1, false, false, &[]);
    pub const DVLD_PPW: Field = Field::new(Region::Class(8), 5195, 3, 1, false, false, &[]);
    pub const CRDY_PPW: Field = Field::new(Region::Class(8), 5195, 4, 1, false, false, &[]);
    pub const CVLD_PPW: Field = Field::new(Region::Class(8), 5195, 5, 1, false, false, &[]);
}

/// Register `reg5196` at 0x5130: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_dma_w`.
pub mod reg5196 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_dma_w",
        5196,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXI_WRDY: Field = Field::new(Region::Class(8), 5196, 0, 8, false, false, &[]);
    pub const AXI_WVLD: Field = Field::new(Region::Class(8), 5196, 8, 8, false, false, &[]);
    pub const AXI_AWRDY: Field = Field::new(Region::Class(8), 5196, 16, 8, false, false, &[]);
    pub const AXI_AWVLD: Field = Field::new(Region::Class(8), 5196, 24, 8, false, false, &[]);
}

/// Register `reg5197` at 0x5134: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_dma_r`.
pub mod reg5197 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_dma_r",
        5197,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXI_OTSD_READ: Field = Field::new(Region::Class(8), 5197, 0, 16, false, false, &[]);
    pub const AXI_ARRDY: Field = Field::new(Region::Class(8), 5197, 16, 7, false, false, &[]);
    pub const AXI_ARVLD: Field = Field::new(Region::Class(8), 5197, 24, 7, false, false, &[]);
}

/// Register `reg5198` at 0x5138: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_dma_rfpr`.
pub mod reg5198 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_dma_rfpr",
        5198,
        1,
        Access::ReadOnly,
        1,
    );
    pub const DFIFO0_LVL: Field = Field::new(Region::Class(8), 5198, 0, 4, false, false, &[]);
    pub const DFIFO1_LVL: Field = Field::new(Region::Class(8), 5198, 4, 4, false, false, &[]);
    pub const DFIFO2_LVL: Field = Field::new(Region::Class(8), 5198, 8, 4, false, false, &[]);
    pub const DFIFO3_LVL: Field = Field::new(Region::Class(8), 5198, 12, 4, false, false, &[]);
    pub const DFIFO4_LVL: Field = Field::new(Region::Class(8), 5198, 16, 4, false, false, &[]);
    pub const DFIFO5_LVL: Field = Field::new(Region::Class(8), 5198, 20, 4, false, false, &[]);
    pub const CMD_VLD: Field = Field::new(Region::Class(8), 5198, 30, 1, false, false, &[]);
}

/// Register `reg5199` at 0x513c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_dma_ch_st`.
pub mod reg5199 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_dma_ch_st",
        5199,
        1,
        Access::ReadOnly,
        1,
    );
    pub const MEIW_BUSY: Field = Field::new(Region::Class(8), 5199, 0, 1, false, false, &[]);
    pub const DSPW_BUSY: Field = Field::new(Region::Class(8), 5199, 1, 1, false, false, &[]);
    pub const BSW_RDY: Field = Field::new(Region::Class(8), 5199, 2, 1, false, false, &[]);
    pub const BSW_FLSH: Field = Field::new(Region::Class(8), 5199, 3, 1, false, false, &[]);
    pub const BSW_BUSY: Field = Field::new(Region::Class(8), 5199, 4, 1, false, false, &[]);
    pub const CRPW_BUSY: Field = Field::new(Region::Class(8), 5199, 5, 1, false, false, &[]);
    pub const LKTW_BUSY: Field = Field::new(Region::Class(8), 5199, 6, 1, false, false, &[]);
    pub const LPFW_BUSY: Field = Field::new(Region::Class(8), 5199, 7, 1, false, false, &[]);
    pub const ROIR_BUSY: Field = Field::new(Region::Class(8), 5199, 8, 1, false, false, &[]);
    pub const DSPR_CRDY: Field = Field::new(Region::Class(8), 5199, 9, 1, false, false, &[]);
    pub const DSPR_CVLD: Field = Field::new(Region::Class(8), 5199, 10, 1, false, false, &[]);
    pub const LKTR_BUSY: Field = Field::new(Region::Class(8), 5199, 11, 1, false, false, &[]);
    pub const LPFR_OTSD: Field = Field::new(Region::Class(8), 5199, 12, 4, false, false, &[]);
    pub const RFPR_OTSD: Field = Field::new(Region::Class(8), 5199, 16, 12, false, false, &[]);
    pub const DSPR_OTSD: Field = Field::new(Region::Class(8), 5199, 28, 4, false, false, &[]);
}

/// Register `reg5200` at 0x5140: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_tctrl_cime_st`.
pub mod reg5200 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_tctrl_cime_st",
        5200,
        1,
        Access::ReadOnly,
        1,
    );
    pub const CPIP_ST: Field = Field::new(Region::Class(8), 5200, 0, 2, false, false, &[]);
    pub const MVP_ST: Field = Field::new(Region::Class(8), 5200, 2, 3, false, false, &[]);
    pub const QPD6_ST: Field = Field::new(Region::Class(8), 5200, 5, 2, false, false, &[]);
    pub const CMD_ST: Field = Field::new(Region::Class(8), 5200, 7, 2, false, false, &[]);
}

/// Register `reg5201` at 0x5144: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_tctrl`.
pub mod reg5201 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_tctrl",
        5201,
        1,
        Access::ReadOnly,
        1,
    );
    pub const CME_BYPS: Field = Field::new(Region::Class(8), 5201, 0, 1, false, false, &[]);
    pub const SWIN_BYPS: Field = Field::new(Region::Class(8), 5201, 1, 1, false, false, &[]);
    pub const RME_BYPS: Field = Field::new(Region::Class(8), 5201, 2, 1, false, false, &[]);
    pub const INTRA_BYPS: Field = Field::new(Region::Class(8), 5201, 3, 1, false, false, &[]);
    pub const FME_BYPS: Field = Field::new(Region::Class(8), 5201, 4, 1, false, false, &[]);
    pub const RDO_BYPS: Field = Field::new(Region::Class(8), 5201, 5, 1, false, false, &[]);
    pub const LPF_BYPS: Field = Field::new(Region::Class(8), 5201, 6, 1, false, false, &[]);
    pub const ETPY_BYPS: Field = Field::new(Region::Class(8), 5201, 7, 1, false, false, &[]);
}

/// Register `reg5203` at 0x514c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_lpf_st`.
pub mod reg5203 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_lpf_st",
        5203,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5204` at 0x5150: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg_topc_lpfr`.
pub mod reg5204 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg_topc_lpfr",
        5204,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5205` at 0x5154: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg0_cache`.
pub mod reg5205 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg0_cache",
        5205,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5206` at 0x5158: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg1_cache`.
pub mod reg5206 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg1_cache",
        5206,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5207` at 0x515c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.dbg2_cache`.
pub mod reg5207 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.dbg2_cache",
        5207,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5248` at 0x5200: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.frame_cyc`.
pub mod reg5248 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.frame_cyc",
        5248,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5249` at 0x5204: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.pp_fcyc`.
pub mod reg5249 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.pp_fcyc",
        5249,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5250` at 0x5208: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.cme_fcyc`.
pub mod reg5250 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.cme_fcyc",
        5250,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5251` at 0x520c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.cme_dspr_fcyc`.
pub mod reg5251 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.cme_dspr_fcyc",
        5251,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5252` at 0x5210: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.ldr_fcyc`.
pub mod reg5252 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.ldr_fcyc",
        5252,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5253` at 0x5214: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.rme_fcyc`.
pub mod reg5253 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.rme_fcyc",
        5253,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5254` at 0x5218: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.fme_fcyc`.
pub mod reg5254 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.fme_fcyc",
        5254,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5255` at 0x521c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.rdo_fcyc`.
pub mod reg5255 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.rdo_fcyc",
        5255,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5256` at 0x5220: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.lpf_fcyc`.
pub mod reg5256 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.lpf_fcyc",
        5256,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5257` at 0x5224: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.etpy_fcyc`.
pub mod reg5257 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.etpy_fcyc",
        5257,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5312` at 0x5300: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_cmd`.
pub mod reg5312 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_cmd",
        5312,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_E: Field = Field::new(Region::Class(8), 5312, 0, 1, false, false, &[]);
    pub const AXIP_CLR: Field = Field::new(Region::Class(8), 5312, 1, 1, false, false, &[]);
    pub const AXIP_MOD: Field = Field::new(Region::Class(8), 5312, 2, 1, false, false, &[]);
}

/// Register `reg5313` at 0x5304: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_ltcy`.
pub mod reg5313 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_ltcy",
        5313,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_LTCY_ID: Field = Field::new(Region::Class(8), 5313, 0, 4, false, false, &[]);
    pub const AXIP_LTCY_THD: Field = Field::new(Region::Class(8), 5313, 4, 12, false, false, &[]);
}

/// Register `reg5314` at 0x5308: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_cnt`.
pub mod reg5314 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_cnt",
        5314,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_CNT_TYP: Field = Field::new(Region::Class(8), 5314, 0, 1, false, false, &[]);
    pub const AXIP_CNT_DDR: Field = Field::new(Region::Class(8), 5314, 1, 2, false, false, &[]);
    pub const AXIP_CNT_RID: Field = Field::new(Region::Class(8), 5314, 3, 5, false, false, &[]);
    pub const AXIP_CNT_WID: Field = Field::new(Region::Class(8), 5314, 8, 5, false, false, &[]);
}

/// Register `reg5316` at 0x5310: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_cmd`.
pub mod reg5316 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_cmd",
        5316,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_E: Field = Field::new(Region::Class(8), 5316, 0, 1, false, false, &[]);
    pub const AXIP_CLR: Field = Field::new(Region::Class(8), 5316, 1, 1, false, false, &[]);
    pub const AXIP_MOD: Field = Field::new(Region::Class(8), 5316, 2, 1, false, false, &[]);
}

/// Register `reg5317` at 0x5314: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_ltcy`.
pub mod reg5317 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_ltcy",
        5317,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_LTCY_ID: Field = Field::new(Region::Class(8), 5317, 0, 4, false, false, &[]);
    pub const AXIP_LTCY_THD: Field = Field::new(Region::Class(8), 5317, 4, 12, false, false, &[]);
}

/// Register `reg5318` at 0x5318: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_cnt`.
pub mod reg5318 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_cnt",
        5318,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_CNT_TYP: Field = Field::new(Region::Class(8), 5318, 0, 1, false, false, &[]);
    pub const AXIP_CNT_DDR: Field = Field::new(Region::Class(8), 5318, 1, 2, false, false, &[]);
    pub const AXIP_CNT_RID: Field = Field::new(Region::Class(8), 5318, 3, 5, false, false, &[]);
    pub const AXIP_CNT_WID: Field = Field::new(Region::Class(8), 5318, 8, 5, false, false, &[]);
}

/// Register `reg5320` at 0x5320: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_axip0_maxl`.
pub mod reg5320 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_axip0_maxl",
        5320,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_MAX_LTCY: Field = Field::new(Region::Class(8), 5320, 0, 16, false, false, &[]);
}

/// Register `reg5321` at 0x5324: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_num_ltcy`.
pub mod reg5321 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_num_ltcy",
        5321,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5322` at 0x5328: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_sum_ltcy`.
pub mod reg5322 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_sum_ltcy",
        5322,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5323` at 0x532c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_rbyt`.
pub mod reg5323 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_rbyt",
        5323,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5324` at 0x5330: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_wbyt`.
pub mod reg5324 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_wbyt",
        5324,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5325` at 0x5334: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip0_wrk_cyc`.
pub mod reg5325 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip0_wrk_cyc",
        5325,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5328` at 0x5340: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.st_axip1_maxl`.
pub mod reg5328 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.st_axip1_maxl",
        5328,
        1,
        Access::ReadOnly,
        1,
    );
    pub const AXIP_MAX_LTCY: Field = Field::new(Region::Class(8), 5328, 0, 16, false, false, &[]);
}

/// Register `reg5329` at 0x5344: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_num_ltcy`.
pub mod reg5329 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_num_ltcy",
        5329,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5330` at 0x5348: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_sum_ltcy`.
pub mod reg5330 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_sum_ltcy",
        5330,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5331` at 0x534c: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_rbyt`.
pub mod reg5331 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_rbyt",
        5331,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5332` at 0x5350: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_wbyt`.
pub mod reg5332 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_wbyt",
        5332,
        1,
        Access::ReadOnly,
        1,
    );
}

/// Register `reg5333` at 0x5354: 1 word, ReadOnly.
///
/// The C path is `reg_dbg.axip1_wrk_cyc`.
pub mod reg5333 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(8),
        "reg_dbg.axip1_wrk_cyc",
        5333,
        1,
        Access::ReadOnly,
        1,
    );
}
