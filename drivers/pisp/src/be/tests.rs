use core::mem::offset_of;

use super::*;
use crate::format::formats;

/// Each phase passes the centre tap: a valid filter with no filter design in it.
const TEST_FILTER: [i16; BE_RESAMPLE_FILTER_SIZE] = {
    let mut c = [0; BE_RESAMPLE_FILTER_SIZE];
    let mut p = 0;
    while p < 16 {
        c[p * 6 + 2] = 1024;
        p += 1;
    }
    c
};

/// A valid RGB-to-YCbCr matrix, standing in for whatever the caller would choose.
const TEST_CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33554432, 33554432],
};

fn bayer16(w: u16, h: u16) -> ImageFormatConfig {
    let mut f = ImageFormatConfig {
        width: w,
        height: h,
        format: formats::BAYER16,
        ..Default::default()
    };
    crate::format::compute_stride_align(&mut f, 64);
    f
}

fn simple(format: u32) -> BackEnd {
    BackEnd::simple_bayer(
        bayer16(1280, 800),
        BayerOrder::Bggr,
        4096,
        (1.5, 1.0, 1.8),
        None,
        format,
        (format != formats::RGB888).then_some(TEST_CSC),
    )
}

#[test]
fn nv12_full_size() {
    let mut be = simple(formats::NV12);
    let t = be.prepare().unwrap();
    let c = &t.config;
    assert_eq!(t.num_tiles, 3);
    let o = c.output_format[0];
    assert_eq!((o.image.width, o.image.height), (1280, 800));
    assert_eq!((o.image.stride, o.image.stride2), (1280, 1280));
    assert_eq!((o.hi, o.hi2), (65535, 65535));
    assert_ne!(c.global.rgb_enables & rgb_enable::CSC0, 0);
    assert_eq!(c.global.rgb_enables & rgb_enable::RESAMPLE0, 0);
    assert_eq!(c.wbg.gain_r, 1536);
    assert_eq!(c.blc.black_level_gr, 4096);
    // Tiles cover the output exactly once, left to right.
    let mut x = 0;
    for tile in &t.tiles[..3] {
        assert_eq!(tile.output_offset_x[0], x);
        assert_eq!(
            tile.input_width - tile.crop_x_start[0] - tile.crop_x_end[0],
            tile.output_width[0]
        );
        assert_eq!(tile.output_addr_offset[0], u32::from(x));
        assert_eq!(tile.input_addr_offset, u32::from(tile.input_offset_x) * 2);
        assert!(tile.input_width <= 640);
        x += tile.output_width[0];
    }
    assert_eq!(x, 1280);
    assert_eq!(
        t.tiles[0].edge,
        TILE_LEFT_EDGE | TILE_TOP_EDGE | TILE_BOTTOM_EDGE
    );
    assert_eq!(
        t.tiles[2].edge,
        TILE_RIGHT_EDGE | TILE_TOP_EDGE | TILE_BOTTOM_EDGE
    );
    let mut bytes = [0u8; BeTilesConfig::SIZE];
    assert_eq!(t.write_to(&mut bytes), Ok(16720));
    // The tile count is the last field; the first tile's edge flags follow the config.
    assert_eq!(bytes[16716..], 3u32.to_le_bytes());
    assert_eq!(
        bytes[offset_of!(BeTilesConfig, tiles)],
        TILE_LEFT_EDGE | TILE_TOP_EDGE | TILE_BOTTOM_EDGE
    );
}

#[test]
fn rgb_output_has_no_csc() {
    let mut be = simple(formats::RGB888);
    let t = be.prepare().unwrap();
    assert_eq!(t.config.global.rgb_enables & rgb_enable::CSC0, 0);
    assert_eq!(t.config.output_format[0].image.stride, 3840);
}

