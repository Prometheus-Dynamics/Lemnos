//! The descriptor walk, on a whole camera and on every way of breaking one.

use super::fixture::{self, webcam};
use crate::descriptors::{self, Alt, MAX_FRAME_BYTES, Mode, parse};

const YUY2: u32 = u32::from_le_bytes(*b"YUY2");
const MJPG: u32 = u32::from_le_bytes(*b"MJPG");

fn mode(
    fourcc: u32,
    size: (u32, u32),
    fps_milli: u32,
    max: u32,
    index: (u8, u8),
    interval: u32,
    expected: Option<u32>,
) -> Mode {
    Mode {
        fourcc,
        width: size.0,
        height: size.1,
        fps_milli,
        max_frame_bytes: max,
        format_index: index.0,
        frame_index: index.1,
        interval,
        expected_bytes: expected,
    }
}

#[test]
fn a_camera_parses_into_every_format_size_and_rate() {
    let camera = parse(&webcam().bytes).unwrap();
    assert_eq!(camera.version, 0x0110);
    assert_eq!(camera.stream_interface, 1);
    assert_eq!(camera.malformed, 0);
    assert_eq!(camera.unsupported, 0);
    let vga = Some(614_400);
    let qvga = Some(153_600);
    assert_eq!(
        camera.modes,
        vec![
            mode(YUY2, (640, 480), 30_000, 614_400, (1, 1), 333_333, vga),
            mode(YUY2, (640, 480), 15_000, 614_400, (1, 1), 666_666, vga),
            mode(YUY2, (320, 240), 30_000, 153_600, (1, 2), 333_333, qvga),
            mode(YUY2, (320, 240), 20_000, 153_600, (1, 2), 500_000, qvga),
            mode(YUY2, (320, 240), 10_000, 153_600, (1, 2), 1_000_000, qvga),
            mode(MJPG, (1280, 720), 30_000, 921_600, (2, 1), 333_333, None),
        ]
    );
    assert_eq!(
        camera.alts,
        vec![
            Alt {
                alternate: 1,
                endpoint: 0x81,
                bytes: 192
            },
            Alt {
                alternate: 2,
                endpoint: 0x81,
                bytes: 944
            },
            Alt {
                alternate: 3,
                endpoint: 0x81,
                bytes: 3072
            },
        ]
    );
}

/// The camera with one edit made.
fn with(edit: impl FnOnce(&mut Vec<u8>, &fixture::Webcam)) -> Vec<u8> {
    let cam = webcam();
    let mut bytes = cam.bytes.clone();
    edit(&mut bytes, &cam);
    bytes
}

/// Parses, requiring a usable camera with this much malformed.
fn parses_with_malformed(bytes: &[u8], malformed: u32) -> descriptors::Camera {
    let camera = parse(bytes).unwrap();
    assert_eq!(camera.malformed, malformed, "{camera:?}");
    camera
}

#[test]
fn nothing_at_all_is_refused() {
    assert_eq!(parse(&[]), Err(descriptors::BAD_CONFIG));
    assert_eq!(parse(&[9, 2]), Err(descriptors::BAD_CONFIG));
}

#[test]
fn a_config_header_that_cannot_be_right_is_refused() {
    let short = with(|bytes, _| bytes[0] = 8);
    assert_eq!(parse(&short), Err(descriptors::BAD_CONFIG));
    let wrong = with(|bytes, _| bytes[1] = 3);
    assert_eq!(parse(&wrong), Err(descriptors::BAD_CONFIG));
    let tiny = with(|bytes, _| bytes[2..4].copy_from_slice(&5u16.to_le_bytes()));
    assert_eq!(parse(&tiny), Err(descriptors::BAD_CONFIG));
}

#[test]
fn a_total_length_past_the_bytes_read_is_counted_and_what_arrived_is_used() {
    let bytes = with(|bytes, _| bytes[2..4].copy_from_slice(&0xfff0u16.to_le_bytes()));
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 6);
}

#[test]
fn a_total_length_shorter_than_the_config_ends_the_walk_there() {
    let bytes = with(|bytes, cam| {
        let total = (cam.vs_header as u16).to_le_bytes();
        bytes[2..4].copy_from_slice(&total);
    });
    // The walk stops right after the streaming interface, before any format.
    assert_eq!(parse(&bytes), Err(descriptors::NO_MODES));
}

#[test]
fn a_zero_length_descriptor_ends_the_walk_without_hanging() {
    let bytes = with(|bytes, cam| bytes[cam.format_mjpeg] = 0);
    // The walk stops before the alternate settings, so there is nothing to stream from.
    assert_eq!(parse(&bytes), Err(descriptors::NO_ISO));
}

