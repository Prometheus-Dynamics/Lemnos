//! The VEPU541 H.265 write side: every register of the job, as the header lays it out.
//!
//! Words 0 to 131 (the window the job writes). Word 7 appears here as `int_stus`, the same
//! word the read-back window names `hw_status` (see [`super::ret`]).

#![allow(clippy::unreadable_literal)]

/// Register `int_stus` at 0x001c: 1 word, WriteOneToClear.
pub mod int_stus {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "int_stus", 7, 1, Access::WriteOneToClear, 1);
    pub const ENC_DONE_STA: Field = Field::new(Region::Set, 7, 0, 1, false, true, &[]);
    pub const LKT_DONE_STA: Field = Field::new(Region::Set, 7, 1, 1, false, true, &[]);
    pub const SCLR_DONE_STA: Field = Field::new(Region::Set, 7, 2, 1, false, true, &[]);
    pub const SLC_DONE_STA: Field = Field::new(Region::Set, 7, 3, 1, false, true, &[]);
    pub const BSF_OFLW_STA: Field = Field::new(Region::Set, 7, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_STA: Field = Field::new(Region::Set, 7, 5, 1, false, true, &[]);
    pub const WBUS_ERR_STA: Field = Field::new(Region::Set, 7, 6, 1, false, true, &[]);
    pub const RBUS_ERR_STA: Field = Field::new(Region::Set, 7, 7, 1, false, true, &[]);
    pub const WDG_STA: Field = Field::new(Region::Set, 7, 8, 1, false, true, &[]);
}

/// Register `version` at 0x0000: 1 word, ReadOnly.
pub mod version {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "version", 0, 1, Access::ReadOnly, 1);
    pub const RKVENC_VER: Field = Field::new(Region::Set, 0, 0, 32, false, false, &[]);
}

/// Register `enc_strt` at 0x0004: 1 word, ReadWrite.
pub mod enc_strt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "enc_strt", 1, 1, Access::ReadWrite, 1);
    pub const LKT_NUM: Field = Field::new(Region::Set, 1, 0, 8, false, true, &[]);
    pub const RKVENC_CMD: Field = Field::new(Region::Set, 1, 8, 2, false, true, &[]);
    pub const ENC_CKE: Field = Field::new(Region::Set, 1, 16, 1, false, true, &[]);
    pub const RESETN_HW_EN: Field = Field::new(Region::Set, 1, 17, 1, false, true, &[]);
    pub const ENC_DONE_TMVP_EN: Field = Field::new(Region::Set, 1, 18, 1, false, true, &[]);
    pub const RESERVE: Field = Field::new(Region::Set, 1, 19, 13, false, true, &[]);
}

/// Register `enc_clr` at 0x0008: 1 word, ReadWrite.
pub mod enc_clr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "enc_clr", 2, 1, Access::ReadWrite, 1);
    pub const SAFE_CLR: Field = Field::new(Region::Set, 2, 0, 1, false, true, &[]);
    pub const FORCE_CLR: Field = Field::new(Region::Set, 2, 1, 1, false, true, &[]);
}

/// Register `lkt_addr` at 0x000c: 1 word, ReadWrite.
pub mod lkt_addr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "lkt_addr", 3, 1, Access::ReadWrite, 1);
    pub const LKT_ADDR: AddressReg = AddressReg::new(Region::Set, 3, 1);
}

/// Register `int_en` at 0x0010: 1 word, ReadWrite.
pub mod int_en {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "int_en", 4, 1, Access::ReadWrite, 1);
    pub const ENC_DONE_EN: Field = Field::new(Region::Set, 4, 0, 1, false, true, &[]);
    pub const LKT_DONE_EN: Field = Field::new(Region::Set, 4, 1, 1, false, true, &[]);
    pub const SCLR_DONE_EN: Field = Field::new(Region::Set, 4, 2, 1, false, true, &[]);
    pub const SLC_DONE_EN: Field = Field::new(Region::Set, 4, 3, 1, false, true, &[]);
    pub const BSF_OVFLW_EN: Field = Field::new(Region::Set, 4, 4, 1, false, true, &[]);
    pub const BRSP_OSTD_EN: Field = Field::new(Region::Set, 4, 5, 1, false, true, &[]);
    pub const WBUS_ERR_EN: Field = Field::new(Region::Set, 4, 6, 1, false, true, &[]);
    pub const RBUS_ERR_EN: Field = Field::new(Region::Set, 4, 7, 1, false, true, &[]);
    pub const WDG_EN: Field = Field::new(Region::Set, 4, 8, 1, false, true, &[]);
}

/// Register `int_msk` at 0x0014: 1 word, ReadWrite.
pub mod int_msk {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "int_msk", 5, 1, Access::ReadWrite, 1);
    pub const ENC_DONE_MSK: Field = Field::new(Region::Set, 5, 0, 1, false, true, &[]);
    pub const LKT_DONE_MSK: Field = Field::new(Region::Set, 5, 1, 1, false, true, &[]);
    pub const SCLR_DONE_MSK: Field = Field::new(Region::Set, 5, 2, 1, false, true, &[]);
    pub const SLC_DONE_MSK: Field = Field::new(Region::Set, 5, 3, 1, false, true, &[]);
    pub const BSF_FOLW_MSK: Field = Field::new(Region::Set, 5, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_MSK: Field = Field::new(Region::Set, 5, 5, 1, false, true, &[]);
    pub const WBUS_ERR_MSK: Field = Field::new(Region::Set, 5, 6, 1, false, true, &[]);
    pub const RBUS_ERR_MSK: Field = Field::new(Region::Set, 5, 7, 1, false, true, &[]);
    pub const WDG_MSK: Field = Field::new(Region::Set, 5, 8, 1, false, true, &[]);
}

/// Register `int_clr` at 0x0018: 1 word, ReadWrite.
pub mod int_clr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "int_clr", 6, 1, Access::ReadWrite, 1);
    pub const ENC_DONE_CLR: Field = Field::new(Region::Set, 6, 0, 1, false, true, &[]);
    pub const LKT_DONE_CLR: Field = Field::new(Region::Set, 6, 1, 1, false, true, &[]);
    pub const SCLR_DONE_CLR: Field = Field::new(Region::Set, 6, 2, 1, false, true, &[]);
    pub const SLC_DONE_CLR: Field = Field::new(Region::Set, 6, 3, 1, false, true, &[]);
    pub const BSF_FOLW_CLR: Field = Field::new(Region::Set, 6, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_CLR: Field = Field::new(Region::Set, 6, 5, 1, false, true, &[]);
    pub const WBUS_ERR_CLR: Field = Field::new(Region::Set, 6, 6, 1, false, true, &[]);
    pub const RBUS_ERR_CLR: Field = Field::new(Region::Set, 6, 7, 1, false, true, &[]);
    pub const WDG_MSK: Field = Field::new(Region::Set, 6, 8, 1, false, true, &[]);
}