#[test]
fn smart_resize_uses_downscaler_on_output1() {
    let mut be = simple(formats::NV12);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables | rgb_enable::OUTPUT1,
        BayerOrder::Bggr,
    );
    be.set_output_format(
        Branch::One,
        BeOutputFormatConfig {
            image: ImageFormatConfig {
                format: formats::YUV420P,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    be.set_smart_resize(Branch::One, 320, 200);
    be.set_resample_filter(Branch::One, &TEST_FILTER);
    be.set_smart_resize(Branch::Zero, 960, 600);
    be.set_resample_filter(Branch::Zero, &TEST_FILTER);
    let t = be.prepare().unwrap();
    let c = &t.config;
    assert_ne!(c.global.rgb_enables & rgb_enable::DOWNSCALE1, 0);
    assert_ne!(c.global.rgb_enables & rgb_enable::RESAMPLE1, 0);
    assert_ne!(c.global.rgb_enables & rgb_enable::RESAMPLE0, 0);
    assert_eq!(c.global.rgb_enables & rgb_enable::DOWNSCALE0, 0);
    assert_eq!(c.downscale[1].scale_factor_h, 2 << 12);
    let o1 = c.output_format[1].image;
    assert_eq!((o1.width, o1.height), (320, 200));
    let (mut w0, mut w1) = (0, 0);
    for tile in &t.tiles[..t.num_tiles as usize] {
        if tile.output_offset_y[0] == 0 {
            w0 += tile.output_width[0];
        }
        if tile.output_offset_y[1] == 0 {
            w1 += tile.output_width[1];
        }
    }
    assert_eq!((w0, w1), (960, 320));
}

#[test]
fn bad_configs_are_rejected() {
    let mut be = simple(formats::NV12);
    be.set_global(0, rgb_enable::OUTPUT0, BayerOrder::Bggr);
    assert!(be.prepare().is_err());
    let mut be = simple(formats::NV12);
    be.set_input_format(bayer16(1279, 800));
    assert!(be.prepare().is_err());
    let mut be = simple(formats::NV12);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables & !rgb_enable::OUTPUT0,
        BayerOrder::Bggr,
    );
    assert!(be.prepare().is_err());
}

#[test]
fn enabled_blocks_with_empty_tables_are_refused() {
    for (bit, what) in [
        (rgb_enable::GAMMA, "gamma enabled with no curve"),
        (rgb_enable::YCBCR, "YCbCr enabled with no matrix"),
        (
            rgb_enable::YCBCR_INVERSE,
            "inverse YCbCr enabled with no matrix",
        ),
    ] {
        let mut be = simple(formats::RGB888);
        let g = be.config().global;
        be.set_global(g.bayer_enables, g.rgb_enables | bit, BayerOrder::Bggr);
        assert_eq!(be.prepare().unwrap_err(), PrepareError::Config(what));
    }
    let mut be = simple(formats::RGB888);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables | rgb_enable::CSC0,
        BayerOrder::Bggr,
    );
    assert_eq!(
        be.prepare().unwrap_err(),
        PrepareError::Output(0, "CSC enabled with no matrix")
    );
    let mut be = simple(formats::RGB888);
    be.config_mut().ccm = BeCcmConfig::default();
    assert_eq!(
        be.prepare().unwrap_err(),
        PrepareError::Config("CCM enabled with no matrix")
    );
}

#[test]
fn a_crop_must_lie_inside_the_input() {
    for crop in [
        BeCropConfig {
            offset_x: 1200,
            offset_y: 0,
            width: 160,
            height: 100,
        },
        BeCropConfig {
            offset_x: 0,
            offset_y: 0,
            width: 160,
            height: 0,
        },
    ] {
        let mut be = simple(formats::RGB888);
        be.set_crop(Branch::Zero, crop);
        assert_eq!(
            be.prepare().unwrap_err(),
            PrepareError::Output(0, "crop must lie inside the input")
        );
    }
}

#[test]
fn a_fixed_step_past_the_input_is_refused() {
    let mut be = simple(formats::RGB888);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables | rgb_enable::RESAMPLE0,
        BayerOrder::Bggr,
    );
    be.set_resample(
        Branch::Zero,
        BeResampleExtra {
            scaled_width: 1280,
            scaled_height: 800,
            ..Default::default()
        },
    );
    be.set_resample_filter(Branch::Zero, &TEST_FILTER);
    be.set_resample_step(Branch::Zero, Some((2 << 12, 2 << 12)));
    assert_eq!(
        be.prepare().unwrap_err(),
        PrepareError::Output(0, "resampler step runs past the end of the input")
    );
}

