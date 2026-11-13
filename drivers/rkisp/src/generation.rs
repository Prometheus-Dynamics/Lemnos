//! Which ISP a chip has, and the layouts `camera.isp/1` names for each.
//!
//! The layout numbers mean something only to this driver and its callers: each generation has
//! one parameter layout and one statistics layout, numbered `0x<generation>01` and
//! `0x<generation>02`, so a caller that reads a number knows the structure without asking which
//! chip it is on.

use crate::uapi;

/// One buffer layout: the number `camera.isp/1` gives it and the bytes one buffer of it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferLayout {
    /// The layout number (`LAYOUT_*`).
    pub layout: u32,
    /// Bytes of one buffer.
    pub bytes: u32,
}

#[cfg(feature = "device")]
impl From<BufferLayout> for lemnos_contract_camera_isp::ConfigLayout {
    fn from(l: BufferLayout) -> Self {
        Self {
            layout: l.layout,
            bytes: l.bytes,
        }
    }
}

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
/// An ISP32 parameter buffer (`struct isp32_isp_params_cfg`), for ISP_V32 and ISP_V32_L alike.
pub const LAYOUT_ISP32_PARAMS: u32 = 0x3201;
/// An ISP32 statistics buffer (`struct rkisp32_isp_stat_buffer`).
pub const LAYOUT_ISP32_STATISTICS: u32 = 0x3202;
/// An ISP32 lite statistics buffer (`struct rkisp32_lite_stat_buffer`, ISP_V32_L).
pub const LAYOUT_ISP32_LITE_STATISTICS: u32 = 0x3203;
/// An ISP39 parameter buffer (`struct isp39_isp_params_cfg`).
pub const LAYOUT_ISP39_PARAMS: u32 = 0x3901;
/// An ISP39 statistics buffer (`struct rkisp39_stat_buffer`).
pub const LAYOUT_ISP39_STATISTICS: u32 = 0x3902;

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
    /// `ISP_V32` (rv1106): the full statistics structure.
    Isp32,
    /// `ISP_V32_L` (rk3562): the same parameter structure as [`Generation::Isp32`], lite
    /// statistics.
    Isp32Lite,
    /// `ISP_V39` (rk3576).
    Isp39,
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
    assert!(uapi::ISP32_PARAMS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP32_STATS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP32_LITE_STATS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP39_PARAMS_BYTES <= u32::MAX as usize);
    assert!(uapi::ISP39_STATS_BYTES <= u32::MAX as usize);
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
            "rv1106" => Self::Isp32,
            "rk3562" => Self::Isp32Lite,
            "rk3576" => Self::Isp39,
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
            "isp32" => Some(Self::Isp32),
            "isp32lite" => Some(Self::Isp32Lite),
            "isp39" => Some(Self::Isp39),
            _ => None,
        }
    }

    /// The parameter layout a job's config buffer holds.
    #[must_use]
    pub const fn config_layout(self) -> BufferLayout {
        let (layout, n) = match self {
            Self::Isp21 => (LAYOUT_ISP21_PARAMS, uapi::ISP21_PARAMS_BYTES),
            Self::Isp30 => (LAYOUT_ISP30_PARAMS, uapi::ISP30_PARAMS_BYTES),
            Self::Isp35 => (LAYOUT_ISP35_PARAMS, uapi::ISP35_PARAMS_BYTES),
            Self::Isp32 | Self::Isp32Lite => (LAYOUT_ISP32_PARAMS, uapi::ISP32_PARAMS_BYTES),
            Self::Isp39 => (LAYOUT_ISP39_PARAMS, uapi::ISP39_PARAMS_BYTES),
        };
        BufferLayout {
            layout,
            bytes: bytes(n),
        }
    }

    /// The module bits this generation's parameter buffers define (`uapi::module` for ISP21,
    /// `module30`, `module32`, `module35`, `module39`).
    #[must_use]
    pub const fn module_bits(self) -> u64 {
        match self {
            Self::Isp21 => uapi::module::ALL,
            Self::Isp30 => uapi::module30::ALL,
            Self::Isp32 | Self::Isp32Lite => uapi::module32::ALL,
            Self::Isp35 => uapi::module35::ALL,
            Self::Isp39 => uapi::module39::ALL,
        }
    }

    /// Whether a parameter buffer's module mask names only modules this generation has. A mask
    /// with any other bit set is a caller's mistake: an ISP35 bit on an ISP30 is refused, not
    /// passed to the kernel.
    #[must_use]
    pub const fn accepts_modules(self, mask: u64) -> bool {
        mask & !self.module_bits() == 0
    }

    /// The statistics layout and its size: what the driver's `IspInfo` reports as
    /// `statistics_layout` and `statistics_bytes`.
    #[must_use]
    pub const fn statistics_layout(self) -> BufferLayout {
        let (layout, n) = match self {
            Self::Isp21 => (LAYOUT_ISP21_STATISTICS, uapi::ISP21_STATS_BYTES),
            Self::Isp30 => (LAYOUT_ISP30_STATISTICS, uapi::ISP30_STATS_BYTES),
            Self::Isp35 => (LAYOUT_ISP35_STATISTICS, uapi::ISP35_STATS_BYTES),
            Self::Isp32 => (LAYOUT_ISP32_STATISTICS, uapi::ISP32_STATS_BYTES),
            Self::Isp32Lite => (LAYOUT_ISP32_LITE_STATISTICS, uapi::ISP32_LITE_STATS_BYTES),
            Self::Isp39 => (LAYOUT_ISP39_STATISTICS, uapi::ISP39_STATS_BYTES),
        };
        BufferLayout {
            layout,
            bytes: bytes(n),
        }
    }
}
