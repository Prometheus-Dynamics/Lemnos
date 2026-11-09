use alloc::vec::Vec;

use super::*;

/// The bit stream for one LED: its data bytes in G, R, B, W order, 3 SPI bytes each.
fn led(pixel: Rgbw, wire: Wire) -> Vec<u8> {
    [pixel.g, pixel.r, pixel.b, pixel.w][..wire.bytes()]
        .iter()
        .flat_map(|&byte| encode_spi_byte(byte))
        .collect()
}

#[test]
fn spi_frame_puts_each_led_at_its_physical_slot_with_offset() {
    let config = StripConfig::new(4, Wire::Rgbw).with_offset(1);
    let mut out = [0xaa; 48];
    let n = encode_spi(
        &config,
        [Rgbw::new(1, 2, 3, 4), Rgbw::rgb(0x102030)].into_iter(),
        &mut out,
    );
    assert_eq!(n, 48);
    // Logical 0 at physical 1, logical 1 at physical 2, the rest off.
    let off = led(Rgbw::new(0, 0, 0, 0), Wire::Rgbw);
    let expected = [
        off.clone(),
        led(Rgbw::new(1, 2, 3, 4), Wire::Rgbw),
        led(Rgbw::rgb(0x102030), Wire::Rgbw),
        off,
    ]
    .concat();
    assert_eq!(out.as_slice(), expected.as_slice());
    assert_eq!(config.physical(3), 0);
}

#[test]
fn output_folds_white_into_rgb_and_scales_brightness() {
    let config = StripConfig::new(1, Wire::Rgb).with_brightness(128);
    // 100 × 128/255 = 50, 20 → 10; 250 → 125; white folded in.
    assert_eq!(
        config.output(Rgbw::new(100, 0, 250, 20)),
        Rgbw::new(60, 10, 135, 0)
    );
    assert_eq!(Rgbw::rgb(0x123456).to_rgb(), 0x123456);
}

#[test]
fn spi_stream_encodes_three_bits_per_bit_in_grb_order() {
    assert_eq!(encode_spi_byte(0x00), [0x92, 0x49, 0x24]);
    assert_eq!(encode_spi_byte(0xff), [0xdb, 0x6d, 0xb6]);
    let config = StripConfig::new(2, Wire::Rgb);
    let mut out = [0; 18];
    assert_eq!(
        encode_spi(&config, [Rgbw::rgb(0xff0000)].into_iter(), &mut out),
        18
    );
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
    let example = StripConfig::new(16, Wire::Rgb).with_offset(5);
    assert_eq!(
        (
            example.physical(0),
            example.physical(10),
            example.physical(11)
        ),
        (5, 15, 0)
    );
    let ccw = example.with_direction(Direction::Ccw);
    assert_eq!(
        (ccw.physical(0), ccw.physical(1), ccw.physical(6)),
        (5, 4, 15)
    );
    assert_eq!(Direction::from_name("ccw"), Some(Direction::Ccw));
}

#[test]
fn spi_byte_encoding_matches_the_bit_expansion_for_every_byte() {
    for byte in 0..=255u8 {
        assert_eq!(
            encode_spi_byte(byte),
            spi_byte_bits(byte),
            "byte {byte:#04x}"
        );
    }
}
