//! Which fixed-function 2D unit each target SoC has, by which route this crate reaches it, and
//! what it does with each resize filter. Plain data with no OS calls, readable on any target;
//! [`route_for`] answers "which route would serve this operation" from the same table.
//!
//! Sources: the mainline `rockchip-rga` driver in Linux 7.2 (`rga-hw.c`, `rga3-hw.c`, the RK356x
//! and RK3588 device trees) and the Apache-2.0 headers of librga (`im2d_type.h`, `drmrga.h`, at
//! airockchip/librga 2b32edc). The K230D's VGLite: `vg_lite.h`, `vg_lite.c` and
//! `vg_lite_options.h` of the K230 Linux SDK v0.6.1 (MIT; see `lemnos-drivers-vglite`).
//! Nothing here has been measured on a board: every exactness is [`Exactness::Unmeasured`]
//! until a board run records it.

use crate::format::{Layout, PixelFormat};
use crate::resize::Filter;

/// A target SoC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Soc {
    Rk3566,
    Rk3568,
    Rk3576,
    Rk3588,
    Rk3562,
    /// Kendryte K230D: VGLite 2.5D GPU (`lemnos-drivers-vglite`) and an Image 2D engine with no
    /// Linux driver.
    K230d,
}

impl Soc {
    pub const ALL: [Soc; 6] = [
        Soc::Rk3566,
        Soc::Rk3568,
        Soc::Rk3576,
        Soc::Rk3588,
        Soc::Rk3562,
        Soc::K230d,
    ];
}

/// A fixed-function 2D unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unit {
    /// Rockchip RGA2 (the mainline driver's `rk3288-rga` compatible).
    Rga2,
    /// Rockchip RGA3 (RK3588 has two cores).
    Rga3,
    /// K230D Image 2D engine.
    K230Image2d,
    /// K230D VGLite 2.5D GPU. Untested on board: the row is implemented for the ported
    /// operations, and lines and path fill wait for the stroke port.
    K230Vglite,
}

/// How a unit is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Route {
    /// The mainline V4L2 mem2mem driver ([`crate::Rga`]).
    V4l2,
    /// The vendor kernel's multi-RGA driver, `/dev/rga` ([`crate::MultiRga`]): the fill, mask
    /// ROP, blend, palette, colour key, flip and turn operations, and gray input.
    MultiRga,
    /// The direct `/dev/vg_lite` route of the K230D's GPU (`lemnos-drivers-vglite`). Served for
    /// the operations its rows list: clear, blits (resize, crop, flips and quarter turns, the
    /// blends) and palette. Untested on board. Lines and path fill are not served.
    Vglite,
    /// Not written: the interfaces need a design note first.
    Design,
}

/// How close a unit's result is to the scalar reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exactness {
    Exact,
    /// Measured: no byte differs by more than this.
    Tolerance(u8),
    /// Not measured on a board yet.
    Unmeasured,
}

/// What a route does with one filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterCaps {
    pub filter: Filter,
    /// Downscale, upscale.
    pub down: bool,
    pub up: bool,
    pub exactness: Exactness,
    /// How the unit computes it, and what is known to differ from the reference.
    pub note: &'static str,
}

/// A fixed-function operation other than a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FixedOp {
    /// 8-bit indices to RGBA8 through 256 entries.
    Palette,
    /// Per-channel range test giving a mask.
    ColorKey,
    /// A rectangle, unscaled; the full frame is a copy.
    Crop,
    /// Mirror.
    Flip,
    /// Quarter turns.
    Rotate,
    /// A solid rectangle; boxes are four.
    Fill,
    /// AND, OR, XOR of masks.
    MaskRop,
    /// Porter-Duff SRC, SRC_OVER, DST_OVER.
    Blend,
    /// A one-pixel line between two points.
    Line,
    /// One channel of RGBA8 as one gray byte a pixel, exactly (not luma). Implemented on the
    /// `/dev/rga` route as `MultiRga::channel_to_gray` (RGA2 cores, the user colour-space
    /// conversion), untested on board. No row lists it until the board test
    /// (`tests/multirga.rs`, every byte of each channel) matches, so `route_for` answers `None`
    /// and the caller keeps it on the CPU.
    Channel,
}

