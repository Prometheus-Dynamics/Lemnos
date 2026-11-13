//! Per-block builders: each block's enable bit follows its setter, the libpisp defaults are the
//! values a caller gets by enabling a block with them, a YUV output converts by default, and the
//! refusals of out-of-range settings hold.

use super::*;
use crate::format::formats;
use crate::uapi::{BayerOrder, BeCcmConfig, Wire};

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

fn bits(be: &BackEnd) -> (u32, u32) {
    let g = be.config().global;
    (g.bayer_enables, g.rgb_enables)
}

/// The wire bytes of a matrix, as the module reads them.
fn bytes(m: &BeCcmConfig) -> [u8; BeCcmConfig::SIZE] {
    let mut out = [0u8; BeCcmConfig::SIZE];
    m.write_to(&mut out).unwrap();
    out
}

/// The RGB to YCbCr matrix of full-range BT.601, derived from its documented luma coefficients
/// (Kr 0.299, Kg 0.587, Kb 0.114) rather than copied from a table.
fn documented_bt601_full() -> BeCcmConfig {
    let (kr, kg, kb) = (0.299f64, 0.587f64, 0.114f64);
    let m = [
        kr,
        kg,
        kb,
        -kr / (2.0 * (1.0 - kb)),
        -kg / (2.0 * (1.0 - kb)),
        0.5,
        0.5,
        -kg / (2.0 * (1.0 - kr)),
        -kb / (2.0 * (1.0 - kr)),
    ];
    BeCcmConfig {
        coeffs: m.map(|v| (v * 1024.0).round() as i16),
        pad: [0; 2],
        // Chroma offset 128 in 8-bit units, in 2^-18 units.
        offsets: [0, 128 << 18, 128 << 18],
    }
}

#[test]
fn a_default_nv12_output_converts_with_full_range_bt601() {
    let mut be = BackEnd::simple_bayer(
        bayer16(1280, 800),
        BayerOrder::Bggr,
        4096,
        (1.5, 1.0, 1.8),
        None,
        formats::NV12,
        None,
    );
    assert_ne!(bits(&be).1 & rgb_enable::csc(0), 0);
    let t = be.prepare().unwrap();
    assert_eq!(
        bytes(&t.config.csc[0]),
        bytes(&documented_bt601_full()),
        "output 0's CSC is the documented BT.601 matrix"
    );
}

#[test]
fn a_caller_matrix_replaces_the_default_and_rgb_has_none() {
    let mine = Encoding::Rec709.forward();
    let be = BackEnd::simple_bayer(
        bayer16(1280, 800),
        BayerOrder::Bggr,
        0,
        (1.0, 1.0, 1.0),
        None,
        formats::NV12,
        Some(mine),
    );
    assert_eq!(be.config().csc[0], mine);
    let rgb = BackEnd::simple_bayer(
        bayer16(1280, 800),
        BayerOrder::Bggr,
        0,
        (1.0, 1.0, 1.0),
        None,
        formats::RGB888,
        None,
    );
    assert_eq!(bits(&rgb).1 & rgb_enable::csc(0), 0);
}

#[test]
fn a_new_back_end_carries_the_libpisp_defaults_and_enables_none_of_them() {
    let be = BackEnd::new();
    assert_eq!(be.config().debin, defaults::DEBIN_DEFAULT);
    assert_eq!(be.config().demosaic, defaults::DEMOSAIC_DEFAULT);
    assert_eq!(be.config().false_colour, defaults::FALSE_COLOUR_DEFAULT);
    assert_eq!(be.config().sh_fc_combine.y_factor, 192);
    assert_eq!(bits(&be), (0, 0));
}

