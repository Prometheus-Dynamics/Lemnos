//! Decoder tests on a node that plays a stateless device back, with a real access unit (one
//! 320x240 IDR frame): the request's controls and buffers, the frame lent to the caller, input
//! that is truncated or corrupted, the backoff after device failures, and the steady state's
//! allocations.

use super::*;
use crate::mock::{Call, Mock, MockNode, fill_byte};
use lemnos_linux_sys::v4l2::BufType;
use lemnos_measure::test_alloc::allocations;

const STREAM: &[u8] = include_bytes!("../../../hevc/tests/fixtures/idr320.h265");
const MS: u64 = 1_000_000;

type Decoder = HevcDecoder<MockNode, Box<dyn FnMut() -> io::Result<MockNode>>>;

fn decoder(mock: &Mock) -> Decoder {
    let m = mock.clone();
    HevcDecoder::new(Box::new(move || m.open()), Settings::default())
}

/// The slice unit (type 19) of the fixture, as stored.
fn slice_unit() -> &'static [u8] {
    hevc::nal_units(STREAM).last().unwrap().raw
}

/// Where the slice unit starts in the fixture, start code included.
fn slice_start() -> usize {
    slice_unit().as_ptr() as usize - STREAM.as_ptr() as usize - 3
}

fn decode_ok(d: &mut Decoder, now: u64, unit: &[u8]) -> (u32, usize) {
    d.decode(now, unit, Duration::from_millis(1), |f| {
        (f.sequence, f.planes().iter().map(|p| p.len()).sum())
    })
    .unwrap()
    .unwrap()
}

#[test]
fn an_idr_frame_runs_as_one_request_and_is_lent_to_the_caller() {
    let mock = Mock::new(true);
    mock.with(|d| d.capture_fourcc = Some(v4l2::fourcc(b"NC12")));
    let mut d = decoder(&mock);
    assert_eq!(d.state(), State::Closed);
    let seen = d
        .decode(0, STREAM, Duration::from_millis(7), |f| {
            let Format::Multi(m) = f.format else {
                panic!("a multi-planar device gives a multi-planar format")
            };
            assert_eq!((m.width, m.height), (320, 240));
            // The format the driver answered with, not the one asked for.
            assert_eq!(m.pixelformat, v4l2::fourcc(b"NC12"));
            assert_eq!(f.num_planes, 2);
            // The planes as the device wrote them: capture buffer 0's.
            assert_eq!(f.planes()[0].len(), 320 * 240);
            assert_eq!(f.planes()[1].len(), 320 * 240 / 2);
            assert!(f.planes()[0].iter().all(|&b| b == fill_byte(true, 0, 0)));
            assert!(f.planes()[1].iter().all(|&b| b == fill_byte(true, 0, 1)));
            f.sequence
        })
        .unwrap();
    assert!(seen.is_some());
    assert_eq!(d.state(), State::Running);

    // The source buffer holds the slice with its start code in front.
    let s = d.session().unwrap();
    let source = s.output_plane(0, 0).unwrap();
    let unit = slice_unit();
    assert_eq!(&source[..3], &[0, 0, 1]);
    assert_eq!(&source[3..3 + unit.len()], unit);
    // Both buffers came back: the source is free, and the frame buffer is queued again.
    assert_eq!(s.free_output(), Some(0));
    assert_eq!(s.free_capture(), None);

    // The request's controls, in order, each with its structure's size.
    let controls = mock.with(|d| d.controls[..d.control_count].to_vec());
    let size = |n: usize| u32::try_from(n).unwrap();
    assert_eq!(
        controls,
        [
            (sys::CID_SPS, size(size_of::<sys::Sps>())),
            (sys::CID_PPS, size(size_of::<sys::Pps>())),
            (sys::CID_SLICE_PARAMS, size(size_of::<sys::SliceParams>())),
            (sys::CID_DECODE_PARAMS, size(size_of::<sys::DecodeParams>())),
            (sys::CID_DECODE_MODE, 0),
            (sys::CID_START_CODE, 0),
        ]
    );
    // Opened once, streaming both queues, the request queued and reinitialised.
    let calls: Vec<Call> = mock.with(|d| d.calls().collect());
    assert!(calls.contains(&Call::StreamOn(BufType::VideoCaptureMplane)));
    assert!(calls.contains(&Call::StreamOn(BufType::VideoOutputMplane)));
    let tail = &calls[calls.len() - 2..];
    assert_eq!(tail, [Call::QueueRequest, Call::ReinitRequest]);
    assert_eq!(mock.with(|d| d.opens), 1);

    // The next frames use the same session, rotating through the frame buffers.
    for n in 1..10 {
        decode_ok(&mut d, n * MS, STREAM);
    }
    assert_eq!(mock.with(|d| d.opens), 1);
}

