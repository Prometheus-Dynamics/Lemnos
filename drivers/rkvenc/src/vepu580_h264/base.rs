//! The VEPU580 H.264 BASE (kernel class 0): the control and link registers, reg0 to reg22 (reg_ctl).
//!
//! Words 0 to 22 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg0000` at 0x0000: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.version`.
pub mod reg0000 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.version",
        0,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SUB_VER: Field = Field::new(Region::Class(0), 0, 0, 8, false, true, &[]);
    pub const CAP: Field = Field::new(Region::Class(0), 0, 8, 1, false, true, &[]);
    pub const HEVC_CAP: Field = Field::new(Region::Class(0), 0, 9, 1, false, true, &[]);
    pub const RES_CAP: Field = Field::new(Region::Class(0), 0, 12, 4, false, true, &[]);
    pub const OSD_CAP: Field = Field::new(Region::Class(0), 0, 16, 2, false, true, &[]);
    pub const FILTR_CAP: Field = Field::new(Region::Class(0), 0, 18, 2, false, true, &[]);
    pub const BFRM_CAP: Field = Field::new(Region::Class(0), 0, 20, 1, false, true, &[]);
    pub const FBC_CAP: Field = Field::new(Region::Class(0), 0, 21, 2, false, true, &[]);
    pub const IP_ID: Field = Field::new(Region::Class(0), 0, 24, 8, false, true, &[]);
}

/// Register `reg0004` at 0x0010: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.enc_strt`.
pub mod reg0004 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.enc_strt",
        4,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LKT_NUM: Field = Field::new(Region::Class(0), 4, 0, 8, false, true, &[]);
    pub const VEPU_CMD: Field = Field::new(Region::Class(0), 4, 8, 2, false, true, &[]);
}

/// Register `reg0005` at 0x0014: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.enc_clr`.
pub mod reg0005 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.enc_clr",
        5,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SAFE_CLR: Field = Field::new(Region::Class(0), 5, 0, 1, false, true, &[]);
    pub const FORCE_CLR: Field = Field::new(Region::Class(0), 5, 1, 1, false, true, &[]);
}

/// Register `reg0008` at 0x0020: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.int_en`.
pub mod reg0008 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.int_en",
        8,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ENC_DONE_EN: Field = Field::new(Region::Class(0), 8, 0, 1, false, true, &[]);
    pub const LKT_NODE_DONE_EN: Field = Field::new(Region::Class(0), 8, 1, 1, false, true, &[]);
    pub const SCLR_DONE_EN: Field = Field::new(Region::Class(0), 8, 2, 1, false, true, &[]);
    pub const SLC_DONE_EN: Field = Field::new(Region::Class(0), 8, 3, 1, false, true, &[]);
    pub const BSF_OFLW_EN: Field = Field::new(Region::Class(0), 8, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_EN: Field = Field::new(Region::Class(0), 8, 5, 1, false, true, &[]);
    pub const WBUS_ERR_EN: Field = Field::new(Region::Class(0), 8, 6, 1, false, true, &[]);
    pub const RBUS_ERR_EN: Field = Field::new(Region::Class(0), 8, 7, 1, false, true, &[]);
    pub const WDG_EN: Field = Field::new(Region::Class(0), 8, 8, 1, false, true, &[]);
    pub const LKT_ERR_INT_EN: Field = Field::new(Region::Class(0), 8, 9, 1, false, true, &[]);
}

/// Register `reg0009` at 0x0024: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.int_msk`.
pub mod reg0009 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.int_msk",
        9,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ENC_DONE_MSK: Field = Field::new(Region::Class(0), 9, 0, 1, false, true, &[]);
    pub const LKT_NODE_DONE_MSK: Field = Field::new(Region::Class(0), 9, 1, 1, false, true, &[]);
    pub const SCLR_DONE_MSK: Field = Field::new(Region::Class(0), 9, 2, 1, false, true, &[]);
    pub const SLC_DONE_MSK: Field = Field::new(Region::Class(0), 9, 3, 1, false, true, &[]);
    pub const BSF_OFLW_MSK: Field = Field::new(Region::Class(0), 9, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_MSK: Field = Field::new(Region::Class(0), 9, 5, 1, false, true, &[]);
    pub const WBUS_ERR_MSK: Field = Field::new(Region::Class(0), 9, 6, 1, false, true, &[]);
    pub const RBUS_ERR_MSK: Field = Field::new(Region::Class(0), 9, 7, 1, false, true, &[]);
    pub const WDG_MSK: Field = Field::new(Region::Class(0), 9, 8, 1, false, true, &[]);
    pub const LKT_ERR_MSK: Field = Field::new(Region::Class(0), 9, 9, 1, false, true, &[]);
}

