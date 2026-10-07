use super::*;

#[test]
fn rp1_layout_is_little_endian_rgbw_with_offset() {
    let config = StripConfig::new(4, Wire::Rgbw).with_offset(1);
    let mut out = [0xaa; 16];
    let n = encode_rp1(
        &config,
        &[Rgbw::new(1, 2, 3, 4), Rgbw::rgb(0x102030)],
        &mut out,
    );
    assert_eq!(n, 16);
    // Logical 0 at physical 1, logical 1 at physical 2, the rest off.
    assert_eq!(
        out,
        [0, 0, 0, 0, 1, 2, 3, 4, 0x10, 0x20, 0x30, 0, 0, 0, 0, 0]
    );
    assert_eq!(config.physical(3), 0);
}

#[test]
fn rgb_strips_fold_white_and_brightness_scales() {
    let config = StripConfig::new(1, Wire::Rgb).with_brightness(128);
    let mut out = [0; 4];
    encode_rp1(&config, &[Rgbw::new(100, 0, 250, 20)], &mut out);
    // 100 × 128/255 = 50, 20 → 10; 250 → 125; white folded in.
    assert_eq!(out, [60, 10, 135, 0]);
    assert_eq!(Rgbw::rgb(0x123456).to_rgb(), 0x123456);
}

#[test]
fn spi_stream_encodes_three_bits_per_bit_in_grb_order() {
    assert_eq!(encode_spi_byte(0x00), [0x92, 0x49, 0x24]);
    assert_eq!(encode_spi_byte(0xff), [0xdb, 0x6d, 0xb6]);
    let config = StripConfig::new(2, Wire::Rgb);
    let mut out = [0; 18];
    assert_eq!(encode_spi(&config, &[Rgbw::rgb(0xff0000)], &mut out), 18);
    // Green 0, red 0xff, blue 0, then an off LED.
    assert_eq!(
        &out[..9],
        &[0x92, 0x49, 0x24, 0xdb, 0x6d, 0xb6, 0x92, 0x49, 0x24]
    );
    assert_eq!(&out[9..12], &encode_spi_byte(0));
    assert_eq!(StripConfig::new(3, Wire::Rgbw).spi_len(), 36);
    assert_eq!(Wire::from_name("rgbw"), Some(Wire::Rgbw));
}

#[test]
fn geometry_maps_logical_indices_round_the_ring() {
    let raze = StripConfig::new(16, Wire::Rgb).with_offset(5);
    assert_eq!(
        (raze.physical(0), raze.physical(10), raze.physical(11)),
        (5, 15, 0)
    );
    let ccw = raze.with_direction(Direction::Ccw);
    assert_eq!(
        (ccw.physical(0), ccw.physical(1), ccw.physical(6)),
        (5, 4, 15)
    );
    assert_eq!(Direction::from_name("ccw"), Some(Direction::Ccw));
}
