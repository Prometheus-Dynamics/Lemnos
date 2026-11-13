//! Frames put together from replayed isochronous packets, compared byte for byte.

use lemnos_contract_camera_frames::CameraFrames;
use lemnos_core::{Health, Lifecycle};

use super::fixture::{MODE_MJPEG, MODE_QVGA_MIN, MODE_VGA_30};
use super::mock::Packet;
use super::rig::{EOF, ERR, FID, Rig, payload, payload_timed};
use crate::payload::{Assembler, Frame};
use crate::session::{self, identity};

fn mjpeg(frame_bytes: i64) -> Rig {
    Rig::streaming(frame_bytes, MODE_MJPEG)
}

/// A stream joined at a frame boundary: the last frame ended, with FID 0.
fn joined(frame_bytes: i64) -> Rig {
    let mut rig = mjpeg(frame_bytes);
    rig.feed(vec![payload(EOF, b"")]);
    rig
}

fn counters(rig: &mut Rig) -> (u64, u64, u64) {
    let counters = rig.uvc.counters().unwrap();
    (counters.frames, counters.dropped, counters.errors)
}

#[test]
fn a_frame_spans_packets_and_fid_toggles_between_frames() {
    let mut rig = joined(4096);
    rig.feed(vec![
        payload(FID, b"ab"),
        payload(FID, b"cde"),
        payload(FID | EOF, b"f"),
        payload(0, b"gh"),
        payload(EOF, b"ij"),
    ]);
    assert_eq!(rig.contents(), vec![b"abcdef".to_vec(), b"ghij".to_vec()]);
    let frames = rig.frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].bytes, 6);
    assert_eq!(frames[0].sequence, 0);
    assert_eq!(frames[1].bytes, 4);
    assert_eq!(frames[1].sequence, 1);
    assert_eq!(counters(&mut rig), (2, 0, 0));
}

#[test]
fn a_frame_can_end_in_a_later_transfer() {
    let mut rig = joined(4096);
    rig.feed(vec![payload(FID, b"ab")]);
    assert!(rig.contents().is_empty());
    rig.feed(vec![payload(FID | EOF, b"cd")]);
    assert_eq!(rig.contents(), vec![b"abcd".to_vec()]);
}

#[test]
fn packets_before_the_first_frame_boundary_are_dropped_and_counted() {
    let mut rig = mjpeg(4096);
    rig.feed(vec![payload(0, b"partial"), payload(0, b"more")]);
    assert!(rig.contents().is_empty());
    assert_eq!(rig.uvc.session.stats().out_of_sync, 2);
    rig.feed(vec![payload(FID, b"new"), payload(FID | EOF, b"!")]);
    assert_eq!(rig.contents(), vec![b"new!".to_vec()]);
}

#[test]
fn a_fid_toggle_ends_a_frame_with_no_eof() {
    let mut rig = joined(4096);
    rig.feed(vec![
        payload(FID, b"one"),
        payload(0, b"two"),
        payload(EOF, b"!"),
    ]);
    assert_eq!(rig.contents(), vec![b"one".to_vec(), b"two!".to_vec()]);
}

#[test]
fn one_packet_can_finish_two_frames() {
    let mut rig = joined(4096);
    rig.feed(vec![payload(FID, b"one"), payload(EOF, b"two")]);
    assert_eq!(rig.contents(), vec![b"one".to_vec(), b"two".to_vec()]);
}

#[test]
fn data_with_the_finished_frames_fid_is_out_of_sync() {
    let mut rig = joined(4096);
    rig.feed(vec![payload(FID | EOF, b"done"), payload(FID, b"stale")]);
    assert_eq!(rig.contents(), vec![b"done".to_vec()]);
    assert_eq!(rig.uvc.session.stats().out_of_sync, 1);
}