#[test]
fn ycbcr_sets_both_matrices_and_both_enables_together() {
    let mut be = BackEnd::new();
    be.set_ycbcr_encoding(Some(Encoding::Bt2020));
    assert_eq!(be.config().ycbcr, Encoding::Bt2020.forward());
    assert_eq!(be.config().ycbcr_inverse, Encoding::Bt2020.inverse());
    let r = bits(&be).1;
    assert_eq!(
        r & (rgb_enable::YCBCR | rgb_enable::YCBCR_INVERSE),
        rgb_enable::YCBCR | rgb_enable::YCBCR_INVERSE
    );
    be.set_ycbcr(None);
    assert_eq!(
        bits(&be).1 & (rgb_enable::YCBCR | rgb_enable::YCBCR_INVERSE),
        0
    );
}

#[test]
fn each_output_colour_space_conversion_has_its_own_enable() {
    let mut be = BackEnd::new();
    be.set_csc_encoding(Branch::One, Encoding::Rec709);
    assert_eq!(be.config().csc[1], Encoding::Rec709.forward());
    assert_ne!(bits(&be).1 & rgb_enable::csc(1), 0);
    assert_eq!(bits(&be).1 & rgb_enable::csc(0), 0);
    be.set_csc(Branch::One, None);
    assert_eq!(bits(&be).1 & rgb_enable::csc(1), 0);
}

#[test]
fn each_block_sets_its_enable_bit_and_clears_it() {
    let mut be = BackEnd::new();
    let b = bayer_enable::DEBIN;
    be.set_debin(Some(defaults::DEBIN_DEFAULT)).unwrap();
    assert_ne!(bits(&be).0 & b, 0, "debin");
    be.set_debin(None).unwrap();
    assert_eq!(bits(&be).0 & b, 0);

    be.set_demosaic(Some(defaults::DEMOSAIC_DEFAULT));
    assert_ne!(bits(&be).0 & bayer_enable::DEMOSAIC, 0, "demosaic");
    be.set_demosaic(None);
    assert_eq!(bits(&be).0 & bayer_enable::DEMOSAIC, 0);

    be.set_cac(Some(BeCacConfig::default()), BeCacExtra::default());
    assert_ne!(bits(&be).0 & bayer_enable::CAC, 0, "cac");
    be.set_cac(None, BeCacExtra::default());
    assert_eq!(bits(&be).0 & bayer_enable::CAC, 0);

    let tone = BeTonemapConfig {
        lut: [1; BE_TONEMAP_LUT_SIZE],
        ..Default::default()
    };
    be.set_tonemap(Some(tone)).unwrap();
    assert_ne!(bits(&be).0 & bayer_enable::TONEMAP, 0, "tonemap");
    be.set_tonemap(None).unwrap();
    assert_eq!(bits(&be).0 & bayer_enable::TONEMAP, 0);

    be.set_input_decompress(Some(DecompressConfig::default()));
    assert_ne!(
        bits(&be).0 & bayer_enable::DECOMPRESS,
        0,
        "input decompress"
    );
    be.set_input_decompress(None);
    assert_eq!(bits(&be).0 & bayer_enable::DECOMPRESS, 0);

    be.set_sharpen(Some(defaults::SHARPEN_DEFAULT)).unwrap();
    assert_ne!(bits(&be).1 & rgb_enable::SHARPEN, 0, "sharpen");
    be.set_sharpen(None).unwrap();
    assert_eq!(bits(&be).1 & rgb_enable::SHARPEN, 0);

    be.set_false_colour(Some(defaults::FALSE_COLOUR_DEFAULT));
    assert_ne!(bits(&be).1 & rgb_enable::FALSE_COLOUR, 0, "false colour");
    be.set_false_colour(None);
    assert_eq!(bits(&be).1 & rgb_enable::FALSE_COLOUR, 0);

    be.set_sat_control(Some(BeSatControlConfig::default()));
    assert_ne!(
        bits(&be).1 & rgb_enable::SAT_CONTROL,
        0,
        "saturation control"
    );
    be.set_sat_control(None);
    assert_eq!(bits(&be).1 & rgb_enable::SAT_CONTROL, 0);

    be.set_hog(Some(BeHogConfig::default()));
    assert_ne!(bits(&be).1 & rgb_enable::HOG, 0, "hog");
    be.set_hog(None);
    assert_eq!(bits(&be).1 & rgb_enable::HOG, 0);
}

