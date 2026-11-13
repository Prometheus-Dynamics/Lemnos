//! The VEPU580 H.265 PAR (kernel class 3): the weight and parameter registers (reg_wgt).
//!
//! Words 1472 to 1845 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg1472` at 0x1700: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl32_intra_CST_THD0`.
pub mod reg1472 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl32_intra_CST_THD0",
        1472,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_THD0: Field =
        Field::new(Region::Class(3), 1472, 0, 12, false, true, &[]);
    pub const LVL32_INTRA_CST_THD1: Field =
        Field::new(Region::Class(3), 1472, 16, 12, false, true, &[]);
}

/// Register `reg1473` at 0x1704: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl32_intra_CST_THD1`.
pub mod reg1473 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl32_intra_CST_THD1",
        1473,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_THD2: Field =
        Field::new(Region::Class(3), 1473, 0, 12, false, true, &[]);
    pub const LVL32_INTRA_CST_THD3: Field =
        Field::new(Region::Class(3), 1473, 16, 12, false, true, &[]);
}

/// Register `reg1474` at 0x1708: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl16_intra_CST_THD0`.
pub mod reg1474 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl16_intra_CST_THD0",
        1474,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_THD0: Field =
        Field::new(Region::Class(3), 1474, 0, 12, false, true, &[]);
    pub const LVL16_INTRA_CST_THD1: Field =
        Field::new(Region::Class(3), 1474, 16, 12, false, true, &[]);
}

/// Register `reg1475` at 0x170c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl16_intra_CST_THD1`.
pub mod reg1475 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl16_intra_CST_THD1",
        1475,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_THD2: Field =
        Field::new(Region::Class(3), 1475, 0, 12, false, true, &[]);
    pub const LVL16_INTRA_CST_THD3: Field =
        Field::new(Region::Class(3), 1475, 16, 12, false, true, &[]);
}