/// Register `enc_rsl` at 0x0030: 1 word, ReadWrite.
pub mod enc_rsl {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "enc_rsl", 12, 1, Access::ReadWrite, 1);
    pub const PIC_WD8_M1: Field = Field::new(Region::Set, 12, 0, 9, false, true, &[]);
    pub const PIC_WFILL: Field = Field::new(Region::Set, 12, 10, 6, false, true, &[]);
    pub const PIC_HD8_M1: Field = Field::new(Region::Set, 12, 16, 9, false, true, &[]);
    pub const PIC_HFILL: Field = Field::new(Region::Set, 12, 26, 6, false, true, &[]);
}

/// Register `enc_pic` at 0x0034: 1 word, ReadWrite.
pub mod enc_pic {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "enc_pic", 13, 1, Access::ReadWrite, 1);
    pub const ENC_STND: Field = Field::new(Region::Set, 13, 0, 1, false, true, &[]);
    pub const ROI_EN: Field = Field::new(Region::Set, 13, 1, 1, false, true, &[]);
    pub const CUR_FRM_REF: Field = Field::new(Region::Set, 13, 2, 1, false, true, &[]);
    pub const MEI_STOR: Field = Field::new(Region::Set, 13, 3, 1, false, true, &[]);
    pub const BS_SCP: Field = Field::new(Region::Set, 13, 4, 1, false, true, &[]);
    pub const RDO_WGT_SEL: Field = Field::new(Region::Set, 13, 5, 1, false, true, &[]);
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

/// Register `enc_wdg` at 0x0038: 1 word, ReadWrite.
pub mod enc_wdg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "enc_wdg", 14, 1, Access::ReadWrite, 1);
    pub const VS_LOAD_THD: Field = Field::new(Region::Set, 14, 0, 24, false, true, &[]);
    pub const RFP_LOAD_THD: Field = Field::new(Region::Set, 14, 24, 8, false, true, &[]);
}

/// Register `dtrns_map` at 0x003c: 1 word, ReadWrite.
pub mod dtrns_map {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "dtrns_map", 15, 1, Access::ReadWrite, 1);
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

/// Register `dtrns_cfg_541` at 0x0040: 1 word, ReadWrite.
pub mod dtrns_cfg_541 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "dtrns_cfg_541", 16, 1, Access::ReadWrite, 1);
    pub const AXI_BRSP_CKE: Field = Field::new(Region::Set, 16, 0, 7, false, true, &[]);
    pub const CIME_DSPW_ORSD: Field = Field::new(Region::Set, 16, 7, 1, false, true, &[]);
}

/// Register `src_fmt` at 0x0044: 1 word, ReadWrite.
pub mod src_fmt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_fmt", 17, 1, Access::ReadWrite, 1);
    pub const ALPHA_SWAP: Field = Field::new(Region::Set, 17, 0, 1, false, true, &[]);
    pub const RBUV_SWAP: Field = Field::new(Region::Set, 17, 1, 1, false, true, &[]);
    pub const SRC_CFMT: Field = Field::new(Region::Set, 17, 2, 4, false, true, &[]);
    pub const SRC_RANGE: Field = Field::new(Region::Set, 17, 6, 1, false, true, &[]);
    pub const OUT_FMT_CFG: Field = Field::new(Region::Set, 17, 7, 1, false, true, &[]);
}

/// Register `src_udfy` at 0x0048: 1 word, ReadWrite.
pub mod src_udfy {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_udfy", 18, 1, Access::ReadWrite, 1);
    pub const WGHT_B2Y: Field = Field::new(Region::Set, 18, 0, 9, true, true, &[]);
    pub const WGHT_G2Y: Field = Field::new(Region::Set, 18, 9, 9, true, true, &[]);
    pub const WGHT_R2Y: Field = Field::new(Region::Set, 18, 18, 9, true, true, &[]);
}

/// Register `src_udfu` at 0x004c: 1 word, ReadWrite.
pub mod src_udfu {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_udfu", 19, 1, Access::ReadWrite, 1);
    pub const WGHT_B2U: Field = Field::new(Region::Set, 19, 0, 9, true, true, &[]);
    pub const WGHT_G2U: Field = Field::new(Region::Set, 19, 9, 9, true, true, &[]);
    pub const WGHT_R2U: Field = Field::new(Region::Set, 19, 18, 9, true, true, &[]);
}

/// Register `src_udfv` at 0x0050: 1 word, ReadWrite.
pub mod src_udfv {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_udfv", 20, 1, Access::ReadWrite, 1);
    pub const WGHT_B2V: Field = Field::new(Region::Set, 20, 0, 9, true, true, &[]);
    pub const WGHT_G2V: Field = Field::new(Region::Set, 20, 9, 9, true, true, &[]);
    pub const WGHT_R2V: Field = Field::new(Region::Set, 20, 18, 9, true, true, &[]);
}

/// Register `src_udfo` at 0x0054: 1 word, ReadWrite.
pub mod src_udfo {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_udfo", 21, 1, Access::ReadWrite, 1);
    pub const OFST_V: Field = Field::new(Region::Set, 21, 0, 8, false, true, &[]);
    pub const OFST_U: Field = Field::new(Region::Set, 21, 8, 8, false, true, &[]);
    pub const OFST_Y: Field = Field::new(Region::Set, 21, 16, 5, false, true, &[]);
}

/// Register `src_proc` at 0x0058: 1 word, ReadWrite.
pub mod src_proc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_proc", 22, 1, Access::ReadWrite, 1);
    pub const SRC_MIRR: Field = Field::new(Region::Set, 22, 26, 1, false, true, &[]);
    pub const SRC_ROT: Field = Field::new(Region::Set, 22, 27, 2, false, true, &[]);
    pub const TXA_EN: Field = Field::new(Region::Set, 22, 29, 1, false, true, &[]);
    pub const AFBCD_EN: Field = Field::new(Region::Set, 22, 30, 1, false, true, &[]);
}

/// Register `tile_cfg` at 0x005c: 1 word, ReadWrite.
pub mod tile_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "tile_cfg", 23, 1, Access::ReadWrite, 1);
    pub const TILE_WIDTH_M1: Field = Field::new(Region::Set, 23, 0, 6, false, true, &[]);
    pub const TILE_HEIGHT_M1: Field = Field::new(Region::Set, 23, 16, 6, false, true, &[]);
    pub const TILE_EN: Field = Field::new(Region::Set, 23, 31, 1, false, true, &[]);
}

/// Register `tile_pos` at 0x0060: 1 word, ReadWrite.
pub mod tile_pos {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "tile_pos", 24, 1, Access::ReadWrite, 1);
    pub const TILE_X: Field = Field::new(Region::Set, 24, 0, 6, false, true, &[]);
    pub const TILE_Y: Field = Field::new(Region::Set, 24, 16, 6, false, true, &[]);
}

/// Register `klut_ofst` at 0x0064: 1 word, ReadWrite.
pub mod klut_ofst {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_ofst", 25, 1, Access::ReadWrite, 1);
    pub const CHRM_KULT_OFST: Field = Field::new(Region::Set, 25, 0, 3, false, true, &[]);
}

/// Register `klut_wgt0` at 0x0068: 1 word, ReadWrite.
pub mod klut_wgt0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt0", 26, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT0: Field = Field::new(Region::Set, 26, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT1: Field = Field::new(Region::Set, 26, 23, 9, false, true, &[]);
}