#[test]
fn a_descriptor_running_past_the_end_ends_the_walk() {
    let bytes = with(|bytes, cam| bytes[cam.alt3_endpoint] = 200);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 6);
    assert_eq!(camera.alts.len(), 2);
}

#[test]
fn class_header_total_lengths_are_checked() {
    let bytes = with(|bytes, cam| {
        bytes[cam.vc_header + 5..cam.vc_header + 7].copy_from_slice(&0xfff0u16.to_le_bytes());
    });
    parses_with_malformed(&bytes, 1);
    let bytes = with(|bytes, cam| {
        bytes[cam.vs_header + 4..cam.vs_header + 6].copy_from_slice(&4u16.to_le_bytes());
    });
    parses_with_malformed(&bytes, 1);
}

#[test]
fn a_format_index_out_of_range_loses_that_format_and_its_frames_are_counted() {
    let bytes = with(|bytes, cam| bytes[cam.format_mjpeg + 3] = 3);
    let camera = parses_with_malformed(&bytes, 2);
    assert_eq!(camera.modes.len(), 5);
    assert!(camera.modes.iter().all(|mode| mode.fourcc != MJPG));
}

#[test]
fn a_format_index_of_zero_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.format_mjpeg + 3] = 0);
    let camera = parses_with_malformed(&bytes, 2);
    assert_eq!(camera.modes.len(), 5);
}

#[test]
fn a_duplicate_format_index_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.format_mjpeg + 3] = 1);
    let camera = parses_with_malformed(&bytes, 2);
    assert_eq!(camera.modes.len(), 5);
}

#[test]
fn a_frame_index_out_of_range_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.frame_mjpeg + 3] = 2);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 5);
}

#[test]
fn a_frame_index_of_zero_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.frame_vga + 3] = 0);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 4);
}

#[test]
fn a_duplicate_frame_index_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.frame_qvga + 3] = 1);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 3);
}

#[test]
fn a_frame_with_no_format_before_it_is_counted() {
    let bytes = with(|bytes, cam| bytes[cam.format_yuy2 + 2] = 0x7f);
    // The format is unknown to the walk, so both its frames have no format.
    let camera = parses_with_malformed(&bytes, 2);
    assert_eq!(camera.modes.len(), 1);
}

#[test]
fn a_zero_size_frame_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.frame_vga + 5..cam.frame_vga + 7].fill(0));
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 4);
}

#[test]
fn an_impossible_frame_buffer_size_is_refused() {
    let bytes = with(|bytes, cam| {
        let too_big = (MAX_FRAME_BYTES + 1).to_le_bytes();
        bytes[cam.frame_mjpeg + 17..cam.frame_mjpeg + 21].copy_from_slice(&too_big);
    });
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 5);
}

#[test]
fn a_zero_frame_interval_loses_that_rate_only() {
    let bytes = with(|bytes, cam| {
        bytes[cam.frame_vga + 26..cam.frame_vga + 30].fill(0);
    });
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 5);
    assert_eq!(camera.modes[0].interval, 666_666);
}

#[test]
fn a_rate_count_longer_than_the_descriptor_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.frame_vga + 25] = 200);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 4);
}

#[test]
fn a_continuous_range_that_makes_no_sense_is_refused() {
    let bytes = with(|bytes, cam| {
        bytes[cam.frame_qvga + 26..cam.frame_qvga + 30].fill(0);
    });
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 3);
}

#[test]
fn a_continuous_descriptor_too_short_for_its_range_is_refused() {
    let edit = |bytes: &mut Vec<u8>, cam: &fixture::Webcam| {
        // Claim a discrete count of 0 on a descriptor with only 8 bytes of intervals.
        bytes[cam.frame_vga + 25] = 0;
    };
    let bytes = with(edit);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.modes.len(), 4);
}

#[test]
fn an_input_header_endpoint_going_the_wrong_way_loses_the_endpoints() {
    let bytes = with(|bytes, cam| bytes[cam.vs_header + 6] = 0x01);
    assert_eq!(parse(&bytes), Err(descriptors::NO_ISO));
}

#[test]
fn an_endpoint_other_than_the_input_header_named_is_refused() {
    let bytes = with(|bytes, cam| bytes[cam.alt1_endpoint + 2] = 0x82);
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.alts.len(), 2);
}