#[test]
fn a_single_planar_device_gets_single_planar_queues() {
    let mock = Mock::new(false);
    let mut d = decoder(&mock);
    let (_, bytes) = decode_ok(&mut d, 0, STREAM);
    assert!(bytes > 0);
    assert!(matches!(
        d.session().unwrap().capture_format(),
        Format::Single(_)
    ));
}

#[test]
fn parameter_sets_are_kept_for_the_frames_that_follow() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    let at = slice_start();
    let headers = &STREAM[..at];
    let picture = &STREAM[at..];
    // A slice before its parameter sets is refused without touching the device.
    assert!(matches!(
        d.decode(0, picture, Duration::ZERO, |_| ()),
        Err(DecodeError::Unsupported(_))
    ));
    assert_eq!(mock.with(|d| d.opens), 0);
    // Parameter sets alone: no picture.
    assert!(
        d.decode(0, headers, Duration::ZERO, |_| ())
            .unwrap()
            .is_none()
    );
    assert_eq!(mock.with(|d| d.opens), 0);
    // Then the slice alone decodes.
    decode_ok(&mut d, 0, picture);
}

#[test]
fn units_the_decoder_does_not_take_are_refused_before_the_device() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    let at = slice_start();
    // A non-IDR picture (TRAIL_R, type 1).
    let mut trail = STREAM.to_vec();
    trail[at + 3] = 1 << 1;
    assert!(matches!(
        d.decode(0, &trail, Duration::ZERO, |_| ()),
        Err(DecodeError::Unsupported(_))
    ));
    // Two slices in one access unit.
    let mut two = STREAM.to_vec();
    two.extend_from_slice(&STREAM[at..]);
    assert!(matches!(
        d.decode(0, &two, Duration::ZERO, |_| ()),
        Err(DecodeError::Unsupported(_))
    ));
    // A slice larger than a source buffer.
    let small = Settings {
        output_bytes: 64,
        ..Settings::default()
    };
    let m = mock.clone();
    let mut tiny = HevcDecoder::new(move || m.open(), small);
    assert!(matches!(
        tiny.decode(0, STREAM, Duration::ZERO, |_| ()),
        Err(DecodeError::TooLarge)
    ));
    assert_eq!(mock.with(|d| d.opens), 0);
}

#[test]
fn truncated_and_corrupted_units_never_panic_nor_back_the_device_off() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    let at = slice_start();
    // Cut at every byte up to well into the slice data.
    for cut in 0..(at + 96).min(STREAM.len()) {
        let _ = d.decode(cut as u64, &STREAM[..cut], Duration::ZERO, |_| ());
        assert!(
            !matches!(d.state(), State::BackingOff { .. }),
            "cut at {cut}"
        );
    }
    // Every bit of the headers and the slice header flipped, one at a time.
    let mut unit = STREAM.to_vec();
    for byte in 0..at + 32 {
        for bit in 0..8 {
            unit[byte] ^= 1 << bit;
            let _ = d.decode(0, &unit, Duration::ZERO, |_| ());
            unit[byte] ^= 1 << bit;
            assert!(!matches!(d.state(), State::BackingOff { .. }));
        }
    }
    // The original still decodes on the same device afterwards.
    d.stop();
    decode_ok(&mut d, 0, STREAM);
}

