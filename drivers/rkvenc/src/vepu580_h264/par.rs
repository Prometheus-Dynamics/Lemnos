//! The VEPU580 H.264 PAR (kernel class 3): the parameter registers (reg_s3).
//!
//! Words 1472 to 1845 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg1472` at 0x1700: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl32_intra_CST_THD0`.
pub mod reg1472 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl32_intra_CST_THD0",
        1472,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL4_INTRA_CST_THD0: Field =
        Field::new(Region::Class(3), 1472, 0, 12, false, true, &[]);
    pub const LVL4_INTRA_CST_THD1: Field =
        Field::new(Region::Class(3), 1472, 16, 12, false, true, &[]);
}

/// Register `reg1473` at 0x1704: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl32_intra_CST_THD1`.
pub mod reg1473 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl32_intra_CST_THD1",
        1473,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL4_INTRA_CST_THD2: Field =
        Field::new(Region::Class(3), 1473, 0, 12, false, true, &[]);
    pub const LVL4_INTRA_CST_THD3: Field =
        Field::new(Region::Class(3), 1473, 16, 12, false, true, &[]);
}

/// Register `reg1474` at 0x1708: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl16_intra_CST_THD0`.
pub mod reg1474 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl16_intra_CST_THD0",
        1474,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CHRM_CST_THD0: Field =
        Field::new(Region::Class(3), 1474, 0, 12, false, true, &[]);
    pub const LVL8_INTRA_CHRM_CST_THD1: Field =
        Field::new(Region::Class(3), 1474, 16, 12, false, true, &[]);
}

/// Register `reg1475` at 0x170c: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl16_intra_CST_THD1`.
pub mod reg1475 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl16_intra_CST_THD1",
        1475,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CHRM_CST_THD2: Field =
        Field::new(Region::Class(3), 1475, 0, 12, false, true, &[]);
    pub const LVL8_INTRA_CHRM_CST_THD3: Field =
        Field::new(Region::Class(3), 1475, 16, 12, false, true, &[]);
}

/// Register `reg1476` at 0x1710: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl8_intra_CST_THD0`.
pub mod reg1476 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl8_intra_CST_THD0",
        1476,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CST_THD0: Field =
        Field::new(Region::Class(3), 1476, 0, 12, false, true, &[]);
    pub const LVL8_INTRA_CST_THD1: Field =
        Field::new(Region::Class(3), 1476, 16, 12, false, true, &[]);
}

/// Register `reg1477` at 0x1714: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl8_intra_CST_THD1`.
pub mod reg1477 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl8_intra_CST_THD1",
        1477,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CST_THD2: Field =
        Field::new(Region::Class(3), 1477, 0, 12, false, true, &[]);
    pub const LVL8_INTRA_CST_THD3: Field =
        Field::new(Region::Class(3), 1477, 16, 12, false, true, &[]);
}

/// Register `reg1478` at 0x1718: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl16_intra_UL_CST_THD`.
pub mod reg1478 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl16_intra_UL_CST_THD",
        1478,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_UL_CST_THLD: Field =
        Field::new(Region::Class(3), 1478, 0, 12, false, true, &[]);
}

/// Register `reg1479` at 0x171c: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl32_intra_CST_WGT0`.
pub mod reg1479 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl32_intra_CST_WGT0",
        1479,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CST_WGT0: Field =
        Field::new(Region::Class(3), 1479, 0, 8, false, true, &[]);
    pub const LVL8_INTRA_CST_WGT1: Field =
        Field::new(Region::Class(3), 1479, 8, 8, false, true, &[]);
    pub const LVL8_INTRA_CST_WGT2: Field =
        Field::new(Region::Class(3), 1479, 16, 8, false, true, &[]);
    pub const LVL8_INTRA_CST_WGT3: Field =
        Field::new(Region::Class(3), 1479, 24, 8, false, true, &[]);
}

/// Register `reg1480` at 0x1720: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl32_intra_CST_WGT1`.
pub mod reg1480 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl32_intra_CST_WGT1",
        1480,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL4_INTRA_CST_WGT0: Field =
        Field::new(Region::Class(3), 1480, 0, 8, false, true, &[]);
    pub const LVL4_INTRA_CST_WGT1: Field =
        Field::new(Region::Class(3), 1480, 8, 8, false, true, &[]);
    pub const LVL4_INTRA_CST_WGT2: Field =
        Field::new(Region::Class(3), 1480, 16, 8, false, true, &[]);
    pub const LVL4_INTRA_CST_WGT3: Field =
        Field::new(Region::Class(3), 1480, 24, 8, false, true, &[]);
}

