//! The overlay's checks and the controls it builds: every expected byte here is written out by
//! hand from `nonai2d-v4l2-controls.h`.

use super::*;

/// A 64x32 ARGB frame.
fn argb() -> Layout {
    Layout::new(PixelFormat::Argb32, 64, 32).expect("layout")
}

fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn region(rect: Rect, image: &[u8]) -> Region<'_> {
    Region {
        rect,
        format: OsdFormat::Argb32,
        image,
        osd_alpha: 0xff,
        video_alpha: 0x80,
        background_alpha: 0x10,
        background: 0x00_12_34_56,
        order: Order::VideoOsd,
    }
}

fn border(rect: Rect, line_width: u16) -> Border {
    Border {
        rect,
        line_width,
        color: 0x00ff_0000,
    }
}

#[test]
fn the_control_ids_and_sizes_are_the_headers() {
    // V4L2_CID_USER_BASE (0x00980900) + 0x1200, then + 0..4.
    assert_eq!(CID_MODE, 0x0098_1b00);
    assert_eq!(CID_COLOR_GAMUT, 0x0098_1b01);
    assert_eq!(CID_COEFF, 0x0098_1b02);
    assert_eq!(CID_OSD_ATTR, 0x0098_1b03);
    assert_eq!(CID_BORDER_ATTR, 0x0098_1b04);
    // sizeof(nonai2d_osd_attr) * NONAI2D_OSD_REGION_NUM and sizeof(nonai2d_border_attr) on a
    // 64-bit kernel; `layout-check/run.sh` asks the compilers.
    assert_eq!(OSD_CONTROL_BYTES, 320);
    assert_eq!(BORDER_BYTES, 16);
    assert_eq!(
        [Mode::Convert, Mode::Osd, Mode::Border, Mode::OsdBorder].map(|m| m as u32),
        [0, 1, 2, 3]
    );
    assert_eq!(Order::OsdBackgroundVideo as u32, 9);
}

#[test]
fn osd_formats_are_the_ones_with_an_osd_format_of_their_own() {
    let cases = [
        (OsdFormat::Argb32, b"BA24", 4),
        (OsdFormat::Xrgb32, b"BX24", 4),
        (OsdFormat::Rgb24, b"RGB3", 3),
        (OsdFormat::Argb444, b"AR12", 2),
        (OsdFormat::Argb555, b"AR15", 2),
        (OsdFormat::Rgb565, b"RGBP", 2),
    ];
    for (format, code, bytes) in cases {
        assert_eq!(format.fourcc(), u32::from_le_bytes(*code));
        assert_eq!(format.bytes_per_pixel(), bytes);
    }
}

#[test]
fn frames_the_driver_mistakes_in_place_are_refused() {
    for format in [PixelFormat::Abgr32, PixelFormat::Xbgr32] {
        let layout = Layout::new(format, 64, 32).unwrap();
        assert_eq!(
            Overlay::new(layout).unwrap_err(),
            OverlayError::FrameFormat(format)
        );
    }
    for format in [
        PixelFormat::Argb32,
        PixelFormat::Xrgb32,
        PixelFormat::Rgb24,
        PixelFormat::Bgr24,
        PixelFormat::Rgb565,
        PixelFormat::Nv12,
        PixelFormat::Nv21,
        PixelFormat::Yuv420,
        PixelFormat::Yvu420,
    ] {
        Overlay::new(Layout::new(format, 64, 32).unwrap()).expect("drawable");
    }
}

