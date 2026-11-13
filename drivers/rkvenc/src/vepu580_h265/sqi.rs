//! The VEPU580 H.265 SQI (kernel class 4): the RDO and scaling-list registers (reg_rdo).
//!
//! Words 2048 to 2169 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg2048` at 0x2000: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_sqi_cfg`.
pub mod reg2048 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_sqi_cfg",
        2048,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RDO_SEGMENT_EN: Field = Field::new(Region::Class(4), 2048, 0, 1, false, true, &[]);
    pub const RDO_SMEAR_EN: Field = Field::new(Region::Class(4), 2048, 1, 1, false, true, &[]);
}

/// Register `reg2049` at 0x2004: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_cime_thd0`.
pub mod reg2049 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_cime_thd0",
        2049,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2049, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2049, 16, 12, false, true, &[]);
}

/// Register `reg2050` at 0x2008: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_cime_thd1`.
pub mod reg2050 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_cime_thd1",
        2050,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2050, 0, 12, false, true, &[]);
}

/// Register `reg2051` at 0x200c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd0`.
pub mod reg2051 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd0",
        2051,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2051, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2051, 16, 12, false, true, &[]);
}

/// Register `reg2052` at 0x2010: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd1`.
pub mod reg2052 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd1",
        2052,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2052, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2052, 16, 12, false, true, &[]);
}

/// Register `reg2053` at 0x2014: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd2`.
pub mod reg2053 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd2",
        2053,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2053, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2053, 16, 12, false, true, &[]);
}

/// Register `reg2054` at 0x2018: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd3`.
pub mod reg2054 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_var_thd3",
        2054,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2054, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2054, 16, 12, false, true, &[]);
}

/// Register `reg2055` at 0x201c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt0`.
pub mod reg2055 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt0",
        2055,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2055, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2055, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2055, 16, 8, false, true, &[]);
}

/// Register `reg2056` at 0x2020: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt1`.
pub mod reg2056 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt1",
        2056,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2056, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2056, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2056, 16, 8, false, true, &[]);
}

/// Register `reg2057` at 0x2024: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt2`.
pub mod reg2057 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt2",
        2057,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2057, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2057, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2057, 16, 8, false, true, &[]);
}

/// Register `reg2058` at 0x2028: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt3`.
pub mod reg2058 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_inter_atf.rdo_b_atf_wgt3",
        2058,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2058, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2058, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2058, 16, 8, false, true, &[]);
}

/// Register `reg2059` at 0x202c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_cime_thd0`.
pub mod reg2059 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_cime_thd0",
        2059,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2059, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2059, 16, 12, false, true, &[]);
}

/// Register `reg2060` at 0x2030: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_cime_thd1`.
pub mod reg2060 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_cime_thd1",
        2060,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2060, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD3: Field =
        Field::new(Region::Class(4), 2060, 16, 12, false, true, &[]);
}

/// Register `reg2061` at 0x2034: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd0`.
pub mod reg2061 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd0",
        2061,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2061, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2061, 16, 12, false, true, &[]);
}

/// Register `reg2062` at 0x2038: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd1`.
pub mod reg2062 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd1",
        2062,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2062, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2062, 16, 12, false, true, &[]);
}

/// Register `reg2063` at 0x203c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd2`.
pub mod reg2063 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd2",
        2063,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2063, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2063, 16, 12, false, true, &[]);
}

/// Register `reg2064` at 0x2040: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd3`.
pub mod reg2064 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_var_thd3",
        2064,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD40: Field = Field::new(Region::Class(4), 2064, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD41: Field =
        Field::new(Region::Class(4), 2064, 16, 12, false, true, &[]);
}

/// Register `reg2065` at 0x2044: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt0`.
pub mod reg2065 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt0",
        2065,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2065, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2065, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2065, 16, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2065, 24, 8, false, true, &[]);
}

/// Register `reg2066` at 0x2048: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt1`.
pub mod reg2066 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt1",
        2066,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2066, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2066, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2066, 16, 8, false, true, &[]);
}