/// An operation a caller asks of a route (see [`route_for`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// A pixel-format conversion, with or without a resize.
    Convert,
    /// A resize with a filter.
    Resize(Filter),
    /// One of the other fixed-function operations.
    Fixed(FixedOp),
}

/// When a route copies frames through a staging buffer, beyond the unit's own work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Staging {
    /// Never: the unit reads and writes the caller's memory.
    Never,
    /// Byte buffers go through the driver's buffers, one copy in and one out; dmabufs are
    /// queued as they are.
    BytesOnly,
    /// Byte buffers, and dmabufs that are not physically contiguous or whose rows are not
    /// aligned as the unit needs, go through a contiguous staging buffer; contiguous dmabufs
    /// are mapped in place. Each call reports the bytes it copied.
    UnlessContiguous,
}

/// What a route does with one [`FixedOp`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpCaps {
    pub op: FixedOp,
    pub exactness: Exactness,
    pub note: &'static str,
}

/// One unit on one SoC through one route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitCaps {
    pub soc: Soc,
    pub unit: Unit,
    pub route: Route,
    /// Whether this crate reaches the unit through the route. Rows that are described but not
    /// ported say `false`, and [`route_for`] never returns them. The Vglite row is implemented
    /// for the operations its rows list (untested on board).
    pub implemented: bool,
    /// The smallest frame the driver takes, when known.
    pub min_size: Option<(u32, u32)>,
    /// The largest scale factor either way, when known.
    pub max_scale: Option<u32>,
    /// Bytes the unit moves for an 8-bit luma resize, per byte of luma, in thousandths: 1000
    /// when it takes gray directly, 1500 when luma goes through NV12 with dummy chroma.
    pub luma_cost_permille: u16,
    pub staging: Staging,
    pub filters: &'static [FilterCaps],
    pub ops: &'static [OpCaps],
}

const AREA_RGA2_V4L2: FilterCaps = FilterCaps {
    filter: Filter::Area,
    down: true,
    up: false,
    exactness: Exactness::Unmeasured,
    note: "the driver's down mode, factor (dst << 16) / src + 1; phase and rounding unmeasured",
};
const AREA_RGA3_V4L2: FilterCaps = FilterCaps {
    filter: Filter::Area,
    down: true,
    up: false,
    exactness: Exactness::Unmeasured,
    note: "the driver's down mode, factor from TRM 5.4.7; phase and rounding unmeasured",
};
const BILINEAR_RGA2_V4L2: FilterCaps = FilterCaps {
    filter: Filter::Bilinear,
    down: false,
    up: true,
    exactness: Exactness::Unmeasured,
    note: "upscale only; factor (src - 1) / (dst - 1) aligns corners, not OpenCV's half-pixel \
           centres, so edges and phase are expected to differ",
};
const BILINEAR_RGA3_V4L2: FilterCaps = FilterCaps {
    filter: Filter::Bilinear,
    down: false,
    up: true,
    exactness: Exactness::Unmeasured,
    note: "upscale only; factor src / dst, the driver notes up to half a pixel of extra scale",
};
const CROP_V4L2: OpCaps = OpCaps {
    op: FixedOp::Crop,
    exactness: Exactness::Unmeasured,
    note: "the output queue's crop selection, unscaled",
};
const FLIP_V4L2: OpCaps = OpCaps {
    op: FixedOp::Flip,
    exactness: Exactness::Unmeasured,
    note: "V4L2_CID_HFLIP or VFLIP on the context",
};
const ROTATE_V4L2: OpCaps = OpCaps {
    op: FixedOp::Rotate,
    exactness: Exactness::Unmeasured,
    note: "V4L2_CID_ROTATE 90, 180 or 270 on the context; clockwise is assumed, unmeasured",
};