/// Register `klut_wgt1` at 0x006c: 1 word, ReadWrite.
pub mod klut_wgt1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt1", 27, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT1: Field = Field::new(Region::Set, 27, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT2: Field = Field::new(Region::Set, 27, 14, 18, false, true, &[]);
}

/// Register `klut_wgt2` at 0x0070: 1 word, ReadWrite.
pub mod klut_wgt2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt2", 28, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT3: Field = Field::new(Region::Set, 28, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT4: Field = Field::new(Region::Set, 28, 23, 9, false, true, &[]);
}

/// Register `klut_wgt3` at 0x0074: 1 word, ReadWrite.
pub mod klut_wgt3 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt3", 29, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT4: Field = Field::new(Region::Set, 29, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT5: Field = Field::new(Region::Set, 29, 14, 18, false, true, &[]);
}

/// Register `klut_wgt4` at 0x0078: 1 word, ReadWrite.
pub mod klut_wgt4 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt4", 30, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT6: Field = Field::new(Region::Set, 30, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT7: Field = Field::new(Region::Set, 30, 23, 9, false, true, &[]);
}

/// Register `klut_wgt5` at 0x007c: 1 word, ReadWrite.
pub mod klut_wgt5 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt5", 31, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT7: Field = Field::new(Region::Set, 31, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT8: Field = Field::new(Region::Set, 31, 14, 18, false, true, &[]);
}

/// Register `klut_wgt6` at 0x0080: 1 word, ReadWrite.
pub mod klut_wgt6 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt6", 32, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT9: Field = Field::new(Region::Set, 32, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT10: Field = Field::new(Region::Set, 32, 23, 9, false, true, &[]);
}

/// Register `klut_wgt7` at 0x0084: 1 word, ReadWrite.
pub mod klut_wgt7 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt7", 33, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT10: Field = Field::new(Region::Set, 33, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT11: Field = Field::new(Region::Set, 33, 14, 18, false, true, &[]);
}

/// Register `klut_wgt8` at 0x0088: 1 word, ReadWrite.
pub mod klut_wgt8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt8", 34, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT12: Field = Field::new(Region::Set, 34, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT13: Field = Field::new(Region::Set, 34, 23, 9, false, true, &[]);
}

/// Register `klut_wgt9` at 0x008c: 1 word, ReadWrite.
pub mod klut_wgt9 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt9", 35, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT13: Field = Field::new(Region::Set, 35, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT14: Field = Field::new(Region::Set, 35, 14, 18, false, true, &[]);
}

/// Register `klut_wgt10` at 0x0090: 1 word, ReadWrite.
pub mod klut_wgt10 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt10", 36, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT15: Field = Field::new(Region::Set, 36, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT16: Field = Field::new(Region::Set, 36, 23, 9, false, true, &[]);
}

/// Register `klut_wgt11` at 0x0094: 1 word, ReadWrite.
pub mod klut_wgt11 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt11", 37, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT16: Field = Field::new(Region::Set, 37, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT17: Field = Field::new(Region::Set, 37, 14, 18, false, true, &[]);
}

/// Register `klut_wgt12` at 0x0098: 1 word, ReadWrite.
pub mod klut_wgt12 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt12", 38, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT18: Field = Field::new(Region::Set, 38, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT19: Field = Field::new(Region::Set, 38, 23, 9, false, true, &[]);
}

/// Register `klut_wgt13` at 0x009c: 1 word, ReadWrite.
pub mod klut_wgt13 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt13", 39, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT19: Field = Field::new(Region::Set, 39, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT20: Field = Field::new(Region::Set, 39, 14, 18, false, true, &[]);
}

/// Register `klut_wgt14` at 0x00a0: 1 word, ReadWrite.
pub mod klut_wgt14 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt14", 40, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT21: Field = Field::new(Region::Set, 40, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT22: Field = Field::new(Region::Set, 40, 23, 9, false, true, &[]);
}

/// Register `klut_wgt15` at 0x00a4: 1 word, ReadWrite.
pub mod klut_wgt15 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt15", 41, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT22: Field = Field::new(Region::Set, 41, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT23: Field = Field::new(Region::Set, 41, 14, 18, false, true, &[]);
}

/// Register `klut_wgt16` at 0x00a8: 1 word, ReadWrite.
pub mod klut_wgt16 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt16", 42, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT24: Field = Field::new(Region::Set, 42, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT25: Field = Field::new(Region::Set, 42, 23, 9, false, true, &[]);
}

/// Register `klut_wgt17` at 0x00ac: 1 word, ReadWrite.
pub mod klut_wgt17 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt17", 43, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT25: Field = Field::new(Region::Set, 43, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT26: Field = Field::new(Region::Set, 43, 14, 18, false, true, &[]);
}

/// Register `klut_wgt18` at 0x00b0: 1 word, ReadWrite.
pub mod klut_wgt18 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt18", 44, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT27: Field = Field::new(Region::Set, 44, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT28: Field = Field::new(Region::Set, 44, 23, 9, false, true, &[]);
}

/// Register `klut_wgt19` at 0x00b4: 1 word, ReadWrite.
pub mod klut_wgt19 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt19", 45, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT28: Field = Field::new(Region::Set, 45, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT29: Field = Field::new(Region::Set, 45, 14, 18, false, true, &[]);
}

/// Register `klut_wgt20` at 0x00b8: 1 word, ReadWrite.
pub mod klut_wgt20 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt20", 46, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT30: Field = Field::new(Region::Set, 46, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT31: Field = Field::new(Region::Set, 46, 23, 9, false, true, &[]);
}

/// Register `klut_wgt21` at 0x00bc: 1 word, ReadWrite.
pub mod klut_wgt21 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt21", 47, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT31: Field = Field::new(Region::Set, 47, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT32: Field = Field::new(Region::Set, 47, 14, 18, false, true, &[]);
}

/// Register `klut_wgt22` at 0x00c0: 1 word, ReadWrite.
pub mod klut_wgt22 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt22", 48, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT33: Field = Field::new(Region::Set, 48, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT34: Field = Field::new(Region::Set, 48, 23, 9, false, true, &[]);
}

/// Register `klut_wgt23` at 0x00c4: 1 word, ReadWrite.
pub mod klut_wgt23 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "klut_wgt23", 49, 1, Access::ReadWrite, 1);
    pub const CHRM_KLUT_WGT34: Field = Field::new(Region::Set, 49, 0, 9, false, true, &[]);
}

/// Register `rc_cfg` at 0x00c8: 1 word, ReadWrite.
pub mod rc_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rc_cfg", 50, 1, Access::ReadWrite, 1);
    pub const RC_EN: Field = Field::new(Region::Set, 50, 0, 1, false, true, &[]);
    pub const AQMODE_EN: Field = Field::new(Region::Set, 50, 1, 1, false, true, &[]);
    pub const QP_MODE: Field = Field::new(Region::Set, 50, 2, 1, false, true, &[]);
    pub const RC_CTU_NUM: Field = Field::new(Region::Set, 50, 16, 16, false, true, &[]);
}

