//! The Rockchip ISP21 (`rk-isp21-config.h`) and ISP30 (`rk-isp3-config.h`) parameter and
//! statistics buffers as the vendor kernel's `rkisp` driver takes and fills them, with the
//! module bits it reads from `module_ens` / `module_en_update` / `module_cfg_update`.
//!
//! The layouts are bindgen's ([`isp21`], [`isp3x`], [`isp32`]; regenerate with
//! `scripts/rkisp-bindgen.sh`); their sizes and the offsets of the blocks
//! ISP drivers use are checked here against the C compiler's for the same headers.

#![no_std]

pub mod isp21;
pub mod isp3x;

/// The ISP32 layouts (RV1106's ISP32 and RK3562's ISP32-lite), from `rk-isp32-config.h`. Bindgen
/// output: see the module's own header.
pub mod isp32;
pub mod isp32_layout;

/// The ISP35 layouts (RV1126B), from `rk-isp35-config.h`. Bindgen output: see the module's own
/// header.
pub mod isp35;
pub mod isp35_layout;

pub use isp3x::*;

/// A module's bit in the parameter buffer's `module_*` masks (`ISP2X_ID_*` of
/// `rk-isp2-config.h`; bindgen does not expand the headers' `_BITULL` macros).
pub mod module {
    /// The `DPCC` bit of a module.
    pub const DPCC: u64 = 1 << 0;
    /// The `BLS` bit of a module.
    pub const BLS: u64 = 1 << 1;
    /// The `SDG` bit of a module.
    pub const SDG: u64 = 1 << 2;
    /// The `LSC` bit of a module.
    pub const LSC: u64 = 1 << 4;
    /// The `AWB_GAIN` bit of a module.
    pub const AWB_GAIN: u64 = 1 << 5;
    /// The `BDM` bit of a module.
    pub const BDM: u64 = 1 << 7;
    /// The `CCM` bit of a module.
    pub const CCM: u64 = 1 << 8;
    /// The `GOC` bit of a module.
    pub const GOC: u64 = 1 << 9;
    /// The `CPROC` bit of a module.
    pub const CPROC: u64 = 1 << 10;
    /// The `RAWAF` bit of a module.
    pub const RAWAF: u64 = 1 << 17;
    /// The `RAWAE0` bit of a module.
    pub const RAWAE0: u64 = 1 << 18;
    /// The `RAWAE1` bit of a module.
    pub const RAWAE1: u64 = 1 << 19;
    /// The `RAWAE2` bit of a module.
    pub const RAWAE2: u64 = 1 << 20;
    /// The `RAWAE3` bit of a module.
    pub const RAWAE3: u64 = 1 << 21;
    /// The `RAWAWB` bit of a module.
    pub const RAWAWB: u64 = 1 << 22;
    /// The `RAWHIST0` bit of a module.
    pub const RAWHIST0: u64 = 1 << 23;
    /// The `RAWHIST1` bit of a module.
    pub const RAWHIST1: u64 = 1 << 24;
    /// The `RAWHIST2` bit of a module.
    pub const RAWHIST2: u64 = 1 << 25;
    /// The `RAWHIST3` bit of a module.
    pub const RAWHIST3: u64 = 1 << 26;
    /// The `GIC` bit of a module.
    pub const GIC: u64 = 1 << 30;
    /// The `DHAZ` bit of a module.
    pub const DHAZ: u64 = 1 << 31;
    /// The `GAIN` bit of a module.
    pub const GAIN: u64 = 1 << 34;
    /// The `DEBAYER` bit of a module.
    pub const DEBAYER: u64 = 1 << 35;
    /// The `BAYNR` bit of a module.
    pub const BAYNR: u64 = 1 << 36;
    /// The `BAY3D` bit of a module.
    pub const BAY3D: u64 = 1 << 37;
    /// The `YNR` bit of a module.
    pub const YNR: u64 = 1 << 38;
    /// The `CNR` bit of a module.
    pub const CNR: u64 = 1 << 39;
    /// The `SHARP` bit of a module.
    pub const SHARP: u64 = 1 << 40;
    /// The `DRC` bit of a module.
    pub const DRC: u64 = 1 << 41;
    /// The `CSM` bit of a module.
    pub const CSM: u64 = 1 << 43;
    /// The `CGC` bit of a module.
    pub const CGC: u64 = 1 << 44;
}