/// Register `reg2067` at 0x204c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt2`.
pub mod reg2067 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt2",
        2067,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2067, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2067, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2067, 16, 8, false, true, &[]);
}

/// Register `reg2068` at 0x2050: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt3`.
pub mod reg2068 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b64_skip_atf.rdo_b_atf_wgt3",
        2068,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT40: Field = Field::new(Region::Class(4), 2068, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT41: Field = Field::new(Region::Class(4), 2068, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT42: Field = Field::new(Region::Class(4), 2068, 16, 8, false, true, &[]);
}

/// Register `reg2069` at 0x2054: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_cime_thd0`.
pub mod reg2069 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_cime_thd0",
        2069,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2069, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2069, 16, 12, false, true, &[]);
}

/// Register `reg2070` at 0x2058: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_cime_thd1`.
pub mod reg2070 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_cime_thd1",
        2070,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2070, 0, 12, false, true, &[]);
}

/// Register `reg2071` at 0x205c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd0`.
pub mod reg2071 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd0",
        2071,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2071, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2071, 16, 12, false, true, &[]);
}

/// Register `reg2072` at 0x2060: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd1`.
pub mod reg2072 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd1",
        2072,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2072, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2072, 16, 12, false, true, &[]);
}

/// Register `reg2073` at 0x2064: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd2`.
pub mod reg2073 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd2",
        2073,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2073, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2073, 16, 12, false, true, &[]);
}

/// Register `reg2074` at 0x2068: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd3`.
pub mod reg2074 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_var_thd3",
        2074,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2074, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2074, 16, 12, false, true, &[]);
}

/// Register `reg2075` at 0x206c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt0`.
pub mod reg2075 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt0",
        2075,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2075, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2075, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2075, 16, 8, false, true, &[]);
}

/// Register `reg2076` at 0x2070: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt1`.
pub mod reg2076 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt1",
        2076,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2076, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2076, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2076, 16, 8, false, true, &[]);
}

/// Register `reg2077` at 0x2074: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt2`.
pub mod reg2077 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt2",
        2077,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2077, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2077, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2077, 16, 8, false, true, &[]);
}

/// Register `reg2078` at 0x2078: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt3`.
pub mod reg2078 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_intra_atf.rdo_b_atf_wgt3",
        2078,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2078, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2078, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2078, 16, 8, false, true, &[]);
}

/// Register `reg2079` at 0x207c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_cime_thd0`.
pub mod reg2079 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_cime_thd0",
        2079,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2079, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2079, 16, 12, false, true, &[]);
}

/// Register `reg2080` at 0x2080: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_cime_thd1`.
pub mod reg2080 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_cime_thd1",
        2080,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2080, 0, 12, false, true, &[]);
}

/// Register `reg2081` at 0x2084: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd0`.
pub mod reg2081 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd0",
        2081,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2081, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2081, 16, 12, false, true, &[]);
}

/// Register `reg2082` at 0x2088: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd1`.
pub mod reg2082 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd1",
        2082,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2082, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2082, 16, 12, false, true, &[]);
}

/// Register `reg2083` at 0x208c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd2`.
pub mod reg2083 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd2",
        2083,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2083, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2083, 16, 12, false, true, &[]);
}

/// Register `reg2084` at 0x2090: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd3`.
pub mod reg2084 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_var_thd3",
        2084,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2084, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2084, 16, 12, false, true, &[]);
}

/// Register `reg2085` at 0x2094: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt0`.
pub mod reg2085 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt0",
        2085,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2085, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2085, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2085, 16, 8, false, true, &[]);
}

/// Register `reg2086` at 0x2098: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt1`.
pub mod reg2086 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt1",
        2086,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2086, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2086, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2086, 16, 8, false, true, &[]);
}

/// Register `reg2087` at 0x209c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt2`.
pub mod reg2087 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt2",
        2087,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2087, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2087, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2087, 16, 8, false, true, &[]);
}