/// Register `reg1479` at 0x171c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl32_intra_CST_WGT0`.
pub mod reg1479 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl32_intra_CST_WGT0",
        1479,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_WGT0: Field =
        Field::new(Region::Class(3), 1479, 0, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT1: Field =
        Field::new(Region::Class(3), 1479, 8, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT2: Field =
        Field::new(Region::Class(3), 1479, 16, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT3: Field =
        Field::new(Region::Class(3), 1479, 24, 8, false, true, &[]);
}

/// Register `reg1480` at 0x1720: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl32_intra_CST_WGT1`.
pub mod reg1480 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl32_intra_CST_WGT1",
        1480,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL32_INTRA_CST_WGT4: Field =
        Field::new(Region::Class(3), 1480, 0, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT5: Field =
        Field::new(Region::Class(3), 1480, 8, 8, false, true, &[]);
    pub const LVL32_INTRA_CST_WGT6: Field =
        Field::new(Region::Class(3), 1480, 16, 8, false, true, &[]);
}

/// Register `reg1481` at 0x1724: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1481;

/// Register `reg1482` at 0x1728: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.lvl16_intra_CST_WGT1`.
pub mod reg1482 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.lvl16_intra_CST_WGT1",
        1482,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LVL16_INTRA_CST_WGT4: Field =
        Field::new(Region::Class(3), 1482, 0, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT5: Field =
        Field::new(Region::Class(3), 1482, 8, 8, false, true, &[]);
    pub const LVL16_INTRA_CST_WGT6: Field =
        Field::new(Region::Class(3), 1482, 16, 8, false, true, &[]);
}

/// Register `reg1484` at 0x1730: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.reg1484_qnt_bias_comb`.
pub mod reg1484 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.reg1484_qnt_bias_comb",
        1484,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QNT_BIAS_I: Field = Field::new(Region::Class(3), 1484, 0, 10, false, true, &[]);
    pub const QNT_BIAS_P: Field = Field::new(Region::Class(3), 1484, 10, 10, false, true, &[]);
}

/// Register `reg1496` at 0x1760: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1496;

/// Register `reg1497` at 0x1764: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1497;

/// Register `reg1498` at 0x1768: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1498;

/// Register `reg1499` at 0x176c: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1499;

/// Register `reg1500` at 0x1770: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1500;

/// Register `reg1501` at 0x1774: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1501;

/// Register `reg1502` at 0x1778: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1502;

/// Register `reg1503` at 0x177c: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1503;

/// Register `reg1536` at 0x1800: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1536;

/// Register `reg1600` at 0x1900: the same register as in the H.264 layout.
pub use crate::vepu580_h264::par::reg1600;

/// Register `reg1728` at 0x1b00: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla0_B0`.
pub mod reg1728 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla0_B0",
        1728,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA0_M0: Field = Field::new(Region::Class(3), 1728, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M1: Field = Field::new(Region::Class(3), 1728, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M2: Field =
        Field::new(Region::Class(3), 1728, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M3: Field =
        Field::new(Region::Class(3), 1728, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M4: Field =
        Field::new(Region::Class(3), 1728, 24, 6, false, true, &[]);
}

/// Register `reg1729` at 0x1b04: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla0_B1`.
pub mod reg1729 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla0_B1",
        1729,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA0_M5: Field = Field::new(Region::Class(3), 1729, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M6: Field = Field::new(Region::Class(3), 1729, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M7: Field =
        Field::new(Region::Class(3), 1729, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M8: Field =
        Field::new(Region::Class(3), 1729, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA0_M9: Field =
        Field::new(Region::Class(3), 1729, 24, 6, false, true, &[]);
}

/// Register `reg1730` at 0x1b08: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla1_B0`.
pub mod reg1730 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla1_B0",
        1730,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA1_M0: Field = Field::new(Region::Class(3), 1730, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M1: Field = Field::new(Region::Class(3), 1730, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M2: Field =
        Field::new(Region::Class(3), 1730, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M3: Field =
        Field::new(Region::Class(3), 1730, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M4: Field =
        Field::new(Region::Class(3), 1730, 24, 6, false, true, &[]);
}

/// Register `reg1731` at 0x1b0c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla1_B1`.
pub mod reg1731 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla1_B1",
        1731,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA1_M5: Field = Field::new(Region::Class(3), 1731, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M6: Field = Field::new(Region::Class(3), 1731, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M7: Field =
        Field::new(Region::Class(3), 1731, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M8: Field =
        Field::new(Region::Class(3), 1731, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA1_M9: Field =
        Field::new(Region::Class(3), 1731, 24, 6, false, true, &[]);
}

/// Register `reg1732` at 0x1b10: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla2_B0`.
pub mod reg1732 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla2_B0",
        1732,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA2_M0: Field = Field::new(Region::Class(3), 1732, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M1: Field = Field::new(Region::Class(3), 1732, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M2: Field =
        Field::new(Region::Class(3), 1732, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M3: Field =
        Field::new(Region::Class(3), 1732, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M4: Field =
        Field::new(Region::Class(3), 1732, 24, 6, false, true, &[]);
}

/// Register `reg1733` at 0x1b14: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla2_B1`.
pub mod reg1733 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla2_B1",
        1733,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA2_M5: Field = Field::new(Region::Class(3), 1733, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M6: Field = Field::new(Region::Class(3), 1733, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M7: Field =
        Field::new(Region::Class(3), 1733, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M8: Field =
        Field::new(Region::Class(3), 1733, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA2_M9: Field =
        Field::new(Region::Class(3), 1733, 24, 6, false, true, &[]);
}

/// Register `reg1734` at 0x1b18: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla3_B0`.
pub mod reg1734 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla3_B0",
        1734,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA3_M0: Field = Field::new(Region::Class(3), 1734, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M1: Field = Field::new(Region::Class(3), 1734, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M2: Field =
        Field::new(Region::Class(3), 1734, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M3: Field =
        Field::new(Region::Class(3), 1734, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M4: Field =
        Field::new(Region::Class(3), 1734, 24, 6, false, true, &[]);
}

/// Register `reg1735` at 0x1b1c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla3_B1`.
pub mod reg1735 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla3_B1",
        1735,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA3_M5: Field = Field::new(Region::Class(3), 1735, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M6: Field = Field::new(Region::Class(3), 1735, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M7: Field =
        Field::new(Region::Class(3), 1735, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M8: Field =
        Field::new(Region::Class(3), 1735, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA3_M9: Field =
        Field::new(Region::Class(3), 1735, 24, 6, false, true, &[]);
}

/// Register `reg1736` at 0x1b20: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla4_B0`.
pub mod reg1736 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla4_B0",
        1736,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA4_M0: Field = Field::new(Region::Class(3), 1736, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M1: Field = Field::new(Region::Class(3), 1736, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M2: Field =
        Field::new(Region::Class(3), 1736, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M3: Field =
        Field::new(Region::Class(3), 1736, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M4: Field =
        Field::new(Region::Class(3), 1736, 24, 6, false, true, &[]);
}

/// Register `reg1737` at 0x1b24: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla4_B1`.
pub mod reg1737 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla4_B1",
        1737,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA4_M5: Field = Field::new(Region::Class(3), 1737, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M6: Field = Field::new(Region::Class(3), 1737, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M7: Field =
        Field::new(Region::Class(3), 1737, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M8: Field =
        Field::new(Region::Class(3), 1737, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA4_M9: Field =
        Field::new(Region::Class(3), 1737, 24, 6, false, true, &[]);
}

/// Register `reg1738` at 0x1b28: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla5_B0`.
pub mod reg1738 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla5_B0",
        1738,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA5_M0: Field = Field::new(Region::Class(3), 1738, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M1: Field = Field::new(Region::Class(3), 1738, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M2: Field =
        Field::new(Region::Class(3), 1738, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M3: Field =
        Field::new(Region::Class(3), 1738, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M4: Field =
        Field::new(Region::Class(3), 1738, 24, 6, false, true, &[]);
}

/// Register `reg1739` at 0x1b2c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla5_B1`.
pub mod reg1739 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla5_B1",
        1739,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA5_M5: Field = Field::new(Region::Class(3), 1739, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M6: Field = Field::new(Region::Class(3), 1739, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M7: Field =
        Field::new(Region::Class(3), 1739, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M8: Field =
        Field::new(Region::Class(3), 1739, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA5_M9: Field =
        Field::new(Region::Class(3), 1739, 24, 6, false, true, &[]);
}

/// Register `reg1740` at 0x1b30: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla6_B0`.
pub mod reg1740 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla6_B0",
        1740,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA6_M0: Field = Field::new(Region::Class(3), 1740, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M1: Field = Field::new(Region::Class(3), 1740, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M2: Field =
        Field::new(Region::Class(3), 1740, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M3: Field =
        Field::new(Region::Class(3), 1740, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M4: Field =
        Field::new(Region::Class(3), 1740, 24, 6, false, true, &[]);
}

/// Register `reg1741` at 0x1b34: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla6_B1`.
pub mod reg1741 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla6_B1",
        1741,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA6_M5: Field = Field::new(Region::Class(3), 1741, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M6: Field = Field::new(Region::Class(3), 1741, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M7: Field =
        Field::new(Region::Class(3), 1741, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M8: Field =
        Field::new(Region::Class(3), 1741, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA6_M9: Field =
        Field::new(Region::Class(3), 1741, 24, 6, false, true, &[]);
}

/// Register `reg1742` at 0x1b38: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla7_B0`.
pub mod reg1742 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla7_B0",
        1742,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA7_M0: Field = Field::new(Region::Class(3), 1742, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M1: Field = Field::new(Region::Class(3), 1742, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M2: Field =
        Field::new(Region::Class(3), 1742, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M3: Field =
        Field::new(Region::Class(3), 1742, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M4: Field =
        Field::new(Region::Class(3), 1742, 24, 6, false, true, &[]);
}

/// Register `reg1743` at 0x1b3c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla7_B1`.
pub mod reg1743 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla7_B1",
        1743,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA7_M5: Field = Field::new(Region::Class(3), 1743, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M6: Field = Field::new(Region::Class(3), 1743, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M7: Field =
        Field::new(Region::Class(3), 1743, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M8: Field =
        Field::new(Region::Class(3), 1743, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA7_M9: Field =
        Field::new(Region::Class(3), 1743, 24, 6, false, true, &[]);
}

/// Register `reg1744` at 0x1b40: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla8_B0`.
pub mod reg1744 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla8_B0",
        1744,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA8_M0: Field = Field::new(Region::Class(3), 1744, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M1: Field = Field::new(Region::Class(3), 1744, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M2: Field =
        Field::new(Region::Class(3), 1744, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M3: Field =
        Field::new(Region::Class(3), 1744, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M4: Field =
        Field::new(Region::Class(3), 1744, 24, 6, false, true, &[]);
}

/// Register `reg1745` at 0x1b44: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla8_B1`.
pub mod reg1745 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla8_B1",
        1745,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA8_M5: Field = Field::new(Region::Class(3), 1745, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M6: Field = Field::new(Region::Class(3), 1745, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M7: Field =
        Field::new(Region::Class(3), 1745, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M8: Field =
        Field::new(Region::Class(3), 1745, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA8_M9: Field =
        Field::new(Region::Class(3), 1745, 24, 6, false, true, &[]);
}

/// Register `reg1746` at 0x1b48: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla9_B0`.
pub mod reg1746 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla9_B0",
        1746,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA9_M0: Field = Field::new(Region::Class(3), 1746, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M1: Field = Field::new(Region::Class(3), 1746, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M2: Field =
        Field::new(Region::Class(3), 1746, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M3: Field =
        Field::new(Region::Class(3), 1746, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M4: Field =
        Field::new(Region::Class(3), 1746, 24, 6, false, true, &[]);
}

/// Register `reg1747` at 0x1b4c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla9_B1`.
pub mod reg1747 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla9_B1",
        1747,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA9_M5: Field = Field::new(Region::Class(3), 1747, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M6: Field = Field::new(Region::Class(3), 1747, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M7: Field =
        Field::new(Region::Class(3), 1747, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M8: Field =
        Field::new(Region::Class(3), 1747, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA9_M9: Field =
        Field::new(Region::Class(3), 1747, 24, 6, false, true, &[]);
}

/// Register `reg1748` at 0x1b50: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla10_B0`.
pub mod reg1748 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla10_B0",
        1748,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA10_M0: Field =
        Field::new(Region::Class(3), 1748, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M1: Field =
        Field::new(Region::Class(3), 1748, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M2: Field =
        Field::new(Region::Class(3), 1748, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M3: Field =
        Field::new(Region::Class(3), 1748, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M4: Field =
        Field::new(Region::Class(3), 1748, 24, 6, false, true, &[]);
}

/// Register `reg1749` at 0x1b54: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla10_B1`.
pub mod reg1749 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla10_B1",
        1749,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA10_M5: Field =
        Field::new(Region::Class(3), 1749, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M6: Field =
        Field::new(Region::Class(3), 1749, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M7: Field =
        Field::new(Region::Class(3), 1749, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M8: Field =
        Field::new(Region::Class(3), 1749, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA10_M9: Field =
        Field::new(Region::Class(3), 1749, 24, 6, false, true, &[]);
}

/// Register `reg1750` at 0x1b58: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla11_B0`.
pub mod reg1750 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla11_B0",
        1750,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA11_M0: Field =
        Field::new(Region::Class(3), 1750, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M1: Field =
        Field::new(Region::Class(3), 1750, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M2: Field =
        Field::new(Region::Class(3), 1750, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M3: Field =
        Field::new(Region::Class(3), 1750, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M4: Field =
        Field::new(Region::Class(3), 1750, 24, 6, false, true, &[]);
}

/// Register `reg1751` at 0x1b5c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla11_B1`.
pub mod reg1751 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla11_B1",
        1751,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA11_M5: Field =
        Field::new(Region::Class(3), 1751, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M6: Field =
        Field::new(Region::Class(3), 1751, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M7: Field =
        Field::new(Region::Class(3), 1751, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M8: Field =
        Field::new(Region::Class(3), 1751, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA11_M9: Field =
        Field::new(Region::Class(3), 1751, 24, 6, false, true, &[]);
}

/// Register `reg1752` at 0x1b60: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla12_B0`.
pub mod reg1752 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla12_B0",
        1752,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA12_M0: Field =
        Field::new(Region::Class(3), 1752, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M1: Field =
        Field::new(Region::Class(3), 1752, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M2: Field =
        Field::new(Region::Class(3), 1752, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M3: Field =
        Field::new(Region::Class(3), 1752, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M4: Field =
        Field::new(Region::Class(3), 1752, 24, 6, false, true, &[]);
}

/// Register `reg1753` at 0x1b64: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla12_B1`.
pub mod reg1753 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla12_B1",
        1753,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA12_M5: Field =
        Field::new(Region::Class(3), 1753, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M6: Field =
        Field::new(Region::Class(3), 1753, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M7: Field =
        Field::new(Region::Class(3), 1753, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M8: Field =
        Field::new(Region::Class(3), 1753, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA12_M9: Field =
        Field::new(Region::Class(3), 1753, 24, 6, false, true, &[]);
}

/// Register `reg1754` at 0x1b68: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla13_B0`.
pub mod reg1754 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla13_B0",
        1754,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA13_M0: Field =
        Field::new(Region::Class(3), 1754, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M1: Field =
        Field::new(Region::Class(3), 1754, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M2: Field =
        Field::new(Region::Class(3), 1754, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M3: Field =
        Field::new(Region::Class(3), 1754, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M4: Field =
        Field::new(Region::Class(3), 1754, 24, 6, false, true, &[]);
}

/// Register `reg1755` at 0x1b6c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla13_B1`.
pub mod reg1755 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla13_B1",
        1755,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA13_M5: Field =
        Field::new(Region::Class(3), 1755, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M6: Field =
        Field::new(Region::Class(3), 1755, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M7: Field =
        Field::new(Region::Class(3), 1755, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M8: Field =
        Field::new(Region::Class(3), 1755, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA13_M9: Field =
        Field::new(Region::Class(3), 1755, 24, 6, false, true, &[]);
}

/// Register `reg1756` at 0x1b70: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla14_B0`.
pub mod reg1756 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla14_B0",
        1756,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA14_M0: Field =
        Field::new(Region::Class(3), 1756, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M1: Field =
        Field::new(Region::Class(3), 1756, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M2: Field =
        Field::new(Region::Class(3), 1756, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M3: Field =
        Field::new(Region::Class(3), 1756, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M4: Field =
        Field::new(Region::Class(3), 1756, 24, 6, false, true, &[]);
}

/// Register `reg1757` at 0x1b74: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla14_B1`.
pub mod reg1757 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla14_B1",
        1757,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA14_M5: Field =
        Field::new(Region::Class(3), 1757, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M6: Field =
        Field::new(Region::Class(3), 1757, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M7: Field =
        Field::new(Region::Class(3), 1757, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M8: Field =
        Field::new(Region::Class(3), 1757, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA14_M9: Field =
        Field::new(Region::Class(3), 1757, 24, 6, false, true, &[]);
}

/// Register `reg1758` at 0x1b78: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla15_B0`.
pub mod reg1758 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla15_B0",
        1758,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA15_M0: Field =
        Field::new(Region::Class(3), 1758, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M1: Field =
        Field::new(Region::Class(3), 1758, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M2: Field =
        Field::new(Region::Class(3), 1758, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M3: Field =
        Field::new(Region::Class(3), 1758, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M4: Field =
        Field::new(Region::Class(3), 1758, 24, 6, false, true, &[]);
}

/// Register `reg1759` at 0x1b7c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla15_B1`.
pub mod reg1759 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla15_B1",
        1759,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA15_M5: Field =
        Field::new(Region::Class(3), 1759, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M6: Field =
        Field::new(Region::Class(3), 1759, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M7: Field =
        Field::new(Region::Class(3), 1759, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M8: Field =
        Field::new(Region::Class(3), 1759, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA15_M9: Field =
        Field::new(Region::Class(3), 1759, 24, 6, false, true, &[]);
}

/// Register `reg1760` at 0x1b80: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla16_B0`.
pub mod reg1760 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla16_B0",
        1760,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA16_M0: Field =
        Field::new(Region::Class(3), 1760, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M1: Field =
        Field::new(Region::Class(3), 1760, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M2: Field =
        Field::new(Region::Class(3), 1760, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M3: Field =
        Field::new(Region::Class(3), 1760, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M4: Field =
        Field::new(Region::Class(3), 1760, 24, 6, false, true, &[]);
}

/// Register `reg1761` at 0x1b84: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.pre_intra_cla16_B1`.
pub mod reg1761 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.pre_intra_cla16_B1",
        1761,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PRE_INTRA_CLA16_M5: Field =
        Field::new(Region::Class(3), 1761, 0, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M6: Field =
        Field::new(Region::Class(3), 1761, 6, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M7: Field =
        Field::new(Region::Class(3), 1761, 12, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M8: Field =
        Field::new(Region::Class(3), 1761, 18, 6, false, true, &[]);
    pub const PRE_INTRA_CLA16_M9: Field =
        Field::new(Region::Class(3), 1761, 24, 6, false, true, &[]);
}

/// Register `reg1792` at 0x1c00: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_t`.
pub mod reg1792 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_t",
        1792,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_T0: Field =
        Field::new(Region::Class(3), 1792, 0, 12, false, true, &[]);
    pub const INTRA_L16_SOBEL_T1: Field =
        Field::new(Region::Class(3), 1792, 16, 12, false, true, &[]);
}

/// Register `reg1793` at 0x1c04: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_a_00`.
pub mod reg1793 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_a_00",
        1793,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_A0_QP0: Field =
        Field::new(Region::Class(3), 1793, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP1: Field =
        Field::new(Region::Class(3), 1793, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP2: Field =
        Field::new(Region::Class(3), 1793, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP3: Field =
        Field::new(Region::Class(3), 1793, 18, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP4: Field =
        Field::new(Region::Class(3), 1793, 24, 6, false, true, &[]);
}

/// Register `reg1794` at 0x1c08: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_a_01`.
pub mod reg1794 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_a_01",
        1794,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_A0_QP5: Field =
        Field::new(Region::Class(3), 1794, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP6: Field =
        Field::new(Region::Class(3), 1794, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP7: Field =
        Field::new(Region::Class(3), 1794, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_A0_QP8: Field =
        Field::new(Region::Class(3), 1794, 18, 6, false, true, &[]);
}

/// Register `reg1795` at 0x1c0c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_b_00`.
pub mod reg1795 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_b_00",
        1795,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP0: Field =
        Field::new(Region::Class(3), 1795, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP1: Field =
        Field::new(Region::Class(3), 1795, 16, 15, false, true, &[]);
}

/// Register `reg1796` at 0x1c10: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_b_01`.
pub mod reg1796 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_b_01",
        1796,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP2: Field =
        Field::new(Region::Class(3), 1796, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP3: Field =
        Field::new(Region::Class(3), 1796, 16, 15, false, true, &[]);
}

/// Register `reg1797` at 0x1c14: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_b_02`.
pub mod reg1797 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_b_02",
        1797,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP4: Field =
        Field::new(Region::Class(3), 1797, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP5: Field =
        Field::new(Region::Class(3), 1797, 16, 15, false, true, &[]);
}

/// Register `reg1798` at 0x1c18: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_b_03`.
pub mod reg1798 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_b_03",
        1798,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP6: Field =
        Field::new(Region::Class(3), 1798, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_B0_QP7: Field =
        Field::new(Region::Class(3), 1798, 16, 15, false, true, &[]);
}

/// Register `reg1799` at 0x1c1c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_b_04`.
pub mod reg1799 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_b_04",
        1799,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_B0_QP8: Field =
        Field::new(Region::Class(3), 1799, 0, 15, false, true, &[]);
}

/// Register `reg1800` at 0x1c20: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_c_00`.
pub mod reg1800 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_c_00",
        1800,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_C0_QP0: Field =
        Field::new(Region::Class(3), 1800, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP1: Field =
        Field::new(Region::Class(3), 1800, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP2: Field =
        Field::new(Region::Class(3), 1800, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP3: Field =
        Field::new(Region::Class(3), 1800, 18, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP4: Field =
        Field::new(Region::Class(3), 1800, 24, 6, false, true, &[]);
}

/// Register `reg1801` at 0x1c24: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_c_01`.
pub mod reg1801 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_c_01",
        1801,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_C0_QP5: Field =
        Field::new(Region::Class(3), 1801, 0, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP6: Field =
        Field::new(Region::Class(3), 1801, 6, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP7: Field =
        Field::new(Region::Class(3), 1801, 12, 6, false, true, &[]);
    pub const INTRA_L16_SOBEL_C0_QP8: Field =
        Field::new(Region::Class(3), 1801, 18, 6, false, true, &[]);
}

/// Register `reg1802` at 0x1c28: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_d_00`.
pub mod reg1802 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_d_00",
        1802,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP0: Field =
        Field::new(Region::Class(3), 1802, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP1: Field =
        Field::new(Region::Class(3), 1802, 16, 15, false, true, &[]);
}

/// Register `reg1803` at 0x1c2c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_d_01`.
pub mod reg1803 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_d_01",
        1803,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP2: Field =
        Field::new(Region::Class(3), 1803, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP3: Field =
        Field::new(Region::Class(3), 1803, 16, 15, false, true, &[]);
}

/// Register `reg1804` at 0x1c30: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_d_02`.
pub mod reg1804 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_d_02",
        1804,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP4: Field =
        Field::new(Region::Class(3), 1804, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP5: Field =
        Field::new(Region::Class(3), 1804, 16, 15, false, true, &[]);
}

/// Register `reg1805` at 0x1c34: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_d_03`.
pub mod reg1805 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_d_03",
        1805,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP6: Field =
        Field::new(Region::Class(3), 1805, 0, 15, false, true, &[]);
    pub const INTRA_L16_SOBEL_D0_QP7: Field =
        Field::new(Region::Class(3), 1805, 16, 15, false, true, &[]);
}

/// Register `reg1806` at 0x1c38: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_d_04`.
pub mod reg1806 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_d_04",
        1806,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_D0_QP8: Field =
        Field::new(Region::Class(3), 1806, 0, 15, false, true, &[]);
}

/// Register `reg1807` at 0x1c3c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp0_low`.
pub mod reg1807 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp0_low",
        1807,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1808` at 0x1c40: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_01`.
pub mod reg1808 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_01",
        1808,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP0_HIGH: Field =
        Field::new(Region::Class(3), 1808, 0, 2, false, true, &[]);
}

/// Register `reg1809` at 0x1c44: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp1_low`.
pub mod reg1809 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp1_low",
        1809,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1810` at 0x1c48: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_03`.
pub mod reg1810 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_03",
        1810,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP1_HIGH: Field =
        Field::new(Region::Class(3), 1810, 0, 2, false, true, &[]);
}

/// Register `reg1811` at 0x1c4c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp2_low`.
pub mod reg1811 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp2_low",
        1811,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1812` at 0x1c50: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_05`.
pub mod reg1812 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_05",
        1812,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP2_HIGH: Field =
        Field::new(Region::Class(3), 1812, 0, 2, false, true, &[]);
}

/// Register `reg1813` at 0x1c54: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp3_low`.
pub mod reg1813 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp3_low",
        1813,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1814` at 0x1c58: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_07`.
pub mod reg1814 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_07",
        1814,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP3_HIGH: Field =
        Field::new(Region::Class(3), 1814, 0, 2, false, true, &[]);
}

/// Register `reg1815` at 0x1c5c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp4_low`.
pub mod reg1815 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp4_low",
        1815,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1816` at 0x1c60: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_09`.
pub mod reg1816 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_09",
        1816,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP4_HIGH: Field =
        Field::new(Region::Class(3), 1816, 0, 2, false, true, &[]);
}

/// Register `reg1817` at 0x1c64: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp5_low`.
pub mod reg1817 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp5_low",
        1817,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1818` at 0x1c68: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_11`.
pub mod reg1818 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_11",
        1818,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP5_HIGH: Field =
        Field::new(Region::Class(3), 1818, 0, 2, false, true, &[]);
}

/// Register `reg1819` at 0x1c6c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp6_low`.
pub mod reg1819 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp6_low",
        1819,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1820` at 0x1c70: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_13`.
pub mod reg1820 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_13",
        1820,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP6_HIGH: Field =
        Field::new(Region::Class(3), 1820, 0, 2, false, true, &[]);
}

/// Register `reg1821` at 0x1c74: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp7_low`.
pub mod reg1821 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp7_low",
        1821,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1822` at 0x1c78: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_15`.
pub mod reg1822 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_15",
        1822,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP7_HIGH: Field =
        Field::new(Region::Class(3), 1822, 0, 2, false, true, &[]);
}

/// Register `reg1823` at 0x1c7c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l16_sobel_e0_qp8_low`.
pub mod reg1823 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l16_sobel_e0_qp8_low",
        1823,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1824` at 0x1c80: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i16_sobel_e_17`.
pub mod reg1824 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i16_sobel_e_17",
        1824,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L16_SOBEL_E0_QP8_HIGH: Field =
        Field::new(Region::Class(3), 1824, 0, 2, false, true, &[]);
}

/// Register `reg1825` at 0x1c84: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_t_00`.
pub mod reg1825 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_t_00",
        1825,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T2: Field =
        Field::new(Region::Class(3), 1825, 0, 12, false, true, &[]);
    pub const INTRA_L32_SOBEL_T3: Field =
        Field::new(Region::Class(3), 1825, 16, 12, false, true, &[]);
}

/// Register `reg1826` at 0x1c88: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_t_01`.
pub mod reg1826 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_t_01",
        1826,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T4: Field =
        Field::new(Region::Class(3), 1826, 0, 6, false, true, &[]);
}

/// Register `reg1827` at 0x1c8c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_t_02`.
pub mod reg1827 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_t_02",
        1827,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_T5: Field =
        Field::new(Region::Class(3), 1827, 0, 12, false, true, &[]);
    pub const INTRA_L32_SOBEL_T6: Field =
        Field::new(Region::Class(3), 1827, 16, 12, false, true, &[]);
}

/// Register `reg1828` at 0x1c90: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_a`.
pub mod reg1828 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_a",
        1828,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_A1_QP0: Field =
        Field::new(Region::Class(3), 1828, 0, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP1: Field =
        Field::new(Region::Class(3), 1828, 6, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP2: Field =
        Field::new(Region::Class(3), 1828, 12, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP3: Field =
        Field::new(Region::Class(3), 1828, 18, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_A1_QP4: Field =
        Field::new(Region::Class(3), 1828, 24, 6, false, true, &[]);
}

/// Register `reg1829` at 0x1c94: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_b_00`.
pub mod reg1829 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_b_00",
        1829,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP0: Field =
        Field::new(Region::Class(3), 1829, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_B1_QP1: Field =
        Field::new(Region::Class(3), 1829, 16, 15, false, true, &[]);
}

/// Register `reg1830` at 0x1c98: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_b_01`.
pub mod reg1830 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_b_01",
        1830,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP2: Field =
        Field::new(Region::Class(3), 1830, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_B1_QP3: Field =
        Field::new(Region::Class(3), 1830, 16, 15, false, true, &[]);
}

/// Register `reg1831` at 0x1c9c: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_b_02`.
pub mod reg1831 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_b_02",
        1831,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_B1_QP4: Field =
        Field::new(Region::Class(3), 1831, 0, 15, false, true, &[]);
}

/// Register `reg1832` at 0x1ca0: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_c`.
pub mod reg1832 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_c",
        1832,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_C1_QP0: Field =
        Field::new(Region::Class(3), 1832, 0, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP1: Field =
        Field::new(Region::Class(3), 1832, 6, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP2: Field =
        Field::new(Region::Class(3), 1832, 12, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP3: Field =
        Field::new(Region::Class(3), 1832, 18, 6, false, true, &[]);
    pub const INTRA_L32_SOBEL_C1_QP4: Field =
        Field::new(Region::Class(3), 1832, 24, 6, false, true, &[]);
}

/// Register `reg1833` at 0x1ca4: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_d_00`.
pub mod reg1833 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_d_00",
        1833,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP0: Field =
        Field::new(Region::Class(3), 1833, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_D1_QP1: Field =
        Field::new(Region::Class(3), 1833, 16, 15, false, true, &[]);
}

/// Register `reg1834` at 0x1ca8: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_d_01`.
pub mod reg1834 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_d_01",
        1834,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP2: Field =
        Field::new(Region::Class(3), 1834, 0, 15, false, true, &[]);
    pub const INTRA_L32_SOBEL_D1_QP3: Field =
        Field::new(Region::Class(3), 1834, 16, 15, false, true, &[]);
}

/// Register `reg1835` at 0x1cac: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_d_02`.
pub mod reg1835 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_d_02",
        1835,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_D1_QP4: Field =
        Field::new(Region::Class(3), 1835, 0, 15, false, true, &[]);
}

/// Register `reg1836` at 0x1cb0: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l32_sobel_e1_qp0_low`.
pub mod reg1836 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l32_sobel_e1_qp0_low",
        1836,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1837` at 0x1cb4: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_e_01`.
pub mod reg1837 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_e_01",
        1837,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_E1_QP0_HIGH: Field =
        Field::new(Region::Class(3), 1837, 0, 9, false, true, &[]);
}

/// Register `reg1838` at 0x1cb8: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l32_sobel_e1_qp1_low`.
pub mod reg1838 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l32_sobel_e1_qp1_low",
        1838,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1839` at 0x1cbc: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_e_03`.
pub mod reg1839 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_e_03",
        1839,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_E1_QP1_HIGH: Field =
        Field::new(Region::Class(3), 1839, 0, 9, false, true, &[]);
}

/// Register `reg1840` at 0x1cc0: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l32_sobel_e1_qp2_low`.
pub mod reg1840 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l32_sobel_e1_qp2_low",
        1840,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1841` at 0x1cc4: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_e_05`.
pub mod reg1841 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_e_05",
        1841,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_E1_QP2_HIGH: Field =
        Field::new(Region::Class(3), 1841, 0, 9, false, true, &[]);
}

/// Register `reg1842` at 0x1cc8: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l32_sobel_e1_qp3_low`.
pub mod reg1842 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l32_sobel_e1_qp3_low",
        1842,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1843` at 0x1ccc: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_e_07`.
pub mod reg1843 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_e_07",
        1843,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_E1_QP3_HIGH: Field =
        Field::new(Region::Class(3), 1843, 0, 9, false, true, &[]);
}

/// Register `reg1844` at 0x1cd0: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.intra_l32_sobel_e1_qp4_low`.
pub mod reg1844 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.intra_l32_sobel_e1_qp4_low",
        1844,
        1,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1845` at 0x1cd4: 1 word, ReadWrite.
///
/// The C path is `reg_wgt.i32_sobel_e_09`.
pub mod reg1845 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(3),
        "reg_wgt.i32_sobel_e_09",
        1845,
        1,
        Access::ReadWrite,
        1,
    );
    pub const INTRA_L32_SOBEL_E1_QP4_HIGH: Field =
        Field::new(Region::Class(3), 1845, 0, 9, false, true, &[]);
}