#[test]
fn header_only_packets_are_nothing() {
    let mut rig = joined(4096);
    rig.feed(vec![payload(FID, b""), payload(FID | EOF, b"")]);
    rig.feed(vec![payload(0, b"data"), payload(EOF, b"")]);
    assert_eq!(rig.contents(), vec![b"data".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 0, 0));
    assert_eq!(rig.uvc.session.stats().out_of_sync, 0);
}

#[test]
fn short_packets_and_gaps_keep_the_bytes_in_order() {
    let mut rig = joined(4096);
    let mut packets = vec![
        payload(FID, &[1]),
        Packet::good(Vec::new()),
        payload(FID, &[2, 3]),
        Packet::good(Vec::new()),
        payload(FID | EOF, &[4]),
    ];
    packets.push(Packet::good(Vec::new()));
    rig.feed(packets);
    assert_eq!(rig.contents(), vec![vec![1, 2, 3, 4]]);
}

#[test]
fn the_error_bit_damages_the_frame() {
    let mut rig = joined(4096);
    rig.feed(vec![
        payload(FID, b"ab"),
        payload(FID | ERR | EOF, b"cd"),
        payload(EOF, b"good"),
    ]);
    assert_eq!(rig.contents(), vec![b"good".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 0, 1));
}

#[test]
fn a_failed_packet_damages_its_frame() {
    let mut rig = joined(4096);
    rig.feed(vec![
        payload(FID, b"ab"),
        Packet::failed(),
        payload(FID | EOF, b"cd"),
        payload(EOF, b"good"),
    ]);
    assert_eq!(rig.contents(), vec![b"good".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 0, 1));
    assert_eq!(rig.uvc.session.stats().failed_packets, 1);
}

#[test]
fn a_failed_packet_between_frames_damages_the_next_one() {
    let mut rig = joined(4096);
    rig.feed(vec![
        Packet::failed(),
        payload(FID | EOF, b"lost"),
        payload(EOF, b"good"),
    ]);
    assert_eq!(rig.contents(), vec![b"good".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 0, 1));
}

#[test]
fn a_frame_too_big_for_the_buffer_is_dropped_and_counted() {
    let mut rig = joined(8);
    rig.feed(vec![
        payload(FID, b"123456"),
        payload(FID | EOF, b"789"),
        payload(EOF, b"fits ok!"),
    ]);
    assert_eq!(rig.contents(), vec![b"fits ok!".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 1, 0));
    assert_eq!(rig.uvc.session.stats().overruns, 1);
}

#[test]
fn an_exact_fit_is_not_an_overrun() {
    let mut rig = joined(4);
    rig.feed(vec![payload(FID | EOF, b"1234")]);
    assert_eq!(rig.contents(), vec![b"1234".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 0, 0));
}

#[test]
fn an_overrun_is_reported_until_a_frame_fits_again() {
    let mut rig = joined(4);
    rig.feed(vec![payload(FID | EOF, b"12345")]);
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Degraded(session::FRAME_TOO_BIG)
    );
    rig.feed(vec![payload(EOF, b"1234")]);
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Ok);
}

fn bad_header_cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("zero header length", vec![0, FID]),
        ("header of one", vec![1, FID, 9]),
        ("header past the packet", vec![200, FID, 1, 2, 3]),
        ("only the length byte", vec![12]),
        (
            "PTS announced, header too short",
            vec![4, FID | 0x04, 0, 0, 9],
        ),
        (
            "SCR announced, header too short",
            vec![7, FID | 0x08, 0, 0, 0, 0, 0, 9],
        ),
        (
            "PTS and SCR announced, header too short",
            vec![10, FID | 0x04 | 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 9],
        ),
    ]
}

#[test]
fn each_malformed_header_is_counted_and_damages_the_frame() {
    for (case, packet) in bad_header_cases() {
        let mut rig = joined(4096);
        rig.feed(vec![
            payload(FID, b"ab"),
            Packet::good(packet),
            payload(FID | EOF, b"cd"),
            payload(EOF, b"good"),
        ]);
        assert_eq!(rig.contents(), vec![b"good".to_vec()], "{case}");
        assert_eq!(counters(&mut rig), (1, 0, 1), "{case}");
        assert_eq!(rig.uvc.session.stats().bad_packets, 1, "{case}");
    }
}