#[test]
fn a_reserved_transactions_field_is_refused() {
    let bytes = with(|bytes, cam| {
        let max_packet = (0x1800u16 | 1024).to_le_bytes();
        bytes[cam.alt3_endpoint + 4..cam.alt3_endpoint + 6].copy_from_slice(&max_packet);
    });
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.alts.len(), 2);
}

#[test]
fn a_zero_size_endpoint_is_refused() {
    let bytes = with(|bytes, cam| {
        bytes[cam.alt1_endpoint + 4..cam.alt1_endpoint + 6].fill(0);
    });
    let camera = parses_with_malformed(&bytes, 1);
    assert_eq!(camera.alts.len(), 2);
}

#[test]
fn a_superspeed_companion_sets_the_real_rate() {
    let mut parts = fixture::control_parts();
    parts.push(fixture::interface(1, 0, 0, 2));
    parts.push(vec![14, 0x24, 0x01, 1, 40, 0, 0x81, 0, 3, 0, 0, 0, 1, 0]);
    parts.push(fixture::yuy2_format(1, 1));
    parts.push(fixture::frame(
        0x05,
        1,
        (2, 2),
        8,
        333_333,
        &[333_333],
        false,
    ));
    parts.push(fixture::interface(1, 1, 1, 2));
    parts.push(fixture::iso_endpoint(0x81, 1024));
    parts.push(vec![6, 0x30, 0, 0, 0x00, 0x60]);
    let camera = parse(&fixture::config(&parts)).unwrap();
    assert_eq!(
        camera.alts,
        vec![Alt {
            alternate: 1,
            endpoint: 0x81,
            bytes: 0x6000
        }]
    );
}

#[test]
fn a_device_with_no_video_interfaces_is_refused() {
    let parts = vec![vec![9, 4, 0, 0, 0, 0x08, 0x06, 0x50, 0]];
    assert_eq!(parse(&fixture::config(&parts)), Err(descriptors::NOT_UVC));
}

#[test]
fn a_bulk_only_camera_is_refused_by_name() {
    let mut parts = fixture::control_parts();
    parts.push(fixture::interface(1, 0, 1, 2));
    parts.push(vec![14, 0x24, 0x01, 1, 40, 0, 0x81, 0, 3, 0, 0, 0, 1, 0]);
    parts.push(fixture::yuy2_format(1, 1));
    parts.push(fixture::frame(
        0x05,
        1,
        (2, 2),
        8,
        333_333,
        &[333_333],
        false,
    ));
    parts.push(fixture::bulk_endpoint(0x81));
    assert_eq!(parse(&fixture::config(&parts)), Err(descriptors::NO_ISO));
}

#[test]
fn a_format_guid_that_is_not_standard_is_skipped_as_unsupported() {
    let bytes = with(|bytes, cam| bytes[cam.format_yuy2 + 20] ^= 0xff);
    let camera = parse(&bytes).unwrap();
    assert_eq!(camera.malformed, 0);
    assert_eq!(camera.unsupported, 1);
    assert_eq!(camera.modes.len(), 1);
}

#[test]
fn every_truncation_and_every_corrupted_byte_parses_without_panicking() {
    let cam = webcam();
    for end in 0..=cam.bytes.len() {
        let _ = parse(&cam.bytes[..end]);
    }
    for at in 0..cam.bytes.len() {
        for value in [0, 1, 2, 7, 8, 9, 0x24, 0x7f, 0x80, 0xff] {
            let mut bytes = cam.bytes.clone();
            bytes[at] = value;
            if let Ok(camera) = parse(&bytes) {
                assert!(camera.modes.len() <= descriptors::MAX_MODES);
                assert!(camera.alts.len() <= descriptors::MAX_ALTS);
                for mode in &camera.modes {
                    assert!(mode.max_frame_bytes <= MAX_FRAME_BYTES);
                    assert_ne!(mode.interval, 0);
                }
            }
        }
    }
}

#[test]
fn alternate_settings_are_chosen_by_bandwidth() {
    let camera = parse(&webcam().bytes).unwrap();
    assert_eq!(camera.alt_for(1).map(|alt| alt.alternate), Some(1));
    assert_eq!(camera.alt_for(192).map(|alt| alt.alternate), Some(1));
    assert_eq!(camera.alt_for(193).map(|alt| alt.alternate), Some(2));
    assert_eq!(camera.alt_for(3072).map(|alt| alt.alternate), Some(3));
    // More than any setting has gets the largest.
    assert_eq!(camera.alt_for(1 << 20).map(|alt| alt.alternate), Some(3));
}