/// Register `reg0010` at 0x0028: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.int_clr`.
pub mod reg0010 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.int_clr",
        10,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ENC_DONE_CLR: Field = Field::new(Region::Class(0), 10, 0, 1, false, true, &[]);
    pub const LKT_NODE_DONE_CLR: Field = Field::new(Region::Class(0), 10, 1, 1, false, true, &[]);
    pub const SCLR_DONE_CLR: Field = Field::new(Region::Class(0), 10, 2, 1, false, true, &[]);
    pub const SLC_DONE_CLR: Field = Field::new(Region::Class(0), 10, 3, 1, false, true, &[]);
    pub const BSF_OFLW_CLR: Field = Field::new(Region::Class(0), 10, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_CLR: Field = Field::new(Region::Class(0), 10, 5, 1, false, true, &[]);
    pub const WBUS_ERR_CLR: Field = Field::new(Region::Class(0), 10, 6, 1, false, true, &[]);
    pub const RBUS_ERR_CLR: Field = Field::new(Region::Class(0), 10, 7, 1, false, true, &[]);
    pub const WDG_CLR: Field = Field::new(Region::Class(0), 10, 8, 1, false, true, &[]);
    pub const LKT_ERR_CLR: Field = Field::new(Region::Class(0), 10, 9, 1, false, true, &[]);
}

/// Register `reg0011` at 0x002c: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.int_sta`.
pub mod reg0011 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.int_sta",
        11,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ENC_DONE_STA: Field = Field::new(Region::Class(0), 11, 0, 1, false, true, &[]);
    pub const LKT_NODE_DONE_STA: Field = Field::new(Region::Class(0), 11, 1, 1, false, true, &[]);
    pub const SCLR_DONE_STA: Field = Field::new(Region::Class(0), 11, 2, 1, false, true, &[]);
    pub const SLC_DONE_STA: Field = Field::new(Region::Class(0), 11, 3, 1, false, true, &[]);
    pub const BSF_OFLW_STA: Field = Field::new(Region::Class(0), 11, 4, 1, false, true, &[]);
    pub const BRSP_OTSD_STA: Field = Field::new(Region::Class(0), 11, 5, 1, false, true, &[]);
    pub const WBUS_ERR_STA: Field = Field::new(Region::Class(0), 11, 6, 1, false, true, &[]);
    pub const RBUS_ERR_STA: Field = Field::new(Region::Class(0), 11, 7, 1, false, true, &[]);
    pub const WDG_STA: Field = Field::new(Region::Class(0), 11, 8, 1, false, true, &[]);
    pub const LKT_ERR_STA: Field = Field::new(Region::Class(0), 11, 9, 1, false, true, &[]);
}

/// Register `reg0012` at 0x0030: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.dtrns_map`.
pub mod reg0012 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.dtrns_map",
        12,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LPFW_BUS_ORDR: Field = Field::new(Region::Class(0), 12, 0, 1, false, true, &[]);
    pub const CMVW_BUS_ORDR: Field = Field::new(Region::Class(0), 12, 1, 1, false, true, &[]);
    pub const DSPW_BUS_ORDR: Field = Field::new(Region::Class(0), 12, 2, 1, false, true, &[]);
    pub const RFPW_BUS_ORDR: Field = Field::new(Region::Class(0), 12, 3, 1, false, true, &[]);
    pub const SRC_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 4, 4, false, true, &[]);
    pub const MEIW_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 8, 4, false, true, &[]);
    pub const BSW_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 12, 3, false, true, &[]);
    pub const LKTR_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 15, 4, false, true, &[]);
    pub const ROIR_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 19, 4, false, true, &[]);
    pub const LKTW_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 23, 4, false, true, &[]);
    pub const AFBC_BSIZE: Field = Field::new(Region::Class(0), 12, 27, 1, false, true, &[]);
    pub const EBUFW_BUS_ORDR: Field = Field::new(Region::Class(0), 12, 28, 1, false, true, &[]);
    pub const REC_NFBC_BUS_EDIN: Field = Field::new(Region::Class(0), 12, 29, 3, false, true, &[]);
}

/// Register `reg0013` at 0x0034: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.dtrns_cfg`.
pub mod reg0013 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.dtrns_cfg",
        13,
        1,
        Access::ReadWrite,
        1,
    );
    pub const DSPR_OTSD: Field = Field::new(Region::Class(0), 13, 7, 1, false, true, &[]);
    pub const AXI_BRSP_CKE: Field = Field::new(Region::Class(0), 13, 16, 8, false, true, &[]);
}