#[test]
fn a_transfer_that_claims_more_than_the_packet_holds_is_counted_and_not_read() {
    let mut rig = joined(4096);
    // Alternate setting 2 carries 944 bytes.
    rig.feed(vec![
        payload(FID, b"ab"),
        Packet::lying(vec![2, FID], 945),
        payload(FID | EOF, b"cd"),
        payload(EOF, b"good"),
    ]);
    assert_eq!(rig.contents(), vec![b"good".to_vec()]);
    assert_eq!(rig.uvc.session.stats().bad_packets, 1);
    assert_eq!(counters(&mut rig), (1, 0, 1));
}

#[test]
fn a_full_packet_of_exactly_its_capacity_is_good() {
    let mut rig = joined(4096);
    let body = vec![0x5a; 942];
    rig.feed(vec![payload(FID | EOF, &body)]);
    assert_eq!(rig.contents(), vec![body]);
}

#[test]
fn an_uncompressed_frame_must_be_exactly_its_size() {
    let mut rig = Rig::streaming(200_000, MODE_QVGA_MIN);
    rig.feed(vec![payload(EOF, b"")]);
    let full = vec![0x11u8; 153_600];
    let mut packets = Vec::new();
    for (index, chunk) in full.chunks(900).enumerate() {
        let eof = index == full.len().div_ceil(900) - 1;
        packets.push(payload(FID | if eof { EOF } else { 0 }, chunk));
        if packets.len() == 32 {
            rig.feed(core::mem::take(&mut packets));
        }
    }
    rig.feed(packets);
    assert_eq!(rig.contents(), vec![full]);
    assert_eq!(counters(&mut rig), (1, 0, 0));

    // Far too short, twice.
    rig.feed(vec![payload(EOF, &[0x22; 100])]);
    let short = counters(&mut rig);
    assert_eq!(short, (1, 0, 1));
    rig.feed(vec![payload(FID | EOF, &[0x22; 942])]);
    assert_eq!(counters(&mut rig), (1, 0, 2));
    assert!(rig.contents().is_empty());
}

#[test]
fn an_uncompressed_frame_longer_than_its_size_is_damaged() {
    let mut assembler = Assembler::new(16);
    assembler.reset(Some(4));
    let mut frames = 0;
    let mut sink = |_: Frame<'_>| frames += 1;
    assembler.push(&[2, EOF], true, &mut sink);
    assembler.push(&[2, FID | EOF, 1, 2, 3, 4, 5], true, &mut sink);
    assembler.push(&[2, EOF, 1, 2, 3, 4], true, &mut sink);
    assert_eq!(frames, 1);
    assert_eq!(assembler.stats().damaged, 1);
}

#[test]
fn pts_and_source_clock_reference_become_the_frame_identity() {
    let mut rig = joined(4096);
    rig.feed(vec![
        payload_timed(FID, 0x1122_3344, 0x5566_7788, b"ab"),
        // A later packet doesn't replace what the frame started with.
        payload_timed(FID | EOF, 0x99aa_bbcc, 0xddee_ff00, b"cd"),
    ]);
    let frames = rig.frames();
    assert_eq!(frames[0].identity, 0x1122_3344_5566_7788);
    assert_eq!(frames[0].timestamp_ns, 1_000);
    assert_eq!(rig.contents(), vec![b"abcd".to_vec()]);
    assert_eq!(identity(None, None), 0);
    assert_eq!(identity(Some(1), None), 1 << 32);
    assert_eq!(identity(None, Some(1)), 1);
}