/// Register `rc_qp` at 0x00cc: 1 word, ReadWrite.
pub mod rc_qp {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(Region::Set, "rc_qp", 51, 1, Access::ReadWrite, 1);
    pub const RC_QP_RANGE: Field = Field::new(Region::Set, 51, 16, 4, false, true, &[]);
    pub const RC_MAX_QP: Field = Field::new(Region::Set, 51, 20, 6, false, true, &[]);
    pub const RC_MIN_QP: Field = Field::new(Region::Set, 51, 26, 6, false, true, &[]);
}

/// Register `rc_tgt` at 0x00d0: 1 word, ReadWrite.
pub mod rc_tgt {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rc_tgt", 52, 1, Access::ReadWrite, 1);
    pub const CTU_EBITS: Field = Field::new(Region::Set, 52, 0, 20, false, true, &[]);
}

/// Register `rc_adj0` at 0x00d4: 1 word, ReadWrite.
pub mod rc_adj0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rc_adj0", 53, 1, Access::ReadWrite, 1);
    pub const QP_ADJUST0: Field = Field::new(Region::Set, 53, 0, 5, true, true, &[]);
    pub const QP_ADJUST1: Field = Field::new(Region::Set, 53, 5, 5, true, true, &[]);
    pub const QP_ADJUST2: Field = Field::new(Region::Set, 53, 10, 5, true, true, &[]);
    pub const QP_ADJUST3: Field = Field::new(Region::Set, 53, 15, 5, true, true, &[]);
    pub const QP_ADJUST4: Field = Field::new(Region::Set, 53, 20, 5, true, true, &[]);
}

/// Register `rc_adj1` at 0x00d8: 1 word, ReadWrite.
pub mod rc_adj1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rc_adj1", 54, 1, Access::ReadWrite, 1);
    pub const QP_ADJUST5: Field = Field::new(Region::Set, 54, 0, 5, true, true, &[]);
    pub const QP_ADJUST6: Field = Field::new(Region::Set, 54, 5, 5, true, true, &[]);
    pub const QP_ADJUST7: Field = Field::new(Region::Set, 54, 10, 5, true, true, &[]);
    pub const QP_ADJUST8: Field = Field::new(Region::Set, 54, 15, 5, true, true, &[]);
}

/// Register `rc_erp0_bits_thd0` at 0x00dc: 1 word, ReadWrite.
pub mod rc_erp0_bits_thd0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp0_bits_thd0",
        55,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD0: Field = Field::new(Region::Set, 55, 0, 32, true, true, &[]);
}

/// Register `rc_erp1_bits_thd1` at 0x00e0: 1 word, ReadWrite.
pub mod rc_erp1_bits_thd1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp1_bits_thd1",
        56,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD1: Field = Field::new(Region::Set, 56, 0, 32, true, true, &[]);
}

/// Register `rc_erp2_bits_thd2` at 0x00e4: 1 word, ReadWrite.
pub mod rc_erp2_bits_thd2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp2_bits_thd2",
        57,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD2: Field = Field::new(Region::Set, 57, 0, 32, true, true, &[]);
}

/// Register `rc_erp3_bits_thd3` at 0x00e8: 1 word, ReadWrite.
pub mod rc_erp3_bits_thd3 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp3_bits_thd3",
        58,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD3: Field = Field::new(Region::Set, 58, 0, 32, true, true, &[]);
}

/// Register `rc_erp4_bits_thd4` at 0x00ec: 1 word, ReadWrite.
pub mod rc_erp4_bits_thd4 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp4_bits_thd4",
        59,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD4: Field = Field::new(Region::Set, 59, 0, 32, true, true, &[]);
}

/// Register `rc_erp5_bits_thd5` at 0x00f0: 1 word, ReadWrite.
pub mod rc_erp5_bits_thd5 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp5_bits_thd5",
        60,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD5: Field = Field::new(Region::Set, 60, 0, 32, true, true, &[]);
}

/// Register `rc_erp6_bits_thd6` at 0x00f4: 1 word, ReadWrite.
pub mod rc_erp6_bits_thd6 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp6_bits_thd6",
        61,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD6: Field = Field::new(Region::Set, 61, 0, 32, true, true, &[]);
}

/// Register `rc_erp7_bits_thd7` at 0x00f8: 1 word, ReadWrite.
pub mod rc_erp7_bits_thd7 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp7_bits_thd7",
        62,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD7: Field = Field::new(Region::Set, 62, 0, 32, true, true, &[]);
}

/// Register `rc_erp8_bits_thd8` at 0x00fc: 1 word, ReadWrite.
pub mod rc_erp8_bits_thd8 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Set,
        "rc_erp8_bits_thd8",
        63,
        1,
        Access::ReadWrite,
        1,
    );
    pub const BITS_THD8: Field = Field::new(Region::Set, 63, 0, 32, true, true, &[]);
}

/// Register `qpmap0` at 0x0100: 1 word, ReadWrite.
pub mod qpmap0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "qpmap0", 64, 1, Access::ReadWrite, 1);
    pub const QPMIN_AREA0: Field = Field::new(Region::Set, 64, 0, 6, false, true, &[]);
    pub const QPMAX_AREA0: Field = Field::new(Region::Set, 64, 6, 6, false, true, &[]);
    pub const QPMIN_AREA1: Field = Field::new(Region::Set, 64, 12, 6, false, true, &[]);
    pub const QPMAX_AREA1: Field = Field::new(Region::Set, 64, 18, 6, false, true, &[]);
    pub const QPMIN_AREA2: Field = Field::new(Region::Set, 64, 24, 6, false, true, &[]);
}

/// Register `qpmap1` at 0x0104: 1 word, ReadWrite.
pub mod qpmap1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "qpmap1", 65, 1, Access::ReadWrite, 1);
    pub const QPMAX_AREA2: Field = Field::new(Region::Set, 65, 0, 6, false, true, &[]);
    pub const QPMIN_AREA3: Field = Field::new(Region::Set, 65, 6, 6, false, true, &[]);
    pub const QPMAX_AREA3: Field = Field::new(Region::Set, 65, 12, 6, false, true, &[]);
    pub const QPMIN_AREA4: Field = Field::new(Region::Set, 65, 18, 6, false, true, &[]);
    pub const QPMAX_AREA4: Field = Field::new(Region::Set, 65, 24, 6, false, true, &[]);
}

/// Register `qpmap2` at 0x0108: 1 word, ReadWrite.
pub mod qpmap2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "qpmap2", 66, 1, Access::ReadWrite, 1);
    pub const QPMIN_AREA5: Field = Field::new(Region::Set, 66, 0, 6, false, true, &[]);
    pub const QPMAX_AREA5: Field = Field::new(Region::Set, 66, 6, 6, false, true, &[]);
    pub const QPMIN_AREA6: Field = Field::new(Region::Set, 66, 12, 6, false, true, &[]);
    pub const QPMAX_AREA6: Field = Field::new(Region::Set, 66, 18, 6, false, true, &[]);
    pub const QPMIN_AREA7: Field = Field::new(Region::Set, 66, 24, 6, false, true, &[]);
}