const FLIP_MULTIRGA: OpCaps = OpCaps {
    op: FixedOp::Flip,
    exactness: Exactness::Unmeasured,
    note: "a mirror mode of the request",
};
const ROTATE_MULTIRGA: OpCaps = OpCaps {
    op: FixedOp::Rotate,
    exactness: Exactness::Unmeasured,
    note: "a quarter-turn mode of the request; clockwise is assumed, unmeasured",
};
const FILL_MULTIRGA: OpCaps = OpCaps {
    op: FixedOp::Fill,
    exactness: Exactness::Unmeasured,
    note: "a solid fill colour of the request; a mask is RGBA8888 at a quarter width, so its \
           rectangles align to 4 bytes",
};
const ROP_MULTIRGA: OpCaps = OpCaps {
    op: FixedOp::MaskRop,
    exactness: Exactness::Unmeasured,
    note: "the request's ROP codes on masks as RGBA8888 at a quarter width; the XOR code's \
           meaning as plain XOR is unmeasured",
};
const BLEND_MULTIRGA: OpCaps = OpCaps {
    op: FixedOp::Blend,
    exactness: Exactness::Unmeasured,
    note: "the request's Porter-Duff modes; the hardware's rounding is undocumented",
};

/// Mainline (Linux 7.2): no fill (it stores `V4L2_CID_BG_COLOR` and never programs it), no ROP,
/// no blend, no palette or colour key.
const V4L2_OPS: &[OpCaps] = &[CROP_V4L2, FLIP_V4L2, ROTATE_V4L2];
const RGA3_MULTIRGA_OPS: &[OpCaps] = &[FLIP_MULTIRGA, ROTATE_MULTIRGA, BLEND_MULTIRGA];

const RGA2_MULTIRGA_OPS: &[OpCaps] = &[
    FLIP_MULTIRGA,
    ROTATE_MULTIRGA,
    FILL_MULTIRGA,
    ROP_MULTIRGA,
    BLEND_MULTIRGA,
    OpCaps {
        op: FixedOp::Palette,
        exactness: Exactness::Unmeasured,
        note: "8-bit indices through a 256x1 RGBA8888 table",
    },
    OpCaps {
        op: FixedOp::ColorKey,
        exactness: Exactness::Unmeasured,
        note: "a range test drawn opaque onto a cleared RGBA8888 destination, whose alpha is \
               the mask; the inverted mode keeping in-range pixels is from its name only",
    },
];

const RGA2_V4L2: &[FilterCaps] = &[AREA_RGA2_V4L2, BILINEAR_RGA2_V4L2];
const RGA3_V4L2: &[FilterCaps] = &[AREA_RGA3_V4L2, BILINEAR_RGA3_V4L2];

const fn v4l2(soc: Soc, unit: Unit) -> UnitCaps {
    let rga3 = matches!(unit, Unit::Rga3);
    UnitCaps {
        soc,
        unit,
        route: Route::V4l2,
        implemented: true,
        min_size: Some(if rga3 { (128, 128) } else { (34, 34) }),
        max_scale: Some(if rga3 { 8 } else { 16 }),
        luma_cost_permille: 1500,
        staging: Staging::BytesOnly,
        filters: if rga3 { RGA3_V4L2 } else { RGA2_V4L2 },
        ops: V4L2_OPS,
    }
}

/// The `/dev/rga` row. It serves no resize: the multi-RGA filters are not in this route.
const fn multirga(soc: Soc, unit: Unit) -> UnitCaps {
    UnitCaps {
        soc,
        unit,
        route: Route::MultiRga,
        implemented: true,
        min_size: None,
        max_scale: None,
        luma_cost_permille: 1000,
        staging: Staging::Never,
        filters: &[],
        // Palette and colour key are RGA2 features.
        ops: if matches!(unit, Unit::Rga2) {
            RGA2_MULTIRGA_OPS
        } else {
            RGA3_MULTIRGA_OPS
        },
    }
}

