//! The ISP32 generations (RV1106 and RK3562) and the video nodes each exposes, as the vendor
//! kernel's `rkisp` driver registers them. The buffers' layouts are in [`crate::uapi::isp32`].
//! Written, untested on board.

/// The ISP32 generation a buffer or a device is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The full ISP32: RV1106 (the driver's `ISP_V32`, `rv1106_isp_match_data`).
    Isp32,
    /// The ISP32-lite: RK3562 (the driver's `ISP_V32_L`, `rk3562_isp_match_data`).
    Isp32Lite,
}

/// The video nodes an ISP32 exposes, as the vendor driver registers them
/// (`rkisp_register_stream_v32` and the `*_VDEV_NAME` macros of `capture.h`).
pub mod nodes {
    /// The main path (both variants).
    pub const MAINPATH: &str = "rkisp_mainpath";
    /// The self path, the second, scaled output (both variants).
    pub const SELFPATH: &str = "rkisp_selfpath";
    /// The bypass path (ISP32 only; the path diagram is at `capture_v32.c:17-25`).
    pub const BYPASSPATH: &str = "rkisp_bypasspath";
    /// The main path's 4x4-sampled output (ISP32 only, `MPDS`).
    pub const MAINPATH_4X4: &str = "rkisp_mainpath_4x4sampling";
    /// The bypass path's 4x4-sampled output (ISP32 only, `BPDS`).
    pub const BYPASSPATH_4X4: &str = "rkisp_bypasspath_4x4sampling";
    /// The luma path (ISP32 only).
    pub const LUMAPATH: &str = "rkisp_lumapath";
    /// The IQ tool's virtual output (both variants).
    pub const IQTOOL: &str = "rkisp_iqtool";
}

impl Variant {
    /// The video nodes this variant exposes. The pyramid levels come from [`nodes::SELFPATH`]
    /// (the scaled output) and, on the full ISP32 only, the 4x4-sampled outputs
    /// ([`nodes::MAINPATH_4X4`], [`nodes::BYPASSPATH_4X4`]); the BSP has no 8x8 path.
    pub fn output_nodes(self) -> &'static [&'static str] {
        match self {
            Self::Isp32 => &[
                nodes::MAINPATH,
                nodes::SELFPATH,
                nodes::BYPASSPATH,
                nodes::MAINPATH_4X4,
                nodes::BYPASSPATH_4X4,
                nodes::LUMAPATH,
                nodes::IQTOOL,
            ],
            Self::Isp32Lite => &[nodes::MAINPATH, nodes::SELFPATH, nodes::IQTOOL],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selfpath_is_the_second_output_of_both_variants() {
        for v in [Variant::Isp32, Variant::Isp32Lite] {
            assert_eq!(v.output_nodes()[0], nodes::MAINPATH);
            assert!(v.output_nodes().contains(&nodes::SELFPATH));
        }
        assert_eq!(Variant::Isp32Lite.output_nodes().len(), 3);
        assert_eq!(Variant::Isp32.output_nodes().len(), 7);
    }
}