/// Register `reg2088` at 0x20a0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt3`.
pub mod reg2088 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_inter_atf.rdo_b_atf_wgt3",
        2088,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2088, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2088, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2088, 16, 8, false, true, &[]);
}

/// Register `reg2089` at 0x20a4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_cime_thd0`.
pub mod reg2089 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_cime_thd0",
        2089,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2089, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2089, 16, 12, false, true, &[]);
}

/// Register `reg2090` at 0x20a8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_cime_thd1`.
pub mod reg2090 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_cime_thd1",
        2090,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2090, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD3: Field =
        Field::new(Region::Class(4), 2090, 16, 12, false, true, &[]);
}

/// Register `reg2091` at 0x20ac: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd0`.
pub mod reg2091 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd0",
        2091,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2091, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2091, 16, 12, false, true, &[]);
}

/// Register `reg2092` at 0x20b0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd1`.
pub mod reg2092 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd1",
        2092,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2092, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2092, 16, 12, false, true, &[]);
}

/// Register `reg2093` at 0x20b4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd2`.
pub mod reg2093 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd2",
        2093,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2093, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2093, 16, 12, false, true, &[]);
}

/// Register `reg2094` at 0x20b8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd3`.
pub mod reg2094 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_var_thd3",
        2094,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD40: Field = Field::new(Region::Class(4), 2094, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD41: Field =
        Field::new(Region::Class(4), 2094, 16, 12, false, true, &[]);
}

/// Register `reg2095` at 0x20bc: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt0`.
pub mod reg2095 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt0",
        2095,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2095, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2095, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2095, 16, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2095, 24, 8, false, true, &[]);
}

/// Register `reg2096` at 0x20c0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt1`.
pub mod reg2096 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt1",
        2096,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2096, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2096, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2096, 16, 8, false, true, &[]);
}

/// Register `reg2097` at 0x20c4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt2`.
pub mod reg2097 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt2",
        2097,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2097, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2097, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2097, 16, 8, false, true, &[]);
}

/// Register `reg2098` at 0x20c8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt3`.
pub mod reg2098 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b32_skip_atf.rdo_b_atf_wgt3",
        2098,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT40: Field = Field::new(Region::Class(4), 2098, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT41: Field = Field::new(Region::Class(4), 2098, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT42: Field = Field::new(Region::Class(4), 2098, 16, 8, false, true, &[]);
}

/// Register `reg2099` at 0x20cc: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_cime_thd0`.
pub mod reg2099 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_cime_thd0",
        2099,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2099, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2099, 16, 12, false, true, &[]);
}

/// Register `reg2100` at 0x20d0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_cime_thd1`.
pub mod reg2100 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_cime_thd1",
        2100,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2100, 0, 12, false, true, &[]);
}

/// Register `reg2101` at 0x20d4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd0`.
pub mod reg2101 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd0",
        2101,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2101, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2101, 16, 12, false, true, &[]);
}

/// Register `reg2102` at 0x20d8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd1`.
pub mod reg2102 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd1",
        2102,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2102, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2102, 16, 12, false, true, &[]);
}

/// Register `reg2103` at 0x20dc: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd2`.
pub mod reg2103 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd2",
        2103,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2103, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2103, 16, 12, false, true, &[]);
}

/// Register `reg2104` at 0x20e0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd3`.
pub mod reg2104 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_var_thd3",
        2104,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2104, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2104, 16, 12, false, true, &[]);
}

/// Register `reg2105` at 0x20e4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt0`.
pub mod reg2105 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt0",
        2105,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2105, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2105, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2105, 16, 8, false, true, &[]);
}

/// Register `reg2106` at 0x20e8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt1`.
pub mod reg2106 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt1",
        2106,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2106, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2106, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2106, 16, 8, false, true, &[]);
}

/// Register `reg2107` at 0x20ec: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt2`.
pub mod reg2107 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt2",
        2107,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2107, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2107, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2107, 16, 8, false, true, &[]);
}

/// Register `reg2108` at 0x20f0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt3`.
pub mod reg2108 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_intra_atf.rdo_b_atf_wgt3",
        2108,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2108, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2108, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2108, 16, 8, false, true, &[]);
}