const fn design(soc: Soc, unit: Unit) -> UnitCaps {
    UnitCaps {
        soc,
        unit,
        route: Route::Design,
        implemented: false,
        min_size: None,
        max_scale: None,
        luma_cost_permille: 1000,
        staging: Staging::Never,
        filters: &[],
        ops: &[],
    }
}

const VGLITE_FILTERS: &[FilterCaps] = &[
    FilterCaps {
        filter: Filter::Area,
        down: true,
        up: false,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. 2x only: a bilinear blit at half scale, whose half-pixel phase puts each output \
               centre on the corner of four source pixels (weights of a quarter each); VGLite \
               has no averaging filter, and its rounding is unmeasured",
    },
    FilterCaps {
        filter: Filter::Bilinear,
        down: true,
        up: true,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a scale matrix with the bilinear filter, sampled at the half-pixel phase as \
               OpenCV; weights, rounding and edges unmeasured",
    },
];

const VGLITE_OPS: &[OpCaps] = &[
    OpCaps {
        op: FixedOp::Crop,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a translated, point-sampled blit",
    },
    OpCaps {
        op: FixedOp::Flip,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a mirror matrix, point sampled (gcFEATURE_VG_MIRROR is 0)",
    },
    OpCaps {
        op: FixedOp::Rotate,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a quarter-turn matrix, point sampled",
    },
    OpCaps {
        op: FixedOp::Fill,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a clear; masks as A8 (an L8 target converts the colour with a truncating float \
               sum, so it is refused)",
    },
    OpCaps {
        op: FixedOp::Blend,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. a blit with no blend, SRC_OVER or DST_OVER, straight alpha: the K230 build \
               premultiplies every RGBA source itself, so premultiplied input is refused",
    },
    OpCaps {
        op: FixedOp::Palette,
        exactness: Exactness::Unmeasured,
        note: "Untested on board. an INDEX_8 source through a 256-entry colour table",
    },
];

const fn vglite(soc: Soc) -> UnitCaps {
    UnitCaps {
        soc,
        unit: Unit::K230Vglite,
        route: Route::Vglite,
        // Phases 1 and 2 of `lemnos-drivers-vglite` (untested on board): blits and fills, then
        // lines. Lines are not served by this row: the GPU's stroke is not shown to match
        // Bresenham on board, so `FixedOp::Line` stays on the CPU. `lemnos-drivers-vglite`'s
        // `line_bytes` and `line_dmabuf` still draw on the GPU when called directly. To serve
        // lines here, add the `OpCaps` back with `Exactness::Unmeasured` once the device test
        // `tests/lines.rs` (`gpu_lines_against_bresenham_by_slope_class`) reports zero mismatches
        // for every slope class on a K230.
        implemented: true,
        min_size: None,
        max_scale: None,
        luma_cost_permille: 1000,
        staging: Staging::UnlessContiguous,
        filters: VGLITE_FILTERS,
        ops: VGLITE_OPS,
    }
}

static TABLE: [UnitCaps; 12] = [
    v4l2(Soc::Rk3566, Unit::Rga2),
    multirga(Soc::Rk3566, Unit::Rga2),
    v4l2(Soc::Rk3568, Unit::Rga2),
    multirga(Soc::Rk3568, Unit::Rga2),
    // No RGA node in the mainline RK3576 and RK3562 device trees (Linux 7.2).
    multirga(Soc::Rk3576, Unit::Rga2),
    multirga(Soc::Rk3562, Unit::Rga2),
    v4l2(Soc::Rk3588, Unit::Rga3),
    v4l2(Soc::Rk3588, Unit::Rga2),
    multirga(Soc::Rk3588, Unit::Rga3),
    multirga(Soc::Rk3588, Unit::Rga2),
    vglite(Soc::K230d),
    // No Linux driver in the K230 SDK; the RT-Smart side may own it.
    design(Soc::K230d, Unit::K230Image2d),
];