#[test]
fn a_frame_with_no_free_buffer_is_dropped_and_leaves_a_gap_in_the_sequence() {
    let mut rig = Rig::new(super::fixture::webcam().bytes, 4096, 1);
    rig.settle();
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle_streaming();
    rig.feed(vec![payload(EOF, b"")]);
    rig.feed(vec![payload(FID | EOF, b"first")]);
    // The only buffer is still with the consumer.
    rig.feed(vec![payload(EOF, b"second")]);
    assert_eq!(counters(&mut rig), (1, 1, 0));
    let frames = rig.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].sequence, 0);
    rig.feed(vec![payload(FID | EOF, b"third")]);
    let frames = rig.frames();
    assert_eq!(frames[0].sequence, 2);
    assert_eq!(rig.contents(), vec![b"first".to_vec(), b"third".to_vec()]);
}

#[test]
fn counters_clear_on_start() {
    let mut rig = joined(4096);
    rig.feed(vec![payload(FID | EOF, b"a"), payload(EOF | ERR, b"b")]);
    assert_eq!(counters(&mut rig), (1, 0, 1));
    rig.uvc.stop().unwrap();
    rig.settle();
    rig.uvc.start().unwrap();
    assert_eq!(counters(&mut rig), (0, 0, 0));
}

#[test]
fn hostile_packets_never_panic_and_never_overrun() {
    // A fixed-seed generator: the same replay every run.
    let mut seed = 0x1234_5678u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut rig = joined(64);
    for _ in 0..512 {
        let mut packets = Vec::new();
        for _ in 0..32 {
            let length = (next() % 40) as usize;
            let mut data: Vec<u8> = (0..length).map(|_| next() as u8).collect();
            if next() % 4 == 0 && !data.is_empty() {
                // A readable header some of the time, so frames do get going.
                data[0] = 2;
            }
            match next() % 8 {
                0 => packets.push(Packet::failed()),
                1 => packets.push(Packet::lying(data, next() % 4096 + 1)),
                _ => packets.push(Packet::good(data)),
            }
        }
        rig.feed(packets);
        for frame in rig.frames() {
            assert!(frame.bytes as usize <= 64);
        }
    }
    let stats = rig.uvc.session.stats();
    assert!(
        stats.bad_packets > 0 && stats.failed_packets > 0,
        "{stats:?}"
    );
}

#[test]
fn the_stream_starts_with_the_alternate_settings_packets_several_transfers_deep() {
    let mut rig = Rig::webcam(4096);
    rig.settle();
    rig.uvc.select_format(MODE_VGA_30).unwrap();
    rig.uvc.start().unwrap();
    assert_eq!(rig.usb.state().stream, None);
    rig.settle_streaming();
    // 900 asked for, so alternate 2's 944-byte packets, started once the setting is on.
    assert_eq!(
        rig.usb.state().stream,
        Some((0x81, session::TRANSFERS, session::PACKETS as u16, 944))
    );
    assert_eq!(rig.usb.state().alternates_mid_stream, 0);
    rig.feed(vec![payload(EOF, b"")]);
    assert_eq!(rig.usb.state().last_iso, Some((0x81, 944)));
}

#[test]
fn the_stream_is_started_once_and_each_tick_takes_the_next_transfer() {
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.feed(vec![payload(EOF, b"")]);
    let mut frames = 0;
    for n in 0u8..12 {
        let fid = if n % 2 == 0 { FID } else { 0 };
        rig.feed(vec![payload(fid, &[n, n]), payload(fid | EOF, &[n])]);
        frames += rig.frames().len();
    }
    let usb = rig.usb.state();
    assert_eq!(
        (usb.starts, usb.stops),
        (1, 0),
        "no transfer is left unqueued between ticks"
    );
    assert_eq!(usb.iso_calls, 13);
    drop(usb);
    assert_eq!(frames, 12);
}