#[test]
fn extreme_sizes_are_refused_or_tile_correctly() {
    let dims = [0u16, 2, 14, 16, 18, 638, 640, 642, 4096, 32768, 65534];
    let (mut tiled, mut refused) = (0, 0);
    for &w in &dims {
        for &h in &dims {
            for out in [(w, h), (w / 2, h / 2), (2, 2), (65534, 65534)] {
                let mut be = simple(formats::RGB888);
                be.set_input_format(bayer16(w, h));
                be.set_smart_resize(Branch::Zero, out.0, out.1);
                be.set_resample_filter(Branch::Zero, &TEST_FILTER);
                match be.prepare() {
                    Ok(t) => {
                        assert!(w >= 16 && h >= 16, "{w}x{h} prepared");
                        let n = t.num_tiles as usize;
                        assert!((1..=BE_NUM_TILES).contains(&n), "{n} tiles");
                        let img = t.config.output_format[0].image;
                        assert_eq!((img.width, img.height), out);
                        // The top row of tiles writes every column of the output once.
                        let across: u32 = t.tiles[..n]
                            .iter()
                            .filter(|t| t.output_offset_y[0] == 0)
                            .map(|t| u32::from(t.output_width[0]))
                            .sum();
                        assert_eq!(across, u32::from(out.0), "{w}x{h} to {out:?}");
                        tiled += 1;
                    }
                    Err(_) => refused += 1,
                }
            }
        }
    }
    // Both outcomes must have happened, or the sizes above tested nothing.
    assert!(tiled > 20, "only {tiled} tiled");
    assert!(refused > 100, "only {refused} refused");
}

#[test]
fn enormous_strides_wrap_rather_than_overflow() {
    for stride in [0x7fff_fff0, -16, -0x4000_0000] {
        let mut be = simple(formats::RGB888);
        let mut input = bayer16(1280, 800);
        input.stride = stride;
        be.set_input_format(input);
        // A stride only reaches the tile offsets; the configuration itself is legal here.
        let t = be.prepare().unwrap();
        let tile = t.tiles[0];
        assert_eq!(tile.input_addr_offset, 0);
        assert_eq!(t.config.input_format.stride, stride);
    }
}

#[test]
fn mirrored_outputs_write_from_the_far_edge() {
    let mut be = simple(formats::RGB888);
    let g = be.config().global;
    be.set_global(g.bayer_enables, g.rgb_enables, BayerOrder::Bggr);
    let mut f = be.config().output_format[0];
    f.transform = BE_TRANSFORM_HFLIP | BE_TRANSFORM_VFLIP;
    be.set_output_format(Branch::Zero, f);
    let t = be.prepare().unwrap();
    let tiles = &t.tiles[..t.num_tiles as usize];
    assert_eq!(tiles[0].output_offset_x[0], 1280 - tiles[0].output_width[0]);
    assert_eq!(tiles[0].output_offset_y[0], 799);
    let mut covered = 0u32;
    for tile in tiles {
        let end = u32::from(tile.output_offset_x[0]) + u32::from(tile.output_width[0]);
        assert_eq!(end, 1280 - covered);
        covered += u32::from(tile.output_width[0]);
    }
    assert_eq!(covered, 1280);
}

#[test]
fn narrow_tiles_when_limited() {
    let mut be = simple(formats::NV12);
    be.set_max_tile_width(256);
    let t = be.prepare().unwrap();
    assert!(t.num_tiles >= 6);
    assert!(
        t.tiles[..t.num_tiles as usize]
            .iter()
            .all(|t| t.input_width <= 256)
    );
}

#[test]
fn per_channel_black_levels_reach_the_blc_and_the_denoise_blocks() {
    let mut be = simple(formats::NV12);
    be.set_black_levels([64, 66, 65, 63]);
    let c = be.config();
    assert_eq!(
        (
            c.blc.black_level_r,
            c.blc.black_level_gr,
            c.blc.black_level_gb,
            c.blc.black_level_b
        ),
        (64, 66, 65, 63)
    );
    assert_eq!(c.blc.output_black_level, 0);
    // The denoise blocks take one level: the mean of the four, rounded (258 / 4 → 65).
    assert_eq!((c.sdn.black_level, c.tdn.black_level), (65, 65));
    // The single-value path is the same as four equal levels.
    let mut one = simple(formats::NV12);
    one.set_black_level(64);
    let mut four = simple(formats::NV12);
    four.set_black_levels([64; 4]);
    assert_eq!(one.config().blc, four.config().blc);
    assert_eq!(one.config().sdn.black_level, four.config().sdn.black_level);
    assert_eq!(one.config().tdn.black_level, 64);
}