/// Register `qpmap3` at 0x010c: 1 word, ReadWrite.
pub mod qpmap3 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "qpmap3", 67, 1, Access::ReadWrite, 1);
    pub const QPMAX_AREA7: Field = Field::new(Region::Set, 67, 0, 6, false, true, &[]);
    pub const QPMAP_MODE: Field = Field::new(Region::Set, 67, 30, 2, false, true, &[]);
}

/// Register `pic_ofst` at 0x0110: 1 word, ReadWrite.
pub mod pic_ofst {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "pic_ofst", 68, 1, Access::ReadWrite, 1);
    pub const PIC_OFST_Y: Field = Field::new(Region::Set, 68, 0, 13, false, true, &[]);
    pub const PIC_OFST_X: Field = Field::new(Region::Set, 68, 16, 13, false, true, &[]);
}

/// Register `src_strid` at 0x0114: 1 word, ReadWrite.
pub mod src_strid {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "src_strid", 69, 1, Access::ReadWrite, 1);
    pub const SRC_YSTRID: Field = Field::new(Region::Set, 69, 0, 16, false, true, &[]);
    pub const SRC_CSTRID: Field = Field::new(Region::Set, 69, 16, 16, false, true, &[]);
}

/// Register `adr_srcy_hevc` at 0x0118: 1 word, ReadWrite.
pub mod adr_srcy_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "adr_srcy_hevc", 70, 1, Access::ReadWrite, 1);
    pub const ADR_SRCY_HEVC: AddressReg = AddressReg::new(Region::Set, 70, 1);
}

/// Register `adr_srcu_hevc` at 0x011c: 1 word, ReadWrite.
pub mod adr_srcu_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "adr_srcu_hevc", 71, 1, Access::ReadWrite, 1);
    pub const ADR_SRCU_HEVC: AddressReg = AddressReg::new(Region::Set, 71, 1);
}

/// Register `adr_srcv_hevc` at 0x0120: 1 word, ReadWrite.
pub mod adr_srcv_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "adr_srcv_hevc", 72, 1, Access::ReadWrite, 1);
    pub const ADR_SRCV_HEVC: AddressReg = AddressReg::new(Region::Set, 72, 1);
}

/// Register `roi_addr_hevc` at 0x0124: 1 word, ReadWrite.
pub mod roi_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "roi_addr_hevc", 73, 1, Access::ReadWrite, 1);
    pub const ROI_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 73, 1);
}

/// Register `rfpw_h_addr_hevc` at 0x0128: 1 word, ReadWrite.
pub mod rfpw_h_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rfpw_h_addr_hevc", 74, 1, Access::ReadWrite, 1);
    pub const RFPW_H_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 74, 1);
}

/// Register `rfpw_b_addr_hevc` at 0x012c: 1 word, ReadWrite.
pub mod rfpw_b_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rfpw_b_addr_hevc", 75, 1, Access::ReadWrite, 1);
    pub const RFPW_B_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 75, 1);
}

/// Register `rfpr_h_addr_hevc` at 0x0130: 1 word, ReadWrite.
pub mod rfpr_h_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rfpr_h_addr_hevc", 76, 1, Access::ReadWrite, 1);
    pub const RFPR_H_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 76, 1);
}

/// Register `rfpr_b_addr_hevc` at 0x0134: 1 word, ReadWrite.
pub mod rfpr_b_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rfpr_b_addr_hevc", 77, 1, Access::ReadWrite, 1);
    pub const RFPR_B_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 77, 1);
}

/// Register `cmvw_addr_hevc` at 0x0138: 1 word, ReadWrite.
pub mod cmvw_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "cmvw_addr_hevc", 78, 1, Access::ReadWrite, 1);
    pub const CMVW_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 78, 1);
}

/// Register `cmvr_addr_hevc` at 0x013c: 1 word, ReadWrite.
pub mod cmvr_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "cmvr_addr_hevc", 79, 1, Access::ReadWrite, 1);
    pub const CMVR_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 79, 1);
}

/// Register `dspw_addr_hevc` at 0x0140: 1 word, ReadWrite.
pub mod dspw_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "dspw_addr_hevc", 80, 1, Access::ReadWrite, 1);
    pub const DSPW_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 80, 1);
}

/// Register `dspr_addr_hevc` at 0x0144: 1 word, ReadWrite.
pub mod dspr_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "dspr_addr_hevc", 81, 1, Access::ReadWrite, 1);
    pub const DSPR_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 81, 1);
}

/// Register `meiw_addr_hevc` at 0x0148: 1 word, ReadWrite.
pub mod meiw_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "meiw_addr_hevc", 82, 1, Access::ReadWrite, 1);
    pub const MEIW_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 82, 1);
}

/// Register `bsbt_addr_hevc` at 0x014c: 1 word, ReadWrite.
pub mod bsbt_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "bsbt_addr_hevc", 83, 1, Access::ReadWrite, 1);
    pub const BSBT_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 83, 1);
}

/// Register `bsbb_addr_hevc` at 0x0150: 1 word, ReadWrite.
pub mod bsbb_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "bsbb_addr_hevc", 84, 1, Access::ReadWrite, 1);
    pub const BSBB_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 84, 1);
}

/// Register `bsbr_addr_hevc` at 0x0154: 1 word, ReadWrite.
pub mod bsbr_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "bsbr_addr_hevc", 85, 1, Access::ReadWrite, 1);
    pub const BSBR_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 85, 1);
}

/// Register `bsbw_addr_hevc` at 0x0158: 1 word, ReadWrite.
pub mod bsbw_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "bsbw_addr_hevc", 86, 1, Access::ReadWrite, 1);
    pub const BSBW_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 86, 1);
}

/// Register `sli_spl` at 0x015c: 1 word, ReadWrite.
pub mod sli_spl {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "sli_spl", 87, 1, Access::ReadWrite, 1);
    pub const SLI_SPLT: Field = Field::new(Region::Set, 87, 0, 1, false, true, &[]);
    pub const SLI_SPLT_MODE: Field = Field::new(Region::Set, 87, 1, 1, false, true, &[]);
    pub const SLI_SPLT_CPST: Field = Field::new(Region::Set, 87, 2, 1, false, true, &[]);
    pub const SLI_MAX_NUM_M1: Field = Field::new(Region::Set, 87, 3, 10, false, true, &[]);
    pub const SLI_FLSH: Field = Field::new(Region::Set, 87, 13, 1, false, true, &[]);
    pub const SLI_SPLT_CNUM_M1: Field = Field::new(Region::Set, 87, 16, 16, false, true, &[]);
}

/// Register `sli_spl_byte` at 0x0160: 1 word, ReadWrite.
pub mod sli_spl_byte {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "sli_spl_byte", 88, 1, Access::ReadWrite, 1);
    pub const SLI_SPLT_BYTE: Field = Field::new(Region::Set, 88, 0, 18, false, true, &[]);
}