/// Register `reg1481` at 0x1724: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl16_intra_CST_WGT0`.
pub mod reg1481 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl16_intra_CST_WGT0",
        1481,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_WGT0: Field =
        Field::new(Region::Class(3), 1481, 0, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT1: Field =
        Field::new(Region::Class(3), 1481, 8, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT2: Field =
        Field::new(Region::Class(3), 1481, 16, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT3: Field =
        Field::new(Region::Class(3), 1481, 24, 8, false, true, &[]);
}

/// Register `reg1482` at 0x1728: 1 word, ReadWrite.
///
/// The C path is `reg_s3.lvl16_intra_CST_WGT1`.
pub mod reg1482 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.lvl16_intra_CST_WGT1",
        1482,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_INTRA_CHRM_CST_WGT0: Field =
        Field::new(Region::Class(3), 1482, 0, 8, false, true, &[]);
    pub const LVL8_INTRA_CHRM_CST_WGT1: Field =
        Field::new(Region::Class(3), 1482, 8, 8, false, true, &[]);
    pub const LVL8_INTRA_CHRM_CST_WGT2: Field =
        Field::new(Region::Class(3), 1482, 16, 8, false, true, &[]);
    pub const LVL8_INTRA_CHRM_CST_WGT3: Field =
        Field::new(Region::Class(3), 1482, 24, 8, false, true, &[]);
}

/// Register `reg1484` at 0x1730: 1 word, ReadWrite.
///
/// The C path is `reg_s3.RDO_QUANT`.
pub mod reg1484 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.RDO_QUANT",
        1484,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QUANT_F_BIAS_I: Field = Field::new(Region::Class(3), 1484, 0, 10, false, true, &[]);
    pub const QUANT_F_BIAS_P: Field = Field::new(Region::Class(3), 1484, 10, 10, false, true, &[]);
}

/// Register `reg1488` at 0x1740: 1 word, ReadWrite.
///
/// The C path is `reg_s3.ATR_THD0`.
pub mod reg1488 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.ATR_THD0",
        1488,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATR_THD0: Field = Field::new(Region::Class(3), 1488, 0, 12, false, true, &[]);
    pub const ATR_THD1: Field = Field::new(Region::Class(3), 1488, 16, 12, false, true, &[]);
}

/// Register `reg1489` at 0x1744: 1 word, ReadWrite.
///
/// The C path is `reg_s3.ATR_THD1`.
pub mod reg1489 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.ATR_THD1",
        1489,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATR_THD2: Field = Field::new(Region::Class(3), 1489, 0, 12, false, true, &[]);
    pub const ATR_THDQP: Field = Field::new(Region::Class(3), 1489, 16, 6, false, true, &[]);
}

/// Register `reg1490` at 0x1748: 1 word, ReadWrite.
///
/// The C path is `reg_s3.ATR_THD10`.
pub mod reg1490 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.ATR_THD10",
        1490,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATR1_THD0: Field = Field::new(Region::Class(3), 1490, 0, 12, false, true, &[]);
    pub const ATR1_THD1: Field = Field::new(Region::Class(3), 1490, 16, 12, false, true, &[]);
}

/// Register `reg1491` at 0x174c: 1 word, ReadWrite.
///
/// The C path is `reg_s3.ATR_THD11`.
pub mod reg1491 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.ATR_THD11",
        1491,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ATR1_THD2: Field = Field::new(Region::Class(3), 1491, 0, 12, false, true, &[]);
}

/// Register `reg1492` at 0x1750: 1 word, ReadWrite.
///
/// The C path is `reg_s3.Lvl16_ATR_WGT`.
pub mod reg1492 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.Lvl16_ATR_WGT",
        1492,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_ATR_WGT0: Field = Field::new(Region::Class(3), 1492, 0, 8, false, true, &[]);
    pub const LVL16_ATR_WGT1: Field = Field::new(Region::Class(3), 1492, 8, 8, false, true, &[]);
    pub const LVL16_ATR_WGT2: Field = Field::new(Region::Class(3), 1492, 16, 8, false, true, &[]);
}

/// Register `reg1493` at 0x1754: 1 word, ReadWrite.
///
/// The C path is `reg_s3.Lvl8_ATR_WGT`.
pub mod reg1493 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.Lvl8_ATR_WGT",
        1493,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL8_ATR_WGT0: Field = Field::new(Region::Class(3), 1493, 0, 8, false, true, &[]);
    pub const LVL8_ATR_WGT1: Field = Field::new(Region::Class(3), 1493, 8, 8, false, true, &[]);
    pub const LVL8_ATR_WGT2: Field = Field::new(Region::Class(3), 1493, 16, 8, false, true, &[]);
}

/// Register `reg1494` at 0x1758: 1 word, ReadWrite.
///
/// The C path is `reg_s3.Lvl4_ATR_WGT`.
pub mod reg1494 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.Lvl4_ATR_WGT",
        1494,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL4_ATR_WGT0: Field = Field::new(Region::Class(3), 1494, 0, 8, false, true, &[]);
    pub const LVL4_ATR_WGT1: Field = Field::new(Region::Class(3), 1494, 8, 8, false, true, &[]);
    pub const LVL4_ATR_WGT2: Field = Field::new(Region::Class(3), 1494, 16, 8, false, true, &[]);
}