/// Every unit and route on `soc`, preferred first (mainline before the multi-RGA driver, RGA3
/// before RGA2, VGLite before the K230D's undriven Image 2D engine).
pub fn caps(soc: Soc) -> impl Iterator<Item = &'static UnitCaps> {
    TABLE.iter().filter(move |c| c.soc == soc)
}

/// The route that would serve `op` from `src` to `dst` on `soc`, or `None` when no implemented
/// route can: the caller then sends the work to the CPU. Looks only at the table: no device is
/// probed (see `route_on_system` for that). The formats and sizes of each side are checked
/// against the unit's rules; a route refuses anything else as `Unsupported` when it runs.
pub fn route_for(soc: Soc, op: Op, src: Layout, dst: Layout) -> Option<Route> {
    caps(soc)
        .filter(|c| c.implemented)
        .find(|c| c.serves(op, src, dst))
        .map(|c| c.route)
}

impl UnitCaps {
    /// What this route does with `op`, if anything.
    pub fn op(&self, op: FixedOp) -> Option<&OpCaps> {
        self.ops.iter().find(|o| o.op == op)
    }

    /// What this route does with `filter`, if anything.
    pub fn filter(&self, filter: Filter) -> Option<&FilterCaps> {
        self.filters.iter().find(|f| f.filter == filter)
    }

    /// Whether this unit serves `op` from `src` to `dst`, by its rules. Only the mainline V4L2
    /// route has rules here; no other route is implemented.
    pub fn serves(&self, op: Op, src: Layout, dst: Layout) -> bool {
        match self.route {
            Route::MultiRga => return self.serves_multirga(op, src, dst),
            Route::Vglite => return self.serves_vglite(op, src, dst),
            Route::V4l2 => {}
            _ => return false,
        }
        // The mainline driver takes no gray frames (the luma goes through NV12 instead).
        let formats = src.format != PixelFormat::Grey && dst.format != PixelFormat::Grey;
        let sizes = [src, dst].into_iter().all(|l| {
            self.min_size
                .is_none_or(|(w, h)| l.width >= w && l.height >= h)
        });
        let scale = self.max_scale.is_none_or(|max| {
            let max = max as u64;
            let ok = |a: u32, b: u32| {
                let (a, b) = (a as u64, b as u64);
                a * max >= b && b * max >= a
            };
            ok(src.width, dst.width) && ok(src.height, dst.height)
        });
        if !(formats && sizes && scale) {
            return false;
        }
        let shrinks = dst.width <= src.width && dst.height <= src.height;
        let grows = dst.width >= src.width && dst.height >= src.height;
        match op {
            Op::Convert => true,
            Op::Resize(filter) => {
                let Some(f) = self.filter(filter) else {
                    return false;
                };
                let same = (src.width, src.height) == (dst.width, dst.height);
                !same && ((shrinks && f.down) || (grows && f.up))
            }
            Op::Fixed(op) => self.op(op).is_some(),
        }
    }
}

impl UnitCaps {
    /// The multi-RGA rules: gray input to an RGBA-class destination of the same size (Y-only
    /// output is not served), no resize, and the fixed operations its rows list. Its formats
    /// are the request's own, checked when it runs.
    fn serves_multirga(&self, op: Op, src: Layout, dst: Layout) -> bool {
        match op {
            Op::Convert => {
                src.format == PixelFormat::Grey
                    && matches!(dst.format, PixelFormat::Abgr32 | PixelFormat::Xbgr32)
                    && (src.width, src.height) == (dst.width, dst.height)
            }
            Op::Resize(_) => false,
            Op::Fixed(op) => self.op(op).is_some(),
        }
    }
}