#[test]
fn a_device_failure_backs_off_doubling_to_8_s_and_never_gives_up() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    mock.with(|d| d.faults.open = true);
    let mut now = 0;
    let mut delays = Vec::new();
    for _ in 0..9 {
        let Err(DecodeError::Device { retry_at_ns, .. }) =
            d.decode(now, STREAM, Duration::ZERO, |_| ())
        else {
            panic!("the open fails")
        };
        delays.push((retry_at_ns - now) / MS);
        // Before it is due: refused without touching the device.
        assert!(matches!(
            d.decode(retry_at_ns - 1, STREAM, Duration::ZERO, |_| ()),
            Err(DecodeError::BackingOff { .. })
        ));
        now = retry_at_ns;
    }
    assert_eq!(delays, [250, 500, 1000, 2000, 4000, 8000, 8000, 8000, 8000]);
    assert_eq!(
        d.state(),
        State::BackingOff {
            failures: 9,
            retry_at_ns: now
        }
    );
    // The device comes back: the next due call opens it, and the backoff clears.
    mock.with(|d| d.faults.open = false);
    decode_ok(&mut d, now, STREAM);
    assert_eq!(d.state(), State::Running);
    mock.with(|d| d.faults.open = true);
    d.stop();
    let Err(DecodeError::Device { retry_at_ns, .. }) =
        d.decode(now, STREAM, Duration::ZERO, |_| ())
    else {
        panic!("the open fails")
    };
    assert_eq!(retry_at_ns - now, 250 * MS, "the backoff started over");
}

#[test]
fn a_request_that_does_not_complete_closes_the_session_and_reopens_later() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    decode_ok(&mut d, 0, STREAM);
    mock.with(|d| d.faults.hang = true);
    let Err(DecodeError::Device { error, retry_at_ns }) =
        d.decode(MS, STREAM, Duration::ZERO, |_| ())
    else {
        panic!("the request hangs")
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(retry_at_ns, MS + 250 * MS);
    assert!(d.session().is_none());
    // A refused request is a device failure too, with the next delay.
    mock.with(|d| {
        d.faults.hang = false;
        d.faults.queue_request = true;
    });
    let Err(DecodeError::Device {
        retry_at_ns: again, ..
    }) = d.decode(retry_at_ns, STREAM, Duration::ZERO, |_| ())
    else {
        panic!("the request is refused")
    };
    assert_eq!(again, retry_at_ns + 500 * MS);
    mock.with(|d| d.faults.queue_request = false);
    decode_ok(&mut d, again, STREAM);
    assert_eq!(mock.with(|d| d.opens), 3);
}

#[test]
fn a_frame_flagged_in_error_is_reported_and_the_session_stays() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    decode_ok(&mut d, 0, STREAM);
    mock.with(|d| d.faults.frame_error = true);
    assert!(matches!(
        d.decode(MS, STREAM, Duration::ZERO, |_| ()),
        Err(DecodeError::Corrupt)
    ));
    assert_eq!(d.state(), State::Running);
    assert_eq!(
        d.session().unwrap().free_capture(),
        None,
        "its buffer went back"
    );
    decode_ok(&mut d, 2 * MS, STREAM);
    assert_eq!(mock.with(|d| d.opens), 1);
}

#[test]
fn a_new_picture_size_sets_the_queues_up_again_without_a_backoff() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    decode_ok(&mut d, 0, STREAM);
    // The same SPS at another size, with the PPS and slice that follow it.
    let mut buf = [0u8; 256];
    let sps_nal = hevc::nal_units(STREAM).find(|n| n.unit_type == 33).unwrap();
    let n = hevc::unescape(sps_nal.raw, &mut buf).unwrap();
    let sps = hevc::parse_sps(&buf[..n]).unwrap();
    assert_eq!((sps.width, sps.height), (320, 240));
    let open_before = mock.with(|d| d.opens);
    d.link.as_mut().unwrap().size = (640, 480);
    decode_ok(&mut d, MS, STREAM);
    assert_eq!(mock.with(|d| d.opens), open_before + 1);
    assert_eq!(d.state(), State::Running);
}

#[test]
fn a_decoded_frame_allocates_nothing_once_open() {
    let mock = Mock::new(true);
    let mut d = decoder(&mock);
    decode_ok(&mut d, 0, STREAM);
    let ((), allocated) = allocations(|| {
        for n in 1..=1000u64 {
            let (_, bytes) = decode_ok(&mut d, n * MS, STREAM);
            assert_eq!(bytes, 320 * 240 * 3 / 2);
        }
    });
    assert_eq!(allocated, 0, "parse, controls, request, frame and requeue");
    // Refusals before the device allocate nothing either.
    let at = slice_start();
    let ((), allocated) = allocations(|| {
        for _ in 0..100 {
            let _ = d.decode(0, &STREAM[..at - 1], Duration::ZERO, |_| ());
        }
    });
    assert_eq!(allocated, 0);
}
