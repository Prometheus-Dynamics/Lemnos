//! The VEPU580 H.264 RC (kernel class 2): the rate-control klut registers (reg_rc_klut).
//!
//! Words 1024 to 1080 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg1024` at 0x1000: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.rc_adj0`.
pub mod reg1024 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.rc_adj0",
        1024,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QP_ADJ0: Field = Field::new(Region::Class(2), 1024, 0, 5, false, true, &[]);
    pub const QP_ADJ1: Field = Field::new(Region::Class(2), 1024, 5, 5, false, true, &[]);
    pub const QP_ADJ2: Field = Field::new(Region::Class(2), 1024, 10, 5, false, true, &[]);
    pub const QP_ADJ3: Field = Field::new(Region::Class(2), 1024, 15, 5, false, true, &[]);
    pub const QP_ADJ4: Field = Field::new(Region::Class(2), 1024, 20, 5, false, true, &[]);
}

/// Register `reg1025` at 0x1004: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.rc_adj1`.
pub mod reg1025 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.rc_adj1",
        1025,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QP_ADJ5: Field = Field::new(Region::Class(2), 1025, 0, 5, false, true, &[]);
    pub const QP_ADJ6: Field = Field::new(Region::Class(2), 1025, 5, 5, false, true, &[]);
    pub const QP_ADJ7: Field = Field::new(Region::Class(2), 1025, 10, 5, false, true, &[]);
    pub const QP_ADJ8: Field = Field::new(Region::Class(2), 1025, 15, 5, false, true, &[]);
}

/// Register `reg1026` at 0x1008: 9 words, ReadWrite.
///
/// The C path is `reg_rc_klut.rc_dthd_0_8`. An array of 9 RK_U32 entries, one per word.
pub mod reg1026 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.rc_dthd_0_8",
        1026,
        9,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1036` at 0x1030: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.roi_qthd0`.
pub mod reg1036 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.roi_qthd0",
        1036,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QPMIN_AREA0: Field = Field::new(Region::Class(2), 1036, 0, 6, false, true, &[]);
    pub const QPMAX_AREA0: Field = Field::new(Region::Class(2), 1036, 6, 6, false, true, &[]);
    pub const QPMIN_AREA1: Field = Field::new(Region::Class(2), 1036, 12, 6, false, true, &[]);
    pub const QPMAX_AREA1: Field = Field::new(Region::Class(2), 1036, 18, 6, false, true, &[]);
    pub const QPMIN_AREA2: Field = Field::new(Region::Class(2), 1036, 24, 6, false, true, &[]);
}

/// Register `reg1037` at 0x1034: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.roi_qthd1`.
pub mod reg1037 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.roi_qthd1",
        1037,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QPMAX_AREA2: Field = Field::new(Region::Class(2), 1037, 0, 6, false, true, &[]);
    pub const QPMIN_AREA3: Field = Field::new(Region::Class(2), 1037, 6, 6, false, true, &[]);
    pub const QPMAX_AREA3: Field = Field::new(Region::Class(2), 1037, 12, 6, false, true, &[]);
    pub const QPMIN_AREA4: Field = Field::new(Region::Class(2), 1037, 18, 6, false, true, &[]);
    pub const QPMAX_AREA4: Field = Field::new(Region::Class(2), 1037, 24, 6, false, true, &[]);
}

/// Register `reg1038` at 0x1038: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.roi_qthd2`.
pub mod reg1038 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.roi_qthd2",
        1038,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QPMIN_AREA5: Field = Field::new(Region::Class(2), 1038, 0, 6, false, true, &[]);
    pub const QPMAX_AREA5: Field = Field::new(Region::Class(2), 1038, 6, 6, false, true, &[]);
    pub const QPMIN_AREA6: Field = Field::new(Region::Class(2), 1038, 12, 6, false, true, &[]);
    pub const QPMAX_AREA6: Field = Field::new(Region::Class(2), 1038, 18, 6, false, true, &[]);
    pub const QPMIN_AREA7: Field = Field::new(Region::Class(2), 1038, 24, 6, false, true, &[]);
}

/// Register `reg1039` at 0x103c: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.roi_qthd3`.
pub mod reg1039 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.roi_qthd3",
        1039,
        1,
        Access::ReadWrite,
        1,
    );
    pub const QPMAX_AREA7: Field = Field::new(Region::Class(2), 1039, 0, 6, false, true, &[]);
    pub const QPMAP_MODE: Field = Field::new(Region::Class(2), 1039, 30, 2, false, true, &[]);
}

