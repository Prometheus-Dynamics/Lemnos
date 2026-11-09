//! Which Rockchip ISP a system has ([`Isp`], from the SoC) and the parameter buffer trait the
//! device takes ([`ParamBuffer`]). ISP21 (RK3566 / RK3568), ISP30 (RK3588 / RK3588S), ISP32
//! (RV1106 / RV1103) and ISP32-lite (RK3562). Written, untested on board: none of these has run
//! on its chip yet.

/// A Rockchip ISP generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isp {
    /// RK3566 / RK3568.
    Isp21,
    /// RK3588 / RK3588S.
    Isp30,
    /// RV1106 / RV1103 (the driver's `ISP_V32`).
    Isp32,
    /// RK3562 (the driver's `ISP_V32_L`).
    Isp32Lite,
    /// RV1126B (the driver's `ISP_V35`).
    Isp35,
}

impl Isp {
    /// The ISP of the SoC named by one devicetree compatible string (`rockchip,rk3566`);
    /// `None` for anything else, the RV1103B (ISP33) and RK3576 (generation unconfirmed)
    /// included.
    pub fn from_soc(compatible: &str) -> Option<Self> {
        let soc = compatible.strip_prefix("rockchip,")?;
        Some(match soc {
            "rk3566" | "rk3568" => Self::Isp21,
            "rk3588" | "rk3588s" => Self::Isp30,
            "rv1106" | "rv1103" => Self::Isp32,
            "rk3562" => Self::Isp32Lite,
            "rv1126b" => Self::Isp35,
            _ => return None,
        })
    }

    /// The ISP of the first SoC among a machine's compatible strings (the root node's list:
    /// the board's first, then the SoC's).
    pub fn from_compatibles<'a>(list: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        list.into_iter().find_map(Self::from_soc)
    }

    /// This machine's ISP, from `/proc/device-tree/compatible`; `None` off a Rockchip SoC this
    /// crate knows.
    #[cfg(feature = "std")]
    pub fn detect() -> Option<Self> {
        let bytes = std::fs::read("/proc/device-tree/compatible").ok()?;
        Self::from_compatibles(
            bytes
                .split(|&b| b == 0)
                .filter_map(|s| core::str::from_utf8(s).ok()),
        )
    }
}

/// A parameter buffer the driver takes as it is (`crate::device`'s queue). Implement it for
/// any buffer whose bytes are the driver's parameter structure for its generation.
pub trait ParamBuffer {
    /// The bytes of the driver's parameter structure.
    fn param_bytes(&self) -> &[u8];
}