#[test]
fn a_region_must_lie_inside_the_frame() {
    let image = vec![0u8; 64 * 32 * 4];
    let mut overlay = Overlay::new(argb()).unwrap();
    for (r, why) in [
        (rect(0, 0, 66, 2), OverlayError::Outside),
        (rect(2, 0, 64, 2), OverlayError::Outside),
        (rect(62, 0, 4, 2), OverlayError::Outside),
        (rect(0, 0, 2, 34), OverlayError::Outside),
        (rect(0, 30, 2, 4), OverlayError::Outside),
        (rect(u16::MAX - 1, 0, 2, 2), OverlayError::Outside),
        (rect(0, u16::MAX - 1, 2, 2), OverlayError::Outside),
        (rect(0, 0, 0, 2), OverlayError::Empty),
        (rect(0, 0, 2, 0), OverlayError::Empty),
        (rect(1, 0, 2, 2), OverlayError::Odd),
        (rect(0, 1, 2, 2), OverlayError::Odd),
        (rect(0, 0, 3, 2), OverlayError::Odd),
        (rect(0, 0, 2, 3), OverlayError::Odd),
    ] {
        assert_eq!(overlay.add_region(region(r, &image)), Err(why), "{r:?}");
    }
    assert_eq!(overlay.mode(), None, "nothing kept from a refusal");
    // Edge to edge is inside.
    overlay
        .add_region(region(rect(0, 0, 64, 32), &image))
        .unwrap();
    overlay
        .add_region(region(rect(62, 30, 2, 2), &image))
        .unwrap();
    assert_eq!(overlay.mode(), Some(Mode::Osd));
}

#[test]
fn a_region_needs_its_whole_image_and_a_24_bit_background() {
    let mut overlay = Overlay::new(argb()).unwrap();
    // 8x4 at four bytes a pixel: 128 bytes.
    let short = [0u8; 127];
    assert_eq!(
        overlay.add_region(region(rect(0, 0, 8, 4), &short)),
        Err(OverlayError::Image {
            have: 127,
            need: 128
        })
    );
    // At two bytes a pixel the same rectangle takes 64.
    let half = [0u8; 64];
    let rgb565 = Region {
        format: OsdFormat::Rgb565,
        ..region(rect(0, 0, 8, 4), &half)
    };
    overlay.add_region(rgb565).unwrap();
    let image = [0u8; 128];
    let dark = Region {
        background: 0x0100_0000,
        ..region(rect(0, 0, 8, 4), &image)
    };
    assert_eq!(overlay.add_region(dark), Err(OverlayError::Color));
    overlay
        .add_region(region(rect(0, 0, 8, 4), &image))
        .unwrap();
}

#[test]
fn a_border_keeps_its_line_width_inside_the_frame() {
    let mut overlay = Overlay::new(argb()).unwrap();
    for (b, why) in [
        (border(rect(2, 2, 8, 8), 0), OverlayError::Line),
        // Over half the rectangle's smaller side.
        (border(rect(6, 6, 8, 4), 4), OverlayError::Line),
        // A line of 2 needs 2 pixels of margin on every side.
        (border(rect(0, 2, 8, 8), 2), OverlayError::Outside),
        (border(rect(2, 0, 8, 8), 2), OverlayError::Outside),
        (border(rect(2, 2, 62, 8), 2), OverlayError::Outside),
        (border(rect(2, 2, 8, 30), 2), OverlayError::Outside),
        (border(rect(2, 2, 8, 8), 1), OverlayError::Odd),
        (border(rect(2, 2, 8, 0), 2), OverlayError::Line),
    ] {
        assert_eq!(overlay.add_border(b), Err(why), "{b:?}");
    }
    let red = Border {
        color: 0x0100_0000,
        ..border(rect(2, 2, 8, 8), 2)
    };
    assert_eq!(overlay.add_border(red), Err(OverlayError::Color));
    assert!(overlay.borders().is_empty());
    // The largest that fits.
    overlay.add_border(border(rect(2, 2, 60, 28), 2)).unwrap();
    assert_eq!(overlay.mode(), Some(Mode::Border));
}

#[test]
fn the_420_frames_take_the_same_checks() {
    let nv12 = Layout::new(PixelFormat::Nv12, 64, 32).unwrap();
    let mut overlay = Overlay::new(nv12).unwrap();
    let image = [0u8; 16 * 16 * 3];
    let rgb = Region {
        format: OsdFormat::Rgb24,
        ..region(rect(48, 16, 16, 16), &image)
    };
    overlay.add_region(rgb).unwrap();
    let past = Region {
        rect: rect(50, 16, 16, 16),
        ..rgb
    };
    assert_eq!(overlay.add_region(past), Err(OverlayError::Outside));
}

