//! Which ISP a chip has, and the layouts `camera.isp/1` names for each.
//!
//! The layout numbers mean something only to this driver and its callers: each generation has
//! one parameter layout and one statistics layout, numbered `0x<generation>01` and
//! `0x<generation>02`, so a caller that reads a number knows the structure without asking which
//! chip it is on.

use lemnos_contract_camera_isp::ConfigLayout;

use crate::uapi;

/// `camera.isp/1` config layout: an ISP21 parameter buffer (`struct isp21_isp_params_cfg`).
pub const LAYOUT_ISP21_PARAMS: u32 = 0x2101;
/// `camera.isp/1` statistics layout: an ISP21 statistics buffer
/// (`struct rkisp_isp21_stat_buffer`).
pub const LAYOUT_ISP21_STATISTICS: u32 = 0x2102;
/// An ISP30 parameter buffer (`struct isp3x_isp_params_cfg`).
pub const LAYOUT_ISP30_PARAMS: u32 = 0x3001;
/// An ISP30 statistics buffer (`struct rkisp3x_isp_stat_buffer`).
pub const LAYOUT_ISP30_STATISTICS: u32 = 0x3002;
/// An ISP35 parameter buffer (`struct isp35_isp_params_cfg`).
pub const LAYOUT_ISP35_PARAMS: u32 = 0x3501;
/// An ISP35 statistics buffer (`struct rkisp35_stat_buffer`).
pub const LAYOUT_ISP35_STATISTICS: u32 = 0x3502;

/// Image outputs every generation has: the main path (output 0) and the self path, a second
/// scaled image of the same frame (output 1).
pub const OUTPUTS: u8 = 2;

/// An `rkisp` generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    /// The vendor driver's `ISP_V21`.
    Isp21,
    /// `ISP_V30`.
    Isp30,
    /// `ISP_V35`.
    Isp35,
}

/// Bytes of a buffer layout, which fit `u32` (checked at compile time below).
const fn bytes(n: usize) -> u32 {
    n as u32
}

const _: () = {
    assert!(uapi::ISP21_PARAMS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP21_STATS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP30_PARAMS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP30_STATS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP35_PARAMS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP35_STATS_BYTES <= u32::MAX as usize);
};

impl Generation {
    /// The ISP of the chip named by one devicetree compatible string (`rockchip,rk3566`);
    /// `None` for anything else, chips with other ISP generations included.
    #[must_use]
    pub fn from_soc(compatible: &str) -> Option<Self> {
        let soc = compatible.strip_prefix("rockchip,")?;
        Some(match soc {
            "rk3566" | "rk3568" => Self::Isp21,
            "rk3588" | "rk3588s" => Self::Isp30,
            "rv1126b" => Self::Isp35,
            _ => return None,
        })
    }

    /// The ISP of the first chip among a machine's compatible strings (the root node's list: the
    /// board's first, then the chip's).
    pub fn from_compatibles<'a>(list: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        list.into_iter().find_map(Self::from_soc)
    }

    /// The generation a board file names: `isp21`, `isp30` or `isp35`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "isp21" => Some(Self::Isp21),
            "isp30" => Some(Self::Isp30),
            "isp35" => Some(Self::Isp35),
            _ => None,
        }
    }

    /// The parameter layout a job's config buffer holds.
    #[must_use]
    pub const fn config_layout(self) -> ConfigLayout {
        let (layout, n) = match self {
            Self::Isp21 => (LAYOUT_ISP21_PARAMS, uapi::ISP21_PARAMS_BYTES),
            Self::Isp30 => (LAYOUT_ISP30_PARAMS, uapi::ISP30_PARAMS_BYTES),
            Self::Isp35 => (LAYOUT_ISP35_PARAMS, uapi::ISP35_PARAMS_BYTES),
        };
        ConfigLayout {
            layout,
            bytes: bytes(n),
        }
    }

    /// The statistics layout and its size: what [`IspInfo`](lemnos_contract_camera_isp::IspInfo)
    /// reports as `statistics_layout` and `statistics_bytes`.
    #[must_use]
    pub const fn statistics_layout(self) -> ConfigLayout {
        let (layout, n) = match self {
            Self::Isp21 => (LAYOUT_ISP21_STATISTICS, uapi::ISP21_STATS_BYTES),
            Self::Isp30 => (LAYOUT_ISP30_STATISTICS, uapi::ISP30_STATS_BYTES),
            Self::Isp35 => (LAYOUT_ISP35_STATISTICS, uapi::ISP35_STATS_BYTES),
        };
        ConfigLayout {
            layout,
            bytes: bytes(n),
        }
    }
}