#[test]
fn tdn_output_tiles_start_16_byte_aligned() {
    // 1280x800 16-bit Bayer in, NV12 out, TDN on. Every tile writes the TDN
    // average at its own input offset, which libpisp aligns to 16 bytes (8 pixels here); at 2
    // pixels the second tile started at x=494 and the average landed shifted.
    let mut be = simple(formats::NV12);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables | rgb_enable::OUTPUT1,
        BayerOrder::Bggr,
    );
    let nv12 = BeOutputFormatConfig {
        image: ImageFormatConfig {
            format: formats::NV12,
            ..Default::default()
        },
        ..Default::default()
    };
    be.set_output_format(Branch::One, nv12);
    be.set_smart_resize(Branch::One, 640, 400);
    be.set_resample_filter(Branch::One, &TEST_FILTER);
    be.set_tdn_format(bayer16(1280, 800));
    be.set_tdn(Some(BeTdnConfig::default()), true);
    for max in [0, 256, 400] {
        if max > 0 {
            be.set_max_tile_width(max);
        }
        let t = be.prepare().unwrap();
        let tiles = &t.tiles[..t.num_tiles as usize];
        assert!(tiles.len() > 1, "max {max}: one tile");
        for tile in tiles {
            assert_eq!(
                tile.input_offset_x % 8,
                0,
                "max {max}: tile at x={}",
                tile.input_offset_x
            );
            assert_eq!(tile.tdn_output_addr_offset % 16, 0, "max {max}");
            assert_eq!(tile.tdn_input_addr_offset % 16, 0, "max {max}");
        }
    }
}

#[test]
fn half_size_output_keeps_the_callers_filter() {
    let mut be = simple(formats::NV12);
    let g = be.config().global;
    be.set_global(
        g.bayer_enables,
        g.rgb_enables | rgb_enable::OUTPUT1,
        BayerOrder::Bggr,
    );
    let nv12 = BeOutputFormatConfig {
        image: ImageFormatConfig {
            format: formats::NV12,
            ..Default::default()
        },
        ..Default::default()
    };
    be.set_output_format(Branch::One, nv12);
    be.set_smart_resize(Branch::One, 640, 400);
    // No filter yet: refused, rather than a resampler that writes black.
    assert_eq!(
        be.prepare().unwrap_err(),
        PrepareError::Output(1, "resampler enabled with no filter")
    );
    be.set_resample_filter(Branch::One, &TEST_FILTER);
    let t = be.prepare().unwrap();
    assert_eq!(t.config.global.rgb_enables & rgb_enable::DOWNSCALE1, 0);
    assert_ne!(t.config.global.rgb_enables & rgb_enable::RESAMPLE1, 0);
    assert_eq!(t.config.resample[1].coef, TEST_FILTER);
}

#[test]
fn a_large_downscale_computes_its_own_trapezoid() {
    // 4x on output 0, which has no downscaler: the resampler as a trapezoid, no caller filter.
    let mut be = simple(formats::NV12);
    be.set_smart_resize(Branch::Zero, 320, 200);
    let t = be.prepare().unwrap();
    let coef = t.config.resample[0].coef;
    assert_ne!(coef, [0; BE_RESAMPLE_FILTER_SIZE]);
    for phase in coef.chunks(6) {
        let sum: i32 = phase.iter().map(|&c| i32::from(c)).sum();
        assert!((sum - 1024).abs() <= 8, "phase sums to {sum}");
    }
}