/// Register `reg2109` at 0x20f4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_cime_thd0`.
pub mod reg2109 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_cime_thd0",
        2109,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2109, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2109, 16, 12, false, true, &[]);
}

/// Register `reg2110` at 0x20f8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_cime_thd1`.
pub mod reg2110 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_cime_thd1",
        2110,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2110, 0, 12, false, true, &[]);
}

/// Register `reg2111` at 0x20fc: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd0`.
pub mod reg2111 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd0",
        2111,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2111, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2111, 16, 12, false, true, &[]);
}

/// Register `reg2112` at 0x2100: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd1`.
pub mod reg2112 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd1",
        2112,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2112, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2112, 16, 12, false, true, &[]);
}

/// Register `reg2113` at 0x2104: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd2`.
pub mod reg2113 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd2",
        2113,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2113, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2113, 16, 12, false, true, &[]);
}

/// Register `reg2114` at 0x2108: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd3`.
pub mod reg2114 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_var_thd3",
        2114,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2114, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2114, 16, 12, false, true, &[]);
}

/// Register `reg2115` at 0x210c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt0`.
pub mod reg2115 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt0",
        2115,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2115, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2115, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2115, 16, 8, false, true, &[]);
}

/// Register `reg2116` at 0x2110: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt1`.
pub mod reg2116 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt1",
        2116,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2116, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2116, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2116, 16, 8, false, true, &[]);
}

/// Register `reg2117` at 0x2114: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt2`.
pub mod reg2117 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt2",
        2117,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2117, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2117, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2117, 16, 8, false, true, &[]);
}

/// Register `reg2118` at 0x2118: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt3`.
pub mod reg2118 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_inter_atf.rdo_b_atf_wgt3",
        2118,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2118, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2118, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2118, 16, 8, false, true, &[]);
}

/// Register `reg2119` at 0x211c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_cime_thd0`.
pub mod reg2119 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_cime_thd0",
        2119,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2119, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2119, 16, 12, false, true, &[]);
}

/// Register `reg2120` at 0x2120: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_cime_thd1`.
pub mod reg2120 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_cime_thd1",
        2120,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2120, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD3: Field =
        Field::new(Region::Class(4), 2120, 16, 12, false, true, &[]);
}

/// Register `reg2121` at 0x2124: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd0`.
pub mod reg2121 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd0",
        2121,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2121, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2121, 16, 12, false, true, &[]);
}

/// Register `reg2122` at 0x2128: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd1`.
pub mod reg2122 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd1",
        2122,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2122, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2122, 16, 12, false, true, &[]);
}

/// Register `reg2123` at 0x212c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd2`.
pub mod reg2123 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd2",
        2123,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2123, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2123, 16, 12, false, true, &[]);
}

/// Register `reg2124` at 0x2130: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd3`.
pub mod reg2124 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_var_thd3",
        2124,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD40: Field = Field::new(Region::Class(4), 2124, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD41: Field =
        Field::new(Region::Class(4), 2124, 16, 12, false, true, &[]);
}

/// Register `reg2125` at 0x2134: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt0`.
pub mod reg2125 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt0",
        2125,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2125, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2125, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2125, 16, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2125, 24, 8, false, true, &[]);
}

/// Register `reg2126` at 0x2138: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt1`.
pub mod reg2126 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt1",
        2126,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2126, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2126, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2126, 16, 8, false, true, &[]);
}

/// Register `reg2127` at 0x213c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt2`.
pub mod reg2127 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt2",
        2127,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2127, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2127, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2127, 16, 8, false, true, &[]);
}

/// Register `reg2128` at 0x2140: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt3`.
pub mod reg2128 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b16_skip_atf.rdo_b_atf_wgt3",
        2128,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT40: Field = Field::new(Region::Class(4), 2128, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT41: Field = Field::new(Region::Class(4), 2128, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT42: Field = Field::new(Region::Class(4), 2128, 16, 8, false, true, &[]);
}

/// Register `reg2129` at 0x2144: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_cime_thd0`.
pub mod reg2129 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_cime_thd0",
        2129,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2129, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2129, 16, 12, false, true, &[]);
}