#[test]
fn stopping_ends_the_stream_before_the_setting_goes_back_to_0() {
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.feed(vec![payload(EOF, b"")]);
    rig.uvc.stop().unwrap();
    rig.settle();
    let usb = rig.usb.state();
    assert_eq!((usb.starts, usb.stops), (1, 1));
    assert_eq!(usb.stream, None);
    assert_eq!(
        usb.alternates.last().map(|&(_, alternate)| alternate),
        Some(0)
    );
    assert_eq!(usb.alternates_mid_stream, 0);
}

#[test]
fn many_frames_arrive_intact_and_in_order_through_reused_buffers() {
    // Two buffers, so each is written over many times.
    let mut rig = Rig::new(super::fixture::webcam().bytes, 4096, 2);
    rig.settle();
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle_streaming();
    rig.feed(vec![payload(EOF, b"")]);
    let mut sent = Vec::new();
    for n in 0u32..40 {
        let fid = if n % 2 == 0 { FID } else { 0 };
        // Each frame different in length and content, split over three packets.
        let body: Vec<u8> = (0..(100 + n * 7)).map(|i| (i * 31 + n) as u8).collect();
        let (a, rest) = body.split_at(body.len() / 3);
        let (b, c) = rest.split_at(rest.len() / 2);
        rig.feed(vec![
            payload(fid, a),
            payload(fid, b),
            payload(fid | EOF, c),
        ]);
        sent.push(body);
        let frames = rig.frames();
        assert_eq!(frames.len(), 1, "frame {n}");
        assert_eq!(frames[0].sequence, u64::from(n));
        assert_eq!(frames[0].bytes as usize, sent[n as usize].len());
    }
    assert_eq!(rig.contents(), sent);
    assert_eq!(counters(&mut rig), (40, 0, 0));
}

#[test]
fn with_no_consumer_frames_are_dropped_and_capture_carries_on() {
    let mut rig = Rig::new(super::fixture::webcam().bytes, 4096, 2);
    rig.attached
        .store(false, std::sync::atomic::Ordering::Relaxed);
    rig.settle();
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle_streaming();
    rig.feed(vec![payload(EOF, b"")]);
    for n in 0..10u8 {
        let fid = if n % 2 == 0 { FID } else { 0 };
        rig.feed(vec![payload(fid | EOF, &[n; 5])]);
    }
    assert!(
        rig.frames().is_empty(),
        "nothing is published with no consumer"
    );
    assert_eq!(counters(&mut rig), (0, 10, 0));
    assert_eq!(rig.uvc.session.state(), session::STREAMING);
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Ok);
    // Only one buffer was taken from the stream, and it is still the driver's to use.
    assert_eq!(
        rig.stream.contents(0),
        vec![0; 4096],
        "nothing landed in it"
    );

    // A consumer attaches: the next frame reaches it, with the sequence counting every frame.
    rig.attached
        .store(true, std::sync::atomic::Ordering::Relaxed);
    rig.feed(vec![payload(FID | EOF, b"hello")]);
    let frames = rig.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].sequence, 10);
    assert_eq!(rig.contents(), vec![b"hello".to_vec()]);
    assert_eq!(counters(&mut rig), (1, 10, 0));
}

#[test]
fn a_frame_longer_than_the_streams_buffer_is_refused_dropped_and_reported() {
    // The board file gave the stream smaller buffers than `frame_bytes`.
    let mut rig = Rig::with_buffers(super::fixture::webcam().bytes, 4096, 2, 8);
    rig.settle();
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle_streaming();
    rig.feed(vec![payload(EOF, b"")]);
    rig.feed(vec![payload(FID | EOF, b"far too long")]);
    assert!(rig.frames().is_empty());
    assert_eq!(counters(&mut rig), (0, 1, 0));
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Degraded(session::FRAME_TOO_BIG)
    );
    rig.feed(vec![payload(EOF, b"fits")]);
    assert_eq!(rig.contents(), vec![b"fits".to_vec()]);
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Ok);
}