/// Register `reg0014` at 0x0038: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.enc_wdg`.
pub mod reg0014 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.enc_wdg",
        14,
        1,
        Access::ReadWrite,
        1,
    );
    pub const VS_LOAD_THD: Field = Field::new(Region::Class(0), 14, 0, 24, false, true, &[]);
    pub const RFP_LOAD_THD: Field = Field::new(Region::Class(0), 14, 24, 8, false, true, &[]);
}

/// Register `reg0015` at 0x003c: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.qos_cfg`.
pub mod reg0015 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.qos_cfg",
        15,
        1,
        Access::ReadWrite,
        1,
    );
    pub const HURRY_EN: Field = Field::new(Region::Class(0), 15, 0, 1, false, true, &[]);
    pub const HURRY_LOW: Field = Field::new(Region::Class(0), 15, 1, 3, false, true, &[]);
    pub const HURRY_MID: Field = Field::new(Region::Class(0), 15, 4, 3, false, true, &[]);
    pub const HURRY_HIGH: Field = Field::new(Region::Class(0), 15, 7, 3, false, true, &[]);
}

/// Register `reg0016` at 0x0040: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.qos_perd`.
pub mod reg0016 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.qos_perd",
        16,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QOS_PERIOD: Field = Field::new(Region::Class(0), 16, 0, 16, false, true, &[]);
}

/// Register `reg0017` at 0x0044: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.hurry_thd_low`.
pub mod reg0017 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.hurry_thd_low",
        17,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg0018` at 0x0048: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.hurry_thd_mid`.
pub mod reg0018 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.hurry_thd_mid",
        18,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg0019` at 0x004c: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.hurry_thd_high`.
pub mod reg0019 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.hurry_thd_high",
        19,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg0020` at 0x0050: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.enc_idle_en`.
pub mod reg0020 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.enc_idle_en",
        20,
        1,
        Access::ReadWrite,
        1,
    );
    pub const IDLE_EN_CORE: Field = Field::new(Region::Class(0), 20, 0, 1, false, true, &[]);
    pub const IDLE_EN_AXI: Field = Field::new(Region::Class(0), 20, 1, 1, false, true, &[]);
    pub const IDLE_EN_AHB: Field = Field::new(Region::Class(0), 20, 2, 1, false, true, &[]);
}

/// Register `reg0021` at 0x0054: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.common.func_en`.
pub mod reg0021 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.common.func_en",
        21,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CKE: Field = Field::new(Region::Class(0), 21, 0, 1, false, true, &[]);
    pub const RESETN_HW_EN: Field = Field::new(Region::Class(0), 21, 1, 1, false, true, &[]);
    pub const ENC_DONE_TMVP_EN: Field = Field::new(Region::Class(0), 21, 2, 1, false, true, &[]);
    pub const SRAM_CKG_EN: Field = Field::new(Region::Class(0), 21, 3, 1, false, true, &[]);
}

/// Register `reg0022` at 0x0058: 1 word, ReadWrite.
///
/// The C path is `reg_ctl.rdo_ckg`.
pub mod reg0022 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(0),
        "reg_ctl.rdo_ckg",
        22,
        1,
        Access::ReadWrite,
        1,
    );
    pub const TQ8_CKG: Field = Field::new(Region::Class(0), 22, 0, 1, false, true, &[]);
    pub const TQ4_CKG: Field = Field::new(Region::Class(0), 22, 1, 1, false, true, &[]);
    pub const BITS_CKG_8X8: Field = Field::new(Region::Class(0), 22, 2, 1, false, true, &[]);
    pub const BITS_CKG_4X4_1: Field = Field::new(Region::Class(0), 22, 3, 1, false, true, &[]);
    pub const BITS_CKG_4X4_0: Field = Field::new(Region::Class(0), 22, 4, 1, false, true, &[]);
    pub const INTER_MODE_CKG: Field = Field::new(Region::Class(0), 22, 5, 1, false, true, &[]);
    pub const INTER_CTRL_CKG: Field = Field::new(Region::Class(0), 22, 6, 1, false, true, &[]);
    pub const INTER_PRED_CKG: Field = Field::new(Region::Class(0), 22, 7, 1, false, true, &[]);
    pub const INTRA8_CKG: Field = Field::new(Region::Class(0), 22, 8, 1, false, true, &[]);
    pub const INTRA4_CKG: Field = Field::new(Region::Class(0), 22, 9, 1, false, true, &[]);
}