/// Register `reg1040` at 0x1040: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.madi_cfg`.
pub mod reg1040 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.madi_cfg",
        1040,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MADI_MODE: Field = Field::new(Region::Class(2), 1040, 0, 1, false, true, &[]);
    pub const MADI_THD: Field = Field::new(Region::Class(2), 1040, 16, 8, false, true, &[]);
}

/// Register `reg1041` at 0x1044: 4 words, ReadWrite.
///
/// The C path is `reg_rc_klut.aq_tthd`. Packed: four RK_U8 entries per word, the first entry in the low byte.
pub mod reg1041 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.aq_tthd",
        1041,
        4,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1045` at 0x1054: 4 words, ReadWrite.
///
/// The C path is `reg_rc_klut.aq_step`. Packed: four RK_U8 entries per word, the first entry in the low byte.
pub mod reg1045 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.aq_step",
        1045,
        4,
        Access::ReadWrite,
        1,
    );
}

/// Register `reg1052` at 0x1070: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.md_sad_thd`.
pub mod reg1052 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.md_sad_thd",
        1052,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MD_SAD_THD0: Field = Field::new(Region::Class(2), 1052, 0, 8, false, true, &[]);
    pub const MD_SAD_THD1: Field = Field::new(Region::Class(2), 1052, 8, 8, false, true, &[]);
    pub const MD_SAD_THD2: Field = Field::new(Region::Class(2), 1052, 16, 8, false, true, &[]);
}

/// Register `reg1053` at 0x1074: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.madi_thd`.
pub mod reg1053 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.madi_thd",
        1053,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MADI_THD0: Field = Field::new(Region::Class(2), 1053, 0, 8, false, true, &[]);
    pub const MADI_THD1: Field = Field::new(Region::Class(2), 1053, 8, 8, false, true, &[]);
    pub const MADI_THD2: Field = Field::new(Region::Class(2), 1053, 16, 8, false, true, &[]);
}

/// Register `reg1056` at 0x1080: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_ofst`.
pub mod reg1056 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_ofst",
        1056,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_OFST: Field = Field::new(Region::Class(2), 1056, 0, 3, false, true, &[]);
}

/// Register `reg1057` at 0x1084: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt0`.
pub mod reg1057 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt0",
        1057,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT0: Field = Field::new(Region::Class(2), 1057, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT1_L9: Field =
        Field::new(Region::Class(2), 1057, 23, 9, false, true, &[]);
}

/// Register `reg1058` at 0x1088: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt1`.
pub mod reg1058 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt1",
        1058,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT1_H9: Field = Field::new(Region::Class(2), 1058, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT2: Field = Field::new(Region::Class(2), 1058, 14, 18, false, true, &[]);
}

/// Register `reg1059` at 0x108c: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt2`.
pub mod reg1059 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt2",
        1059,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT3: Field = Field::new(Region::Class(2), 1059, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT4_L9: Field =
        Field::new(Region::Class(2), 1059, 23, 9, false, true, &[]);
}

/// Register `reg1060` at 0x1090: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt3`.
pub mod reg1060 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt3",
        1060,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT4_H9: Field = Field::new(Region::Class(2), 1060, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT5: Field = Field::new(Region::Class(2), 1060, 14, 18, false, true, &[]);
}

/// Register `reg1061` at 0x1094: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt4`.
pub mod reg1061 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt4",
        1061,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT6: Field = Field::new(Region::Class(2), 1061, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT7_L9: Field =
        Field::new(Region::Class(2), 1061, 23, 9, false, true, &[]);
}

/// Register `reg1062` at 0x1098: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt5`.
pub mod reg1062 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt5",
        1062,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT7_H9: Field = Field::new(Region::Class(2), 1062, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT8: Field = Field::new(Region::Class(2), 1062, 14, 18, false, true, &[]);
}

/// Register `reg1063` at 0x109c: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt6`.
pub mod reg1063 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt6",
        1063,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT9: Field = Field::new(Region::Class(2), 1063, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT10_L9: Field =
        Field::new(Region::Class(2), 1063, 23, 9, false, true, &[]);
}

/// Register `reg1064` at 0x10a0: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt7`.
pub mod reg1064 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt7",
        1064,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT10_H9: Field =
        Field::new(Region::Class(2), 1064, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT11: Field = Field::new(Region::Class(2), 1064, 14, 18, false, true, &[]);
}

/// Register `reg1065` at 0x10a4: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt8`.
pub mod reg1065 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt8",
        1065,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT12: Field = Field::new(Region::Class(2), 1065, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT13_L9: Field =
        Field::new(Region::Class(2), 1065, 23, 9, false, true, &[]);
}