/// Register `reg2130` at 0x2148: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_cime_thd1`.
pub mod reg2130 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_cime_thd1",
        2130,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2130, 0, 12, false, true, &[]);
}

/// Register `reg2131` at 0x214c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd0`.
pub mod reg2131 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd0",
        2131,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2131, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2131, 16, 12, false, true, &[]);
}

/// Register `reg2132` at 0x2150: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd1`.
pub mod reg2132 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd1",
        2132,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2132, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2132, 16, 12, false, true, &[]);
}

/// Register `reg2133` at 0x2154: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd2`.
pub mod reg2133 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd2",
        2133,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2133, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2133, 16, 12, false, true, &[]);
}

/// Register `reg2134` at 0x2158: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd3`.
pub mod reg2134 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_var_thd3",
        2134,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2134, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2134, 16, 12, false, true, &[]);
}

/// Register `reg2135` at 0x215c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt0`.
pub mod reg2135 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt0",
        2135,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2135, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2135, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2135, 16, 8, false, true, &[]);
}

/// Register `reg2136` at 0x2160: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt1`.
pub mod reg2136 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt1",
        2136,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2136, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2136, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2136, 16, 8, false, true, &[]);
}

/// Register `reg2137` at 0x2164: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt2`.
pub mod reg2137 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt2",
        2137,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2137, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2137, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2137, 16, 8, false, true, &[]);
}

/// Register `reg2138` at 0x2168: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt3`.
pub mod reg2138 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_intra_atf.rdo_b_atf_wgt3",
        2138,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2138, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2138, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2138, 16, 8, false, true, &[]);
}

/// Register `reg2139` at 0x216c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_cime_thd0`.
pub mod reg2139 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_cime_thd0",
        2139,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2139, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2139, 16, 12, false, true, &[]);
}

/// Register `reg2140` at 0x2170: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_cime_thd1`.
pub mod reg2140 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_cime_thd1",
        2140,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2140, 0, 12, false, true, &[]);
}

/// Register `reg2141` at 0x2174: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd0`.
pub mod reg2141 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd0",
        2141,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD00: Field = Field::new(Region::Class(4), 2141, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD01: Field =
        Field::new(Region::Class(4), 2141, 16, 12, false, true, &[]);
}

/// Register `reg2142` at 0x2178: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd1`.
pub mod reg2142 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd1",
        2142,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2142, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2142, 16, 12, false, true, &[]);
}

/// Register `reg2143` at 0x217c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd2`.
pub mod reg2143 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd2",
        2143,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2143, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2143, 16, 12, false, true, &[]);
}

/// Register `reg2144` at 0x2180: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd3`.
pub mod reg2144 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_var_thd3",
        2144,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2144, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2144, 16, 12, false, true, &[]);
}

/// Register `reg2145` at 0x2184: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt0`.
pub mod reg2145 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt0",
        2145,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2145, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT01: Field = Field::new(Region::Class(4), 2145, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT02: Field = Field::new(Region::Class(4), 2145, 16, 8, false, true, &[]);
}

/// Register `reg2146` at 0x2188: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt1`.
pub mod reg2146 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt1",
        2146,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2146, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2146, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2146, 16, 8, false, true, &[]);
}

/// Register `reg2147` at 0x218c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt2`.
pub mod reg2147 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt2",
        2147,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2147, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2147, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2147, 16, 8, false, true, &[]);
}

/// Register `reg2148` at 0x2190: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt3`.
pub mod reg2148 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_inter_atf.rdo_b_atf_wgt3",
        2148,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2148, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2148, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2148, 16, 8, false, true, &[]);
}

/// Register `reg2149` at 0x2194: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_cime_thd0`.
pub mod reg2149 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_cime_thd0",
        2149,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD0: Field = Field::new(Region::Class(4), 2149, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD1: Field =
        Field::new(Region::Class(4), 2149, 16, 12, false, true, &[]);
}