/// The C compiler's sizes and offsets for the same headers (clang, aarch64 and x86-64 alike:
/// the structures are packed).
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<isp3x_isp_params_cfg>() == 10921);
    assert!(size_of::<rkisp3x_isp_stat_buffer>() == 16172);
    assert!(size_of::<isp3x_isp_meas_cfg>() == 2012);
    assert!(size_of::<isp3x_isp_other_cfg>() == 8785);
    assert!(size_of::<isp3x_stat>() == 16160);
    assert!(offset_of!(isp3x_isp_params_cfg, meas) == 28);
    assert!(offset_of!(isp3x_isp_params_cfg, others) == 2040);
    assert!(offset_of!(isp3x_isp_params_cfg, exposure) == 10825);
    assert!(offset_of!(isp3x_isp_other_cfg, bls_cfg) == 0);
    assert!(offset_of!(isp3x_isp_other_cfg, awb_gain_cfg) == 2671);
    assert!(offset_of!(isp3x_isp_other_cfg, ccm_cfg) == 2780);
    assert!(offset_of!(isp3x_isp_other_cfg, gammaout_cfg) == 2846);
    assert!(offset_of!(isp3x_stat, rawae0) == 4194);
    assert!(offset_of!(isp3x_stat, rawhist1) == 6392);
    assert!(offset_of!(isp3x_stat, rawawb) == 12056);
    assert!(offset_of!(isp3x_stat, bls) == 16152);
    assert!(offset_of!(isp3x_isp_meas_cfg, rawawb) == 191);
    assert!(offset_of!(isp3x_isp_meas_cfg, rawae0) == 1097);
    assert!(offset_of!(isp3x_isp_meas_cfg, rawhist1) == 1286);
};

/// The same for ISP21 (`rk-isp21-config.h`, `rk-isp2-config.h`).
const _: () = {
    use core::mem::{offset_of, size_of};
    use isp21::*;
    assert!(size_of::<isp21_isp_params_cfg>() == 10475);
    assert!(size_of::<rkisp_isp21_stat_buffer>() == 13700);
    assert!(size_of::<isp21_isp_meas_cfg>() == 2194);
    assert!(size_of::<isp21_isp_other_cfg>() == 8253);
    assert!(size_of::<isp21_stat>() == 13688);
    assert!(size_of::<isp2x_lsc_cfg>() == 2384);
    assert!(size_of::<isp2x_gammaout_cfg>() == 93);
    assert!(offset_of!(isp21_isp_params_cfg, meas) == 28);
    assert!(offset_of!(isp21_isp_params_cfg, others) == 2222);
    assert!(offset_of!(isp21_isp_other_cfg, bls_cfg) == 0);
    assert!(offset_of!(isp21_isp_other_cfg, lsc_cfg) == 222);
    assert!(offset_of!(isp21_isp_other_cfg, awb_gain_cfg) == 2606);
    assert!(offset_of!(isp21_isp_other_cfg, ccm_cfg) == 2715);
    assert!(offset_of!(isp21_isp_other_cfg, gammaout_cfg) == 2781);
    assert!(offset_of!(isp21_isp_meas_cfg, rawae3) == 988);
    assert!(offset_of!(isp21_isp_meas_cfg, rawhist1) == 1229);
    assert!(offset_of!(rkisp_isp21_stat_buffer, params) == 12);
    assert!(offset_of!(isp21_stat, rawae3) == 6737);
    assert!(offset_of!(isp21_stat, rawhist1) == 10344);
    assert!(offset_of!(isp21_stat, bls) == 13544);
    assert!(offset_of!(isp21_stat, dhaz) == 13552);
};