impl UnitCaps {
    /// The VGLite rules, untested on board: the rows it lists. A resize is a bilinear scale (down
    /// or up, per its filter row) or the 2x area downscale that the half-phase blit serves, one
    /// format at a time and never to or from gray. The other fixed operations need only their
    /// row; the driver checks sizes and formats when it runs.
    fn serves_vglite(&self, op: Op, src: Layout, dst: Layout) -> bool {
        match op {
            Op::Convert => false,
            Op::Resize(filter) => {
                let Some(f) = self.filter(filter) else {
                    return false;
                };
                if src.format != dst.format || src.format == PixelFormat::Grey {
                    return false;
                }
                let shrinks = dst.width <= src.width && dst.height <= src.height;
                let grows = dst.width >= src.width && dst.height >= src.height;
                let halved = |s: u32, d: u32| s >= 2 && d == s / 2;
                match filter {
                    Filter::Area => halved(src.width, dst.width) && halved(src.height, dst.height),
                    Filter::Bilinear => (shrinks && f.down) || (grows && f.up),
                }
            }
            Op::Fixed(op) => self.op(op).is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(format: PixelFormat, w: u32, h: u32) -> Layout {
        Layout::packed(format, w, h)
    }

    #[test]
    fn every_soc_is_described_and_nothing_is_measured_yet() {
        for soc in Soc::ALL {
            assert!(caps(soc).next().is_some(), "{soc:?}");
        }
        for c in caps(Soc::Rk3588) {
            for f in c.filters {
                assert_eq!(f.exactness, Exactness::Unmeasured);
                assert!(!f.note.is_empty());
            }
        }
        let first = caps(Soc::Rk3588).next().unwrap();
        assert_eq!((first.unit, first.route), (Unit::Rga3, Route::V4l2));
        let area = first.filter(Filter::Area).unwrap();
        assert!(area.down && !area.up);
        let k230: Vec<_> = caps(Soc::K230d).collect();
        assert_eq!(
            (k230[0].unit, k230[0].route),
            (Unit::K230Vglite, Route::Vglite)
        );
        assert_eq!(k230[0].staging, Staging::UnlessContiguous);
        // Lines wait for a board match against Bresenham: see the comment in `vglite`.
        assert!(k230[0].op(FixedOp::Line).is_none() && k230[0].op(FixedOp::Blend).is_some());
        assert!(k230[0].op(FixedOp::ColorKey).is_none());
        assert!(k230[1].route == Route::Design && k230[1].filters.is_empty());
        assert!(caps(Soc::Rk3576).all(|c| c.route == Route::MultiRga));
        for c in caps(Soc::Rk3588).filter(|c| c.route == Route::MultiRga) {
            assert!(c.filters.is_empty(), "{c:?}");
            assert!(c.op(FixedOp::Crop).is_none(), "{c:?}");
        }
        // Palette and colour key: the multi-RGA RGA2 rows only.
        for c in Soc::ALL
            .into_iter()
            .flat_map(caps)
            .filter(|c| c.route == Route::MultiRga || c.route == Route::V4l2)
        {
            let has = c.op(FixedOp::Palette).is_some();
            assert_eq!(
                has,
                c.route == Route::MultiRga && c.unit == Unit::Rga2,
                "{c:?}"
            );
            assert_eq!(c.op(FixedOp::Fill).is_some(), has, "{c:?}");
            assert_eq!(c.op(FixedOp::ColorKey).is_some(), has, "{c:?}");
        }
    }

    #[test]
    fn the_mainline_and_multi_rga_routes_are_implemented_and_nothing_else() {
        for c in Soc::ALL.into_iter().flat_map(caps) {
            let want = matches!(c.route, Route::V4l2 | Route::MultiRga | Route::Vglite);
            assert_eq!(c.implemented, want, "{c:?}");
        }
    }

    #[test]
    fn route_for_answers_the_cells_and_sends_the_rest_on() {
        let nv12 = layout(PixelFormat::Nv12, 1280, 720);
        let abgr = layout(PixelFormat::Abgr32, 640, 360);
        // A 2x area downscale: the mainline driver takes it.
        let area = Op::Resize(Filter::Area);
        assert_eq!(route_for(Soc::Rk3566, area, nv12, abgr), Some(Route::V4l2));
        // Bilinear downscale: no route has a filter for it.
        let bilinear = Op::Resize(Filter::Bilinear);
        assert_eq!(route_for(Soc::Rk3566, bilinear, nv12, abgr), None);
        // Gray input goes to the multi-RGA node (same-size, RGBA-class output).
        let gray = layout(PixelFormat::Grey, 640, 360);
        assert_eq!(
            route_for(Soc::Rk3566, Op::Convert, gray, abgr),
            Some(Route::MultiRga)
        );
        assert_eq!(
            route_for(
                Soc::Rk3566,
                Op::Convert,
                gray,
                layout(PixelFormat::Nv12, 640, 360)
            ),
            None
        );
        // Fill, ROP, palette and colour key: the multi-RGA RGA2 row; RGA3 has blend only.
        for op in [
            FixedOp::Fill,
            FixedOp::MaskRop,
            FixedOp::Palette,
            FixedOp::ColorKey,
        ] {
            assert_eq!(
                route_for(Soc::Rk3588, Op::Fixed(op), abgr, abgr),
                Some(Route::MultiRga),
                "{op:?}"
            );
        }
        assert_eq!(
            route_for(Soc::Rk3588, Op::Fixed(FixedOp::Blend), abgr, abgr),
            Some(Route::MultiRga)
        );
        assert_eq!(
            route_for(Soc::Rk3588, Op::Fixed(FixedOp::Line), abgr, abgr),
            None
        );
        // A channel to gray waits for its board test on every SoC (see `FixedOp::Channel`).
        let grey = layout(PixelFormat::Grey, 640, 360);
        for soc in Soc::ALL {
            assert_eq!(
                route_for(soc, Op::Fixed(FixedOp::Channel), abgr, grey),
                None
            );
        }
        // Flips on the mainline driver; a frame below the RGA3 minimum goes to the RGA2 row.
        assert_eq!(
            route_for(Soc::Rk3588, Op::Fixed(FixedOp::Flip), abgr, abgr),
            Some(Route::V4l2)
        );
        let small = layout(PixelFormat::Abgr32, 64, 64);
        assert_eq!(
            route_for(Soc::Rk3588, Op::Convert, small, small),
            Some(Route::V4l2)
        );
        // The K230D's VGLite serves its rows: a fill, a 2x area downscale, a bilinear resize.
        // Lines are not in its rows (see `vglite`); a 3x area downscale is not a half-phase blit.
        assert_eq!(
            route_for(Soc::K230d, Op::Fixed(FixedOp::Fill), abgr, abgr),
            Some(Route::Vglite)
        );
        assert_eq!(
            route_for(Soc::K230d, Op::Fixed(FixedOp::Line), abgr, abgr),
            None
        );
        let big = layout(PixelFormat::Abgr32, 640, 360);
        let small = layout(PixelFormat::Abgr32, 320, 180);
        assert_eq!(
            route_for(Soc::K230d, Op::Resize(Filter::Area), big, small),
            Some(Route::Vglite)
        );
        let third = layout(PixelFormat::Abgr32, 213, 120);
        assert_eq!(
            route_for(Soc::K230d, Op::Resize(Filter::Area), big, third),
            None
        );
        assert_eq!(
            route_for(Soc::K230d, Op::Resize(Filter::Bilinear), big, third),
            Some(Route::Vglite)
        );
        assert_eq!(
            route_for(Soc::K230d, Op::Fixed(FixedOp::Palette), abgr, abgr),
            Some(Route::Vglite)
        );
    }
}