/// Register `reg1066` at 0x10a8: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt9`.
pub mod reg1066 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt9",
        1066,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT13_H9: Field =
        Field::new(Region::Class(2), 1066, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT14: Field = Field::new(Region::Class(2), 1066, 14, 18, false, true, &[]);
}

/// Register `reg1067` at 0x10ac: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt10`.
pub mod reg1067 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt10",
        1067,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT15: Field = Field::new(Region::Class(2), 1067, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT16_L9: Field =
        Field::new(Region::Class(2), 1067, 23, 9, false, true, &[]);
}

/// Register `reg1068` at 0x10b0: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt11`.
pub mod reg1068 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt11",
        1068,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT16_H9: Field =
        Field::new(Region::Class(2), 1068, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT17: Field = Field::new(Region::Class(2), 1068, 14, 18, false, true, &[]);
}

/// Register `reg1069` at 0x10b4: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt12`.
pub mod reg1069 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt12",
        1069,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT18: Field = Field::new(Region::Class(2), 1069, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT19_L9: Field =
        Field::new(Region::Class(2), 1069, 23, 9, false, true, &[]);
}

/// Register `reg1070` at 0x10b8: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt13`.
pub mod reg1070 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt13",
        1070,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT19_H9: Field =
        Field::new(Region::Class(2), 1070, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT20: Field = Field::new(Region::Class(2), 1070, 14, 18, false, true, &[]);
}

/// Register `reg1071` at 0x10bc: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt14`.
pub mod reg1071 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt14",
        1071,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT21: Field = Field::new(Region::Class(2), 1071, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT22_L9: Field =
        Field::new(Region::Class(2), 1071, 23, 9, false, true, &[]);
}

/// Register `reg1072` at 0x10c0: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt15`.
pub mod reg1072 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt15",
        1072,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT22_H9: Field =
        Field::new(Region::Class(2), 1072, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT23: Field = Field::new(Region::Class(2), 1072, 14, 18, false, true, &[]);
}

/// Register `reg1073` at 0x10c4: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt16`.
pub mod reg1073 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt16",
        1073,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT24: Field = Field::new(Region::Class(2), 1073, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT25_L9: Field =
        Field::new(Region::Class(2), 1073, 23, 9, false, true, &[]);
}

/// Register `reg1074` at 0x10c8: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt17`.
pub mod reg1074 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt17",
        1074,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT25_H9: Field =
        Field::new(Region::Class(2), 1074, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT26: Field = Field::new(Region::Class(2), 1074, 14, 18, false, true, &[]);
}

/// Register `reg1075` at 0x10cc: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt18`.
pub mod reg1075 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt18",
        1075,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT27: Field = Field::new(Region::Class(2), 1075, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT28_L9: Field =
        Field::new(Region::Class(2), 1075, 23, 9, false, true, &[]);
}

/// Register `reg1076` at 0x10d0: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt19`.
pub mod reg1076 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt19",
        1076,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT28_H9: Field =
        Field::new(Region::Class(2), 1076, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT29: Field = Field::new(Region::Class(2), 1076, 14, 18, false, true, &[]);
}

/// Register `reg1077` at 0x10d4: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt20`.
pub mod reg1077 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt20",
        1077,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT30: Field = Field::new(Region::Class(2), 1077, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT31_L9: Field =
        Field::new(Region::Class(2), 1077, 23, 9, false, true, &[]);
}

/// Register `reg1078` at 0x10d8: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt21`.
pub mod reg1078 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt21",
        1078,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT31_H9: Field =
        Field::new(Region::Class(2), 1078, 0, 9, false, true, &[]);
    pub const CHRM_KLUT_WGT32: Field = Field::new(Region::Class(2), 1078, 14, 18, false, true, &[]);
}

/// Register `reg1079` at 0x10dc: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt22`.
pub mod reg1079 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt22",
        1079,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT33: Field = Field::new(Region::Class(2), 1079, 0, 18, false, true, &[]);
    pub const CHRM_KLUT_WGT34_L9: Field =
        Field::new(Region::Class(2), 1079, 23, 9, false, true, &[]);
}

/// Register `reg1080` at 0x10e0: 1 word, ReadWrite.
///
/// The C path is `reg_rc_klut.klut_wgt23`.
pub mod reg1080 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(2),
        "reg_rc_klut.klut_wgt23",
        1080,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CHRM_KLUT_WGT34_H9: Field =
        Field::new(Region::Class(2), 1080, 0, 9, false, true, &[]);
}