#[test]
fn compressed_tdn_sets_its_blocks_and_byte_offsets() {
    // The average kept as libcamera keeps it: 8-bit mode 1, offset 2048, one byte per pixel.
    let mut be = simple(formats::NV12);
    let comp = ImageFormatConfig {
        width: 1280,
        height: 800,
        format: formats::PISP_COMP1,
        stride: 1280,
        stride2: 0,
    };
    be.set_tdn_format(comp);
    be.set_tdn(Some(BeTdnConfig::default()), true);
    be.set_max_tile_width(400);
    let t = be.prepare().unwrap();
    let en = t.config.global.bayer_enables;
    for bit in [
        bayer_enable::TDN_COMPRESS,
        bayer_enable::TDN_DECOMPRESS,
        bayer_enable::TDN_INPUT,
        bayer_enable::TDN_OUTPUT,
    ] {
        assert_ne!(en & bit, 0, "{bit:#x}");
    }
    assert_eq!(
        (t.config.tdn_compress.offset, t.config.tdn_compress.mode),
        (2048, 1)
    );
    assert_eq!(
        (t.config.tdn_decompress.offset, t.config.tdn_decompress.mode),
        (2048, 1)
    );
    for tile in &t.tiles[..t.num_tiles as usize] {
        assert_eq!(tile.input_offset_x % 8, 0);
        assert_eq!(tile.tdn_output_addr_offset, u32::from(tile.input_offset_x));
    }
    // Without input (after a reset) nothing is decompressed.
    be.set_tdn(Some(BeTdnConfig::default()), false);
    let en = be.prepare().unwrap().config.global.bayer_enables;
    assert_eq!(en & bayer_enable::TDN_DECOMPRESS, 0);
    assert_ne!(en & bayer_enable::TDN_COMPRESS, 0);
}

#[test]
fn striped_tiles_split_at_a_line_into_two_jobs() {
    // 1280x800 with 576-row stripes, split where the line event says rows 0..600 are in.
    let mut be = simple(formats::NV12);
    be.set_max_stripe_height(576);
    let t = be.prepare().unwrap();
    let all = &t.tiles[..t.num_tiles as usize];
    assert!(all.iter().any(|x| x.input_offset_y > 0), "no second stripe");
    let s = split_tiles(&t, 600).unwrap();
    let (a, b) = (
        &s.first.tiles[..s.first.num_tiles as usize],
        &s.rest.tiles[..s.rest.num_tiles as usize],
    );
    assert_eq!(a.len() + b.len(), all.len());
    assert!(
        a.iter()
            .all(|x| u32::from(x.input_offset_y) + u32::from(x.input_height) <= 600)
    );
    assert!(
        b.iter()
            .all(|x| u32::from(x.input_offset_y) + u32::from(x.input_height) > 600)
    );
    // Output 0's rows above the second stripe are final after the first job; every output row
    // is written by exactly one of the two.
    // (The tiler balances stripes, so the boundary sits at or above the 576 limit.)
    let first_rest_row = b
        .iter()
        .map(|x| u32::from(x.output_offset_y[0]))
        .min()
        .unwrap();
    assert_eq!(s.rows_after_first[0], first_rest_row);
    assert!(
        (400..=576).contains(&s.rows_after_first[0]),
        "{}",
        s.rows_after_first[0]
    );
    let rows = |ts: &[Tile]| -> u32 {
        ts.iter()
            .map(|x| u32::from(x.output_width[0]) * u32::from(x.output_height[0]))
            .sum()
    };
    assert_eq!(rows(a) + rows(b), 1280 * 800);
    // Above the first stripe's input, or below the frame: one job.
    assert!(split_tiles(&t, 100).is_none());
    assert!(split_tiles(&t, 900).is_none());
}

#[test]
fn a_tile_count_beyond_the_array_is_not_split() {
    let mut be = simple(formats::NV12);
    be.set_max_stripe_height(576);
    let mut t = be.prepare().unwrap();
    t.num_tiles = BE_NUM_TILES as u32 + 1;
    assert!(split_tiles(&t, 600).is_none());
    t.num_tiles = u32::MAX;
    let (mut first, mut rest) = (t, t);
    assert!(split_tiles_into(&t, 600, &mut first, &mut rest).is_none());
}

#[test]
fn split_into_reused_buffers_equal_split_tiles() {
    // The pipeline splits every sliced frame into the same two buffers: each result must equal
    // a fresh `split_tiles`, including after a split that did not fit (the buffers then hold
    // whatever the last one wrote).
    let mut be = simple(formats::NV12);
    be.set_max_stripe_height(576);
    let t = be.prepare().unwrap();
    let (mut first, mut rest) = (t, t);
    for line in [600, 100, 900, 400, 600] {
        match (
            split_tiles(&t, line),
            split_tiles_into(&t, line, &mut first, &mut rest),
        ) {
            (Some(want), Some(rows)) => {
                assert_eq!(want.first, first, "line {line}");
                assert_eq!(want.rest, rest, "line {line}");
                assert_eq!(want.rows_after_first, rows, "line {line}");
            }
            (None, None) => {}
            (want, got) => panic!("line {line}: {:?} vs {:?}", want.is_some(), got),
        }
    }
}