/// Register `me_rnge` at 0x0164: 1 word, ReadWrite.
pub mod me_rnge {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "me_rnge", 89, 1, Access::ReadWrite, 1);
    pub const CIME_SRCH_H: Field = Field::new(Region::Set, 89, 0, 4, false, true, &[]);
    pub const CIME_SRCH_V: Field = Field::new(Region::Set, 89, 4, 4, false, true, &[]);
    pub const RIME_SRCH_H: Field = Field::new(Region::Set, 89, 8, 3, false, true, &[]);
    pub const RIME_SRCH_V: Field = Field::new(Region::Set, 89, 11, 3, false, true, &[]);
    pub const DLT_FRM_NUM: Field = Field::new(Region::Set, 89, 16, 16, false, true, &[]);
}

/// Register `me_cnst` at 0x0168: 1 word, ReadWrite.
pub mod me_cnst {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "me_cnst", 90, 1, Access::ReadWrite, 1);
    pub const PMV_MDST_H: Field = Field::new(Region::Set, 90, 0, 8, false, true, &[]);
    pub const PMV_MDST_V: Field = Field::new(Region::Set, 90, 8, 8, false, true, &[]);
    pub const MV_LIMIT: Field = Field::new(Region::Set, 90, 16, 2, false, true, &[]);
    pub const MV_NUM: Field = Field::new(Region::Set, 90, 18, 2, false, true, &[]);
    pub const COLMV_STORE: Field = Field::new(Region::Set, 90, 20, 1, false, true, &[]);
    pub const COLMV_LOAD: Field = Field::new(Region::Set, 90, 21, 1, false, true, &[]);
    pub const RIME_DIS_EN: Field = Field::new(Region::Set, 90, 22, 5, false, true, &[]);
    pub const FME_DIS_EN: Field = Field::new(Region::Set, 90, 27, 5, false, true, &[]);
}

/// Register `me_ram` at 0x016c: 1 word, ReadWrite.
pub mod me_ram {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "me_ram", 91, 1, Access::ReadWrite, 1);
    pub const CIME_RAMA_MAX: Field = Field::new(Region::Set, 91, 0, 11, false, true, &[]);
    pub const CIME_RAMA_H: Field = Field::new(Region::Set, 91, 11, 5, false, true, &[]);
    pub const CACH_L2_TAG: Field = Field::new(Region::Set, 91, 16, 2, false, true, &[]);
    pub const CIME_LINEBUF_W: Field = Field::new(Region::Set, 91, 18, 8, false, true, &[]);
}

/// Register `synt_ref_mark4` at 0x0170: 1 word, ReadWrite.
pub mod synt_ref_mark4 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark4", 92, 1, Access::ReadWrite, 1);
    pub const POC_LSB_LT1: Field = Field::new(Region::Set, 92, 0, 16, false, true, &[]);
    pub const POC_LSB_LT2: Field = Field::new(Region::Set, 92, 16, 16, false, true, &[]);
}

/// Register `synt_ref_mark5` at 0x0174: 1 word, ReadWrite.
pub mod synt_ref_mark5 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark5", 93, 1, Access::ReadWrite, 1);
    pub const DLT_POC_MSB_CYCL1: Field = Field::new(Region::Set, 93, 0, 16, false, true, &[]);
    pub const DLT_POC_MSB_CYCL2: Field = Field::new(Region::Set, 93, 16, 16, false, true, &[]);
}

/// Register `osd_inv_cfg` at 0x0178: 1 word, ReadWrite.
pub mod osd_inv_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "osd_inv_cfg", 94, 1, Access::ReadWrite, 1);
    pub const OSD_CH_INV_EN: Field = Field::new(Region::Set, 94, 0, 8, false, true, &[]);
    pub const OSD_ITYPE: Field = Field::new(Region::Set, 94, 8, 8, false, true, &[]);
    pub const OSD_LU_INV_MSK: Field = Field::new(Region::Set, 94, 16, 8, false, true, &[]);
    pub const OSD_CH_INV_MSK: Field = Field::new(Region::Set, 94, 24, 8, false, true, &[]);
}

/// Register `lpfw_addr_hevc` at 0x017c: 1 word, ReadWrite.
pub mod lpfw_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "lpfw_addr_hevc", 95, 1, Access::ReadWrite, 1);
    pub const LPFW_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 95, 1);
}

/// Register `lpfr_addr_hevc` at 0x0180: 1 word, ReadWrite.
pub mod lpfr_addr_hevc {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "lpfr_addr_hevc", 96, 1, Access::ReadWrite, 1);
    pub const LPFR_ADDR_HEVC: AddressReg = AddressReg::new(Region::Set, 96, 1);
}

/// Register `rdo_cfg` at 0x0198: 1 word, ReadWrite.
pub mod rdo_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "rdo_cfg", 102, 1, Access::ReadWrite, 1);
    pub const LTM_COL: Field = Field::new(Region::Set, 102, 0, 1, false, true, &[]);
    pub const LTM_IDX0L0: Field = Field::new(Region::Set, 102, 1, 1, false, true, &[]);
    pub const CHRM_SPECIAL: Field = Field::new(Region::Set, 102, 2, 1, false, true, &[]);
    pub const RDOQ_EN: Field = Field::new(Region::Set, 102, 3, 1, false, true, &[]);
    pub const CU_INTER_EN: Field = Field::new(Region::Set, 102, 6, 4, false, true, &[]);
    pub const CU_INTRA_EN: Field = Field::new(Region::Set, 102, 19, 4, false, true, &[]);
    pub const CHRM_KLUT_EN: Field = Field::new(Region::Set, 102, 23, 1, false, true, &[]);
    pub const SEQ_SCALING_MATRIX_PRESENT_FLG: Field =
        Field::new(Region::Set, 102, 24, 2, false, true, &[]);
    pub const STAD_BYPS_FLG: Field = Field::new(Region::Set, 102, 31, 1, false, true, &[]);
}

/// Register `synt_nal` at 0x019c: 1 word, ReadWrite.
pub mod synt_nal {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_nal", 103, 1, Access::ReadWrite, 1);
    pub const NAL_UNIT_TYPE: Field = Field::new(Region::Set, 103, 0, 6, false, true, &[]);
}

/// Register `synt_sps` at 0x01a0: 1 word, ReadWrite.
pub mod synt_sps {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_sps", 104, 1, Access::ReadWrite, 1);
    pub const SMPL_ADPT_OFST_EN: Field = Field::new(Region::Set, 104, 0, 1, false, true, &[]);
    pub const NUM_ST_REF_PIC: Field = Field::new(Region::Set, 104, 1, 7, false, true, &[]);
    pub const LT_REF_PIC_PRSNT: Field = Field::new(Region::Set, 104, 8, 1, false, true, &[]);
    pub const NUM_LT_REF_PIC: Field = Field::new(Region::Set, 104, 9, 6, false, true, &[]);
    pub const TMPL_MVP_EN: Field = Field::new(Region::Set, 104, 15, 1, false, true, &[]);
    pub const LOG2_MAX_POC_LSB: Field = Field::new(Region::Set, 104, 16, 4, false, true, &[]);
    pub const STRG_INTRA_SMTH: Field = Field::new(Region::Set, 104, 20, 1, false, true, &[]);
}