#[test]
fn eight_regions_and_thirty_two_borders_at_most() {
    let image = [0u8; 16];
    let mut overlay = Overlay::new(argb()).unwrap();
    for _ in 0..REGIONS {
        overlay
            .add_region(region(rect(0, 0, 2, 2), &image))
            .unwrap();
    }
    assert_eq!(
        overlay.add_region(region(rect(0, 0, 2, 2), &image)),
        Err(OverlayError::Full)
    );
    for _ in 0..BORDERS {
        overlay.add_border(border(rect(2, 2, 4, 4), 2)).unwrap();
    }
    assert_eq!(
        overlay.add_border(border(rect(2, 2, 4, 4), 2)),
        Err(OverlayError::Full)
    );
    assert_eq!(overlay.mode(), Some(Mode::OsdBorder));
}

#[test]
fn the_osd_control_holds_the_regions_from_index_0_and_their_images() {
    let a = [1u8; 8 * 4 * 4];
    let b = [2u8; 2 * 2 * 2];
    let mut overlay = Overlay::new(argb()).unwrap();
    overlay
        .add_region(Region {
            order: Order::BackgroundOsdVideo,
            ..region(rect(10, 6, 8, 4), &a)
        })
        .unwrap();
    overlay
        .add_region(Region {
            format: OsdFormat::Argb444,
            ..region(rect(20, 2, 2, 2), &b)
        })
        .unwrap();
    let control = overlay.osd_control();
    let mut first = [0u8; REGION_BYTES];
    first[0] = 0; // index
    first[1] = 1; // valid
    first[2..4].copy_from_slice(&8u16.to_le_bytes()); // width
    first[4..6].copy_from_slice(&4u16.to_le_bytes()); // height
    first[6..8].copy_from_slice(&10u16.to_le_bytes()); // startx
    first[8..10].copy_from_slice(&6u16.to_le_bytes()); // starty
    first[16..24].copy_from_slice(&(a.as_ptr() as usize as u64).to_le_bytes()); // data
    first[24] = 0x10; // bg_alpha
    first[25] = 0xff; // osd_alpha
    first[26] = 0x80; // video_alpha
    first[28..32].copy_from_slice(&7u32.to_le_bytes()); // add_order
    first[32..36].copy_from_slice(&0x0012_3456u32.to_le_bytes()); // bg_color
    first[36..40].copy_from_slice(b"BA24"); // pixfmt
    assert_eq!(control[..REGION_BYTES], first);
    let second = &control[REGION_BYTES..2 * REGION_BYTES];
    assert_eq!(second[..2], [1, 1]);
    assert_eq!(second[16..24], (b.as_ptr() as usize as u64).to_le_bytes());
    assert_eq!(&second[36..40], b"AR12");
    // The rest are invalid, with their index only: no gap below a valid one.
    for index in 2..REGIONS {
        let entry = &control[index * REGION_BYTES..(index + 1) * REGION_BYTES];
        assert_eq!(entry[0], index as u8);
        assert!(entry[1..].iter().all(|&b| b == 0), "region {index}");
    }
}

#[test]
fn a_border_control_is_one_border_at_its_index() {
    let mut overlay = Overlay::new(argb()).unwrap();
    overlay.add_border(border(rect(4, 6, 20, 10), 2)).unwrap();
    overlay
        .add_border(Border {
            color: 0x0000_ff00,
            ..border(rect(30, 4, 8, 8), 4)
        })
        .unwrap();
    assert_eq!(
        overlay.border_control(0).unwrap(),
        [
            0, 0, // index, padding
            20, 0, // width
            10, 0, // height
            2, 0, // line_width
            0x00, 0x00, 0xff, 0x00, // color
            4, 0, // startx
            6, 0, // starty
        ]
    );
    assert_eq!(
        overlay.border_control(1).unwrap(),
        [1, 0, 8, 0, 8, 0, 4, 0, 0x00, 0xff, 0x00, 0x00, 30, 0, 4, 0]
    );
    assert_eq!(overlay.border_control(2), None);
}
