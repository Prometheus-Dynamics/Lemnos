//! The board check: one IDR frame through a real stateless HEVC decoder, compared byte for byte
//! with a reference decode of the same frame. Ignored by default; run with `--ignored` and
//! `LEMNOS_M2M_DECODE_NODE` naming the decoder's video node.
//!
//! - `LEMNOS_M2M_DECODE_NODE`: the video node, for example the one `LinuxNode::find` gives for
//!   the decoder's driver name.
//! - `LEMNOS_M2M_DECODE_REFERENCE`: a reference decode of the fixture as linear NV12 at 320x240
//!   (115,200 bytes). Without it, the frame is only checked for size and for not being blank.
//!
//! The frame is read in the layout the driver answered with: linear NV12 (`NV12`), or NV12 in
//! 128-byte columns (`Nc12`, two planes, luma then chroma, each column `height` rows).

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use lemnos_driver_m2m_decode::{Format, HevcDecoder, LinuxNode, Settings, State};

const STREAM: &[u8] = include_bytes!("../../hevc/tests/fixtures/idr320.h265");
const FRAMES: u32 = 300;

fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// Linear NV12 of a `w` x `h` picture from the device's planes.
fn linear_nv12(format: &Format, planes: &[&[u8]], w: usize, h: usize) -> Vec<u8> {
    let Format::Multi(m) = format else {
        panic!("expected a multi-planar capture format, got {format:?}");
    };
    let [luma, chroma] = planes else {
        panic!("expected two planes, got {}", planes.len());
    };
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    if m.pixelformat == fourcc(b"Nc12") || m.pixelformat == fourcc(b"NC12") {
        const COL: usize = 128;
        for row in 0..h {
            out.extend((0..w).map(|x| luma[(x / COL) * COL * h + row * COL + x % COL]));
        }
        for row in 0..h / 2 {
            out.extend((0..w).map(|x| chroma[(x / COL) * COL * (h / 2) + row * COL + x % COL]));
        }
    } else if m.pixelformat == fourcc(b"NV12") {
        let (ls, cs) = (
            m.planes[0].bytes_per_line as usize,
            m.planes[1].bytes_per_line as usize,
        );
        for row in 0..h {
            out.extend_from_slice(&luma[row * ls..row * ls + w]);
        }
        for row in 0..h / 2 {
            out.extend_from_slice(&chroma[row * cs..row * cs + w]);
        }
    } else {
        panic!(
            "a capture format this check cannot read: {:#x}",
            m.pixelformat
        );
    }
    out
}

#[test]
#[ignore = "needs a stateless HEVC decoder: run with --ignored and LEMNOS_M2M_DECODE_NODE set"]
fn an_idr_frame_decodes_on_the_board_and_matches_the_reference() {
    let node = std::env::var("LEMNOS_M2M_DECODE_NODE").expect("LEMNOS_M2M_DECODE_NODE");
    let reference = std::env::var("LEMNOS_M2M_DECODE_REFERENCE")
        .ok()
        .map(|path| std::fs::read(path).expect("the reference file"));
    let started = Instant::now();
    let now = || u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let mut decoder = HevcDecoder::new(|| LinuxNode::open(&node), Settings::default());

    let frame = decoder
        .decode(now(), STREAM, Duration::from_millis(1), |f| {
            linear_nv12(f.format, f.planes(), 320, 240)
        })
        .expect("the frame decodes")
        .expect("the unit has a picture");
    assert_eq!(frame.len(), 320 * 240 * 3 / 2);
    assert!(
        frame.iter().any(|&b| b != frame[0]),
        "the frame is not blank"
    );
    if let Some(reference) = reference {
        let differ = frame.iter().zip(&reference).filter(|(a, b)| a != b).count();
        assert_eq!(reference.len(), frame.len(), "reference size");
        assert_eq!(differ, 0, "bytes that differ from the reference");
    }

    // The same frame again and again on the open session: each one matches the first.
    for n in 1..FRAMES {
        let again = decoder
            .decode(now(), STREAM, Duration::from_millis(u64::from(n)), |f| {
                linear_nv12(f.format, f.planes(), 320, 240)
            })
            .expect("the frame decodes")
            .expect("the unit has a picture");
        assert_eq!(again, frame, "frame {n}");
    }
    assert_eq!(decoder.state(), State::Running);
}