#[test]
fn sharpen_refuses_enables_outside_its_five_filters() {
    let mut be = BackEnd::new();
    let mut s = defaults::SHARPEN_DEFAULT;
    s.enables = 0x1f;
    assert_eq!(be.set_sharpen(Some(s)), Ok(()));
    s.enables = 0x20;
    assert!(be.set_sharpen(Some(s)).is_err());
    assert_ne!(
        bits(&be).1 & rgb_enable::SHARPEN,
        0,
        "a refused setter leaves the block as it was"
    );
}

#[test]
fn debin_enables_are_zero_or_one() {
    let mut be = BackEnd::new();
    let mut d = defaults::DEBIN_DEFAULT;
    d.h_enable = 2;
    assert!(be.set_debin(Some(d)).is_err());
    d.h_enable = 1;
    d.v_enable = -1;
    assert!(be.set_debin(Some(d)).is_err());
    assert_eq!(bits(&be).0 & bayer_enable::DEBIN, 0);
}

#[test]
fn an_all_zero_tone_map_is_refused() {
    let mut be = BackEnd::new();
    assert!(be.set_tonemap(Some(BeTonemapConfig::default())).is_err());
    assert_eq!(bits(&be).0 & bayer_enable::TONEMAP, 0);
}

#[test]
fn the_default_debin_and_demosaic_reach_the_config_when_enabled() {
    let mut be = simple_for_defaults();
    be.set_debin(Some(defaults::DEBIN_DEFAULT)).unwrap();
    let t = be.prepare().unwrap();
    assert_eq!(t.config.debin.coeffs, [-7, 105, 35, -5]);
    assert_eq!((t.config.debin.h_enable, t.config.debin.v_enable), (1, 1));
    assert_eq!(
        (t.config.demosaic.sharper, t.config.demosaic.fc_mode),
        (8, 1)
    );
}

fn simple_for_defaults() -> BackEnd {
    BackEnd::simple_bayer(
        bayer16(1280, 800),
        BayerOrder::Bggr,
        4096,
        (1.5, 1.0, 1.8),
        None,
        formats::NV12,
        None,
    )
}

#[test]
fn a_cac_grid_follows_the_input_and_the_tile_offsets() {
    let mut be = simple_for_defaults();
    be.set_cac(
        Some(BeCacConfig::default()),
        BeCacExtra {
            offset_x: 1,
            offset_y: 0,
        },
    );
    let t = be.prepare().unwrap();
    let c = &t.config.cac;
    assert_eq!(u32::from(c.grid_step_x), (8 << 20) / 1280);
    assert_eq!(u32::from(c.grid_step_y), (8 << 20) / 800);
    for tile in &t.tiles[..t.num_tiles as usize] {
        assert_eq!(
            tile.cac_grid_offset_x,
            (u32::from(tile.input_offset_x) + 1) * u32::from(c.grid_step_x)
        );
        assert_eq!(
            tile.cac_grid_offset_y,
            u32::from(tile.input_offset_y) * u32::from(c.grid_step_y)
        );
    }
}

#[test]
fn a_cac_grid_that_does_not_cover_the_image_is_refused() {
    let mut be = simple_for_defaults();
    let c = BeCacConfig {
        grid_step_x: u16::MAX,
        grid_step_y: 1,
        ..Default::default()
    };
    be.set_cac(Some(c), BeCacExtra::default());
    assert!(be.prepare().is_err());
}

#[test]
fn an_enabled_hog_is_refused_until_it_is_sized() {
    let mut be = simple_for_defaults();
    be.set_hog(Some(BeHogConfig::default()));
    assert!(be.prepare().is_err());
}