/// Register `synt_pps` at 0x01a4: 1 word, ReadWrite.
pub mod synt_pps {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_pps", 105, 1, Access::ReadWrite, 1);
    pub const DPDNT_SLI_SEG_EN: Field = Field::new(Region::Set, 105, 0, 1, false, true, &[]);
    pub const OUT_FLG_PRSNT_FLG: Field = Field::new(Region::Set, 105, 1, 1, false, true, &[]);
    pub const NUM_EXTR_SLI_HDR: Field = Field::new(Region::Set, 105, 2, 3, false, true, &[]);
    pub const SGN_DAT_HID_EN: Field = Field::new(Region::Set, 105, 5, 1, false, true, &[]);
    pub const CBC_INIT_PRSNT_FLG: Field = Field::new(Region::Set, 105, 6, 1, false, true, &[]);
    pub const PIC_INIT_QP: Field = Field::new(Region::Set, 105, 7, 6, false, true, &[]);
    pub const CU_QP_DLT_EN: Field = Field::new(Region::Set, 105, 13, 1, false, true, &[]);
    pub const CHRM_QP_OFST_PRSN: Field = Field::new(Region::Set, 105, 14, 1, false, true, &[]);
    pub const LP_FLTR_ACRS_SLI: Field = Field::new(Region::Set, 105, 15, 1, false, true, &[]);
    pub const DBLK_FLTR_OVRD_EN: Field = Field::new(Region::Set, 105, 16, 1, false, true, &[]);
    pub const LST_MDFY_PRSNT_FLG: Field = Field::new(Region::Set, 105, 17, 1, false, true, &[]);
    pub const SLI_SEG_HDR_EXTN: Field = Field::new(Region::Set, 105, 18, 1, false, true, &[]);
    pub const CU_QP_DLT_DEPTH: Field = Field::new(Region::Set, 105, 19, 2, false, true, &[]);
    pub const LPF_FLTR_ACRS_TIL: Field = Field::new(Region::Set, 105, 21, 1, false, true, &[]);
}

/// Register `synt_sli0` at 0x01a8: 1 word, ReadWrite.
pub mod synt_sli0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_sli0", 106, 1, Access::ReadWrite, 1);
    pub const CBC_INIT_FLG: Field = Field::new(Region::Set, 106, 0, 1, false, true, &[]);
    pub const MVD_L1_ZERO_FLG: Field = Field::new(Region::Set, 106, 1, 1, false, true, &[]);
    pub const MERGE_UP_FLAG: Field = Field::new(Region::Set, 106, 2, 1, false, true, &[]);
    pub const MERGE_LEFT_FLAG: Field = Field::new(Region::Set, 106, 3, 1, false, true, &[]);
    pub const REF_PIC_LST_MDF_L0: Field = Field::new(Region::Set, 106, 5, 1, false, true, &[]);
    pub const NUM_REFIDX_L1_ACT: Field = Field::new(Region::Set, 106, 6, 2, false, true, &[]);
    pub const NUM_REFIDX_L0_ACT: Field = Field::new(Region::Set, 106, 8, 2, false, true, &[]);
    pub const NUM_REFIDX_ACT_OVRD: Field = Field::new(Region::Set, 106, 10, 1, false, true, &[]);
    pub const SLI_SAO_CHRM_FLG: Field = Field::new(Region::Set, 106, 11, 1, false, true, &[]);
    pub const SLI_SAO_LUMA_FLG: Field = Field::new(Region::Set, 106, 12, 1, false, true, &[]);
    pub const SLI_TMPRL_MVP_EN: Field = Field::new(Region::Set, 106, 13, 1, false, true, &[]);
    pub const PIC_OUT_FLG: Field = Field::new(Region::Set, 106, 14, 1, false, true, &[]);
    pub const SLI_TYPE: Field = Field::new(Region::Set, 106, 15, 2, false, true, &[]);
    pub const SLI_RSRV_FLG: Field = Field::new(Region::Set, 106, 17, 7, false, true, &[]);
    pub const DPDNT_SLI_SEG_FLG: Field = Field::new(Region::Set, 106, 24, 1, false, true, &[]);
    pub const SLI_PPS_ID: Field = Field::new(Region::Set, 106, 25, 6, false, true, &[]);
    pub const NO_OUT_PRI_PIC: Field = Field::new(Region::Set, 106, 31, 1, false, true, &[]);
}

/// Register `synt_sli1` at 0x01ac: 1 word, ReadWrite.
pub mod synt_sli1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_sli1", 107, 1, Access::ReadWrite, 1);
    pub const SLI_TC_OFST_DIV2: Field = Field::new(Region::Set, 107, 0, 4, true, true, &[]);
    pub const SLI_BETA_OFST_DIV2: Field = Field::new(Region::Set, 107, 4, 4, true, true, &[]);
    pub const SLI_LP_FLTR_ACRS_SLI: Field = Field::new(Region::Set, 107, 8, 1, false, true, &[]);
    pub const SLI_DBLK_FLTR_DIS: Field = Field::new(Region::Set, 107, 9, 1, false, true, &[]);
    pub const DBLK_FLTR_OVRD_FLG: Field = Field::new(Region::Set, 107, 10, 1, false, true, &[]);
    pub const SLI_CB_QP_OFST: Field = Field::new(Region::Set, 107, 11, 5, true, true, &[]);
    pub const SLI_QP: Field = Field::new(Region::Set, 107, 16, 6, false, true, &[]);
    pub const MAX_MRG_CND: Field = Field::new(Region::Set, 107, 22, 3, false, true, &[]);
    pub const COL_REF_IDX: Field = Field::new(Region::Set, 107, 25, 1, false, true, &[]);
    pub const COL_FRM_L0_FLG: Field = Field::new(Region::Set, 107, 26, 1, false, true, &[]);
    pub const LST_ENTRY_L0: Field = Field::new(Region::Set, 107, 27, 4, false, true, &[]);
}

/// Register `synt_sli2_rodr` at 0x01b0: 1 word, ReadWrite.
pub mod synt_sli2_rodr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_sli2_rodr", 108, 1, Access::ReadWrite, 1);
    pub const SLI_POC_LSB: Field = Field::new(Region::Set, 108, 0, 16, false, true, &[]);
    pub const SLI_HDR_EXT_LEN: Field = Field::new(Region::Set, 108, 16, 9, false, true, &[]);
}

/// Register `synt_ref_mark0` at 0x01b4: 1 word, ReadWrite.
pub mod synt_ref_mark0 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark0", 109, 1, Access::ReadWrite, 1);
    pub const ST_REF_PIC_FLG: Field = Field::new(Region::Set, 109, 0, 1, false, true, &[]);
    pub const POC_LSB_LT0: Field = Field::new(Region::Set, 109, 1, 16, false, true, &[]);
    pub const LT_IDX_SPS: Field = Field::new(Region::Set, 109, 17, 5, false, true, &[]);
    pub const NUM_LT_PIC: Field = Field::new(Region::Set, 109, 22, 2, false, true, &[]);
    pub const ST_REF_PIC_IDX: Field = Field::new(Region::Set, 109, 24, 6, false, true, &[]);
    pub const NUM_LT_SPS: Field = Field::new(Region::Set, 109, 30, 2, false, true, &[]);
}