/// Register `reg1496` at 0x1760: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.cime_sqi_cfg`.
pub mod reg1496 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.cime_sqi_cfg",
        1496,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_SAD_MOD_SEL: Field = Field::new(Region::Class(3), 1496, 0, 1, false, true, &[]);
    pub const CIME_SAD_USE_BIG_BLOCK: Field =
        Field::new(Region::Class(3), 1496, 1, 1, false, true, &[]);
    pub const CIME_PMV_SET_ZERO: Field = Field::new(Region::Class(3), 1496, 2, 1, false, true, &[]);
    pub const CIME_PMV_NUM: Field = Field::new(Region::Class(3), 1496, 8, 2, false, true, &[]);
}

/// Register `reg1497` at 0x1764: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.cime_sqi_thd`.
pub mod reg1497 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.cime_sqi_thd",
        1497,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_MVD_TH0: Field = Field::new(Region::Class(3), 1497, 0, 9, false, true, &[]);
    pub const CIME_MVD_TH1: Field = Field::new(Region::Class(3), 1497, 10, 9, false, true, &[]);
    pub const CIME_MVD_TH2: Field = Field::new(Region::Class(3), 1497, 20, 9, false, true, &[]);
}

/// Register `reg1498` at 0x1768: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.cime_sqi_multi0`.
pub mod reg1498 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.cime_sqi_multi0",
        1498,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_MULTI0: Field = Field::new(Region::Class(3), 1498, 0, 10, false, true, &[]);
    pub const CIME_MULTI1: Field = Field::new(Region::Class(3), 1498, 16, 10, false, true, &[]);
}

/// Register `reg1499` at 0x176c: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.cime_sqi_multi1`.
pub mod reg1499 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.cime_sqi_multi1",
        1499,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_MULTI2: Field = Field::new(Region::Class(3), 1499, 0, 10, false, true, &[]);
    pub const CIME_MULTI3: Field = Field::new(Region::Class(3), 1499, 16, 10, false, true, &[]);
}

/// Register `reg1500` at 0x1770: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.rime_sqi_thd`.
pub mod reg1500 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.rime_sqi_thd",
        1500,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_SAD_TH0: Field = Field::new(Region::Class(3), 1500, 0, 12, false, true, &[]);
    pub const RIME_MVD_TH0: Field = Field::new(Region::Class(3), 1500, 16, 4, false, true, &[]);
    pub const RIME_MVD_TH1: Field = Field::new(Region::Class(3), 1500, 24, 4, false, true, &[]);
}

/// Register `reg1501` at 0x1774: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.rime_sqi_multi`.
pub mod reg1501 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.rime_sqi_multi",
        1501,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RIME_MULTI0: Field = Field::new(Region::Class(3), 1501, 0, 10, false, true, &[]);
    pub const RIME_MULTI1: Field = Field::new(Region::Class(3), 1501, 10, 10, false, true, &[]);
    pub const RIME_MULTI2: Field = Field::new(Region::Class(3), 1501, 20, 10, false, true, &[]);
}

/// Register `reg1502` at 0x1778: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.fme_sqi_thd0`.
pub mod reg1502 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.fme_sqi_thd0",
        1502,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_SAD_PU16_TH: Field = Field::new(Region::Class(3), 1502, 0, 12, false, true, &[]);
    pub const CIME_SAD_PU32_TH: Field =
        Field::new(Region::Class(3), 1502, 16, 12, false, true, &[]);
}

/// Register `reg1503` at 0x177c: 1 word, ReadWrite.
///
/// The C path is `reg_s3.common.fme_sqi_thd1`.
pub mod reg1503 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.fme_sqi_thd1",
        1503,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CIME_SAD_PU64_TH: Field = Field::new(Region::Class(3), 1503, 0, 12, false, true, &[]);
    pub const MOVE_LAMBDA: Field = Field::new(Region::Class(3), 1503, 16, 4, false, true, &[]);
}

/// Register `reg1536` at 0x1800: 52 words, ReadWrite.
///
/// The C path is `reg_s3.common.iprd_wgt_qp_hevc_0_51`. An array of 52 RK_U32 entries, one per word.
pub mod reg1536 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.iprd_wgt_qp_hevc_0_51",
        1536,
        52,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1600` at 0x1900: 52 words, ReadWrite.
///
/// The C path is `reg_s3.common.rdo_wgta_qp_grpa_0_51`. An array of 52 RK_U32 entries, one per word.
pub mod reg1600 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_s3.common.rdo_wgta_qp_grpa_0_51",
        1600,
        52,
        Access::ReadWrite,
        1,
    );
}