/// Register `reg2150` at 0x2198: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_cime_thd1`.
pub mod reg2150 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_cime_thd1",
        2150,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_CIME_THD2: Field = Field::new(Region::Class(4), 2150, 0, 12, false, true, &[]);
    pub const CU_RDO_CIME_THD3: Field =
        Field::new(Region::Class(4), 2150, 16, 12, false, true, &[]);
}

/// Register `reg2151` at 0x219c: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd0`.
pub mod reg2151 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd0",
        2151,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD10: Field = Field::new(Region::Class(4), 2151, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD11: Field =
        Field::new(Region::Class(4), 2151, 16, 12, false, true, &[]);
}

/// Register `reg2152` at 0x21a0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd1`.
pub mod reg2152 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd1",
        2152,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD20: Field = Field::new(Region::Class(4), 2152, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD21: Field =
        Field::new(Region::Class(4), 2152, 16, 12, false, true, &[]);
}

/// Register `reg2153` at 0x21a4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd2`.
pub mod reg2153 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd2",
        2153,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD30: Field = Field::new(Region::Class(4), 2153, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD31: Field =
        Field::new(Region::Class(4), 2153, 16, 12, false, true, &[]);
}

/// Register `reg2154` at 0x21a8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd3`.
pub mod reg2154 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_var_thd3",
        2154,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_VAR_THD40: Field = Field::new(Region::Class(4), 2154, 0, 12, false, true, &[]);
    pub const CU_RDO_VAR_THD41: Field =
        Field::new(Region::Class(4), 2154, 16, 12, false, true, &[]);
}

/// Register `reg2155` at 0x21ac: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt0`.
pub mod reg2155 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt0",
        2155,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT00: Field = Field::new(Region::Class(4), 2155, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT10: Field = Field::new(Region::Class(4), 2155, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT11: Field = Field::new(Region::Class(4), 2155, 16, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT12: Field = Field::new(Region::Class(4), 2155, 24, 8, false, true, &[]);
}

/// Register `reg2156` at 0x21b0: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt1`.
pub mod reg2156 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt1",
        2156,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT20: Field = Field::new(Region::Class(4), 2156, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT21: Field = Field::new(Region::Class(4), 2156, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT22: Field = Field::new(Region::Class(4), 2156, 16, 8, false, true, &[]);
}

/// Register `reg2157` at 0x21b4: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt2`.
pub mod reg2157 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt2",
        2157,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT30: Field = Field::new(Region::Class(4), 2157, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT31: Field = Field::new(Region::Class(4), 2157, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT32: Field = Field::new(Region::Class(4), 2157, 16, 8, false, true, &[]);
}

/// Register `reg2158` at 0x21b8: 1 word, ReadWrite.
///
/// The C path is `reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt3`.
pub mod reg2158 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(4),
        "reg_rdo.rdo_b8_skip_atf.rdo_b_atf_wgt3",
        2158,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CU_RDO_ATF_WGT40: Field = Field::new(Region::Class(4), 2158, 0, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT41: Field = Field::new(Region::Class(4), 2158, 8, 8, false, true, &[]);
    pub const CU_RDO_ATF_WGT42: Field = Field::new(Region::Class(4), 2158, 16, 8, false, true, &[]);
}

/// Register `reg2159` at 0x21bc: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2159;

/// Register `reg2160` at 0x21c0: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2160;

/// Register `reg2161` at 0x21c4: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2161;

/// Register `reg2162` at 0x21c8: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2162;

/// Register `reg2163` at 0x21cc: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2163;

/// Register `reg2164` at 0x21d0: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2164;

/// Register `reg2165` at 0x21d4: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2165;

/// Register `reg2166` at 0x21d8: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2166;

/// Register `reg2167` at 0x21dc: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2167;

/// Register `reg2168` at 0x21e0: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2168;

/// Register `reg2169` at 0x21e4: the same register as in the H.264 layout.
pub use crate::vepu580_h264::sqi::reg2169;