/// Register `synt_ref_mark1` at 0x01b8: 1 word, ReadWrite.
pub mod synt_ref_mark1 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark1", 110, 1, Access::ReadWrite, 1);
    pub const USED_BY_S0_FLG: Field = Field::new(Region::Set, 110, 0, 4, false, true, &[]);
    pub const NUM_POS_PIC: Field = Field::new(Region::Set, 110, 4, 1, false, true, &[]);
    pub const NUM_NEG_PIC: Field = Field::new(Region::Set, 110, 5, 5, false, true, &[]);
    pub const DLT_POC_MSB_CYCL0: Field = Field::new(Region::Set, 110, 10, 16, false, true, &[]);
    pub const DLT_POC_MSB_PRSNT0: Field = Field::new(Region::Set, 110, 26, 1, false, true, &[]);
    pub const DLT_POC_MSB_PRSNT1: Field = Field::new(Region::Set, 110, 27, 1, false, true, &[]);
    pub const DLT_POC_MSB_PRSNT2: Field = Field::new(Region::Set, 110, 28, 1, false, true, &[]);
    pub const USED_BY_LT_FLG0: Field = Field::new(Region::Set, 110, 29, 1, false, true, &[]);
    pub const USED_BY_LT_FLG1: Field = Field::new(Region::Set, 110, 30, 1, false, true, &[]);
    pub const USED_BY_LT_FLG2: Field = Field::new(Region::Set, 110, 31, 1, false, true, &[]);
}

/// Register `osd_cfg` at 0x01c0: 1 word, ReadWrite.
pub mod osd_cfg {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "osd_cfg", 112, 1, Access::ReadWrite, 1);
    pub const OSD_EN: Field = Field::new(Region::Set, 112, 0, 8, false, true, &[]);
    pub const OSD_INV: Field = Field::new(Region::Set, 112, 8, 8, false, true, &[]);
    pub const OSD_CLK_SEL: Field = Field::new(Region::Set, 112, 16, 1, false, true, &[]);
    pub const OSD_PLT_TYPE: Field = Field::new(Region::Set, 112, 17, 1, false, true, &[]);
}

/// Register `osd_inv` at 0x01c4: 1 word, ReadWrite.
pub mod osd_inv {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "osd_inv", 113, 1, Access::ReadWrite, 1);
    pub const OSD_INV_R0: Field = Field::new(Region::Set, 113, 0, 4, false, true, &[]);
    pub const OSD_INV_R1: Field = Field::new(Region::Set, 113, 4, 4, false, true, &[]);
    pub const OSD_INV_R2: Field = Field::new(Region::Set, 113, 8, 4, false, true, &[]);
    pub const OSD_INV_R3: Field = Field::new(Region::Set, 113, 12, 4, false, true, &[]);
    pub const OSD_INV_R4: Field = Field::new(Region::Set, 113, 16, 4, false, true, &[]);
    pub const OSD_INV_R5: Field = Field::new(Region::Set, 113, 20, 4, false, true, &[]);
    pub const OSD_INV_R6: Field = Field::new(Region::Set, 113, 24, 4, false, true, &[]);
    pub const OSD_INV_R7: Field = Field::new(Region::Set, 113, 28, 4, false, true, &[]);
}

/// Register `synt_ref_mark2` at 0x01c8: 1 word, ReadWrite.
pub mod synt_ref_mark2 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark2", 114, 1, Access::ReadWrite, 1);
    pub const DLT_POC_S0_M10: Field = Field::new(Region::Set, 114, 0, 16, false, true, &[]);
    pub const DLT_POC_S0_M11: Field = Field::new(Region::Set, 114, 16, 16, false, true, &[]);
}

/// Register `synt_ref_mark3` at 0x01cc: 1 word, ReadWrite.
pub mod synt_ref_mark3 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "synt_ref_mark3", 115, 1, Access::ReadWrite, 1);
    pub const DLT_POC_S0_M12: Field = Field::new(Region::Set, 115, 0, 16, false, true, &[]);
    pub const DLT_POC_S0_M13: Field = Field::new(Region::Set, 115, 16, 16, false, true, &[]);
}

/// Register `osd_pos` at 0x01d0: 8 words, ReadWrite.
pub mod osd_pos {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "osd_pos", 116, 8, Access::ReadWrite, 1);
    pub const LT_POS_X: [Field; 8] = [
        Field::new(Region::Set, 116, 0, 8, false, true, &[]),
        Field::new(Region::Set, 117, 0, 8, false, true, &[]),
        Field::new(Region::Set, 118, 0, 8, false, true, &[]),
        Field::new(Region::Set, 119, 0, 8, false, true, &[]),
        Field::new(Region::Set, 120, 0, 8, false, true, &[]),
        Field::new(Region::Set, 121, 0, 8, false, true, &[]),
        Field::new(Region::Set, 122, 0, 8, false, true, &[]),
        Field::new(Region::Set, 123, 0, 8, false, true, &[]),
    ];
    pub const LT_POS_Y: [Field; 8] = [
        Field::new(Region::Set, 116, 8, 8, false, true, &[]),
        Field::new(Region::Set, 117, 8, 8, false, true, &[]),
        Field::new(Region::Set, 118, 8, 8, false, true, &[]),
        Field::new(Region::Set, 119, 8, 8, false, true, &[]),
        Field::new(Region::Set, 120, 8, 8, false, true, &[]),
        Field::new(Region::Set, 121, 8, 8, false, true, &[]),
        Field::new(Region::Set, 122, 8, 8, false, true, &[]),
        Field::new(Region::Set, 123, 8, 8, false, true, &[]),
    ];
    pub const RD_POS_X: [Field; 8] = [
        Field::new(Region::Set, 116, 16, 8, false, true, &[]),
        Field::new(Region::Set, 117, 16, 8, false, true, &[]),
        Field::new(Region::Set, 118, 16, 8, false, true, &[]),
        Field::new(Region::Set, 119, 16, 8, false, true, &[]),
        Field::new(Region::Set, 120, 16, 8, false, true, &[]),
        Field::new(Region::Set, 121, 16, 8, false, true, &[]),
        Field::new(Region::Set, 122, 16, 8, false, true, &[]),
        Field::new(Region::Set, 123, 16, 8, false, true, &[]),
    ];
    pub const RD_POS_Y: [Field; 8] = [
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

/// Register `osd_addr` at 0x01f0: 8 words, ReadWrite.
pub mod osd_addr {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register =
        Register::new(Region::Set, "osd_addr", 124, 8, Access::ReadWrite, 1);
    pub const OSD_ADDR: [Field; 8] = [
        Field::new(Region::Set, 124, 0, 32, false, true, &[]),
        Field::new(Region::Set, 125, 0, 32, false, true, &[]),
        Field::new(Region::Set, 126, 0, 32, false, true, &[]),
        Field::new(Region::Set, 127, 0, 32, false, true, &[]),
        Field::new(Region::Set, 128, 0, 32, false, true, &[]),
        Field::new(Region::Set, 129, 0, 32, false, true, &[]),
        Field::new(Region::Set, 130, 0, 32, false, true, &[]),
        Field::new(Region::Set, 131, 0, 32, false, true, &[]),
    ];
}
