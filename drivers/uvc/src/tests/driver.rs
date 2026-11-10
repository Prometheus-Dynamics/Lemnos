//! Binding, negotiation and the failure rule, as the driver does them.

use lemnos_contract_camera_frames::{CameraFrames, FrameFormat};
use lemnos_core::{Config, ConfigValue, Driver, ErrorKind, Health, Lifecycle, Status};
use lemnos_hal::blocks::ControlSetup;

use super::fixture::{
    self, CONTROL_INTERFACE, MODE_MJPEG, MODE_QVGA_MIN, MODE_VGA_15, STREAM_INTERFACE,
};
use super::mock::Control;
use super::rig::{self, Rig};
use crate::descriptors::{self, MAX_FRAME_BYTES};
use crate::session::{
    self, BACKOFF_CAP_NS, BACKOFF_FIRST_NS, CLAIM_BUSY, DETACH_FAILED, GONE, IDLE, STARTING,
    STREAMING,
};

const SET_CUR: u8 = 0x01;
const GET_CUR: u8 = 0x81;
const OUT: u8 = 0x21;
const IN: u8 = 0xa1;

#[test]
fn binding_touches_no_usb() {
    let rig = Rig::webcam(4096);
    assert_eq!(rig.usb.state().descriptor_reads, 0);
    assert_eq!(rig.usb.state().claims, 0);
    assert_eq!(rig.uvc.session.state(), STARTING);
}

#[test]
fn frame_bytes_is_required_and_bounded() {
    let cases = [
        vec![],
        vec![("frame_bytes", ConfigValue::Int(0))],
        vec![("frame_bytes", ConfigValue::Int(-1))],
        vec![("frame_bytes", ConfigValue::Bool(true))],
        vec![(
            "frame_bytes",
            ConfigValue::Int(i64::from(MAX_FRAME_BYTES) + 1),
        )],
    ];
    for case in &cases {
        assert_eq!(
            rig::bind_config(&Config::new(case)).err(),
            Some(Status::INVALID_ARGUMENT),
            "{case:?}"
        );
    }
    assert!(
        rig::bind_config(&Config::new(&[(
            "frame_bytes",
            ConfigValue::Int(i64::from(MAX_FRAME_BYTES))
        )]))
        .is_ok()
    );
}

#[test]
fn it_needs_a_camera_and_a_stream_and_provides_the_contract() {
    let manifest = <crate::Uvc as Driver>::MANIFEST;
    let needs: Vec<&str> = manifest.needs.iter().map(|need| need.name).collect();
    assert_eq!(needs, vec!["usb", "frames"]);
    assert_eq!(manifest.provides.len(), 1);
}

#[test]
fn formats_are_listed_from_the_descriptors() {
    let mut rig = Rig::webcam(4096);
    assert_eq!(rig.uvc.format_count(), Err(Status::AGAIN));
    assert_eq!(rig.uvc.format(0), Err(Status::AGAIN));
    rig.settle();
    assert_eq!(rig.uvc.format_count(), Ok(6));
    assert_eq!(
        rig.uvc.format(5),
        Ok(FrameFormat {
            fourcc: u32::from_le_bytes(*b"MJPG"),
            width: 1280,
            height: 720,
            fps_milli: 30_000,
            max_frame_bytes: 921_600,
        })
    );
    assert_eq!(rig.uvc.format(6), Err(Status::NOT_FOUND));
}

#[test]
fn selecting_follows_the_contract() {
    let mut rig = Rig::webcam(4096);
    rig.settle();
    assert_eq!(rig.uvc.selected(), Ok(None));
    assert_eq!(rig.uvc.start(), Err(Status::INVALID_ARGUMENT));
    assert_eq!(rig.uvc.select_format(6), Err(Status::NOT_FOUND));
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    assert_eq!(rig.uvc.selected(), Ok(Some(MODE_MJPEG)));
    rig.uvc.start().unwrap();
    assert_eq!(rig.uvc.select_format(0), Err(Status::BUSY));
    rig.uvc.stop().unwrap();
    assert_eq!(rig.uvc.select_format(0), Ok(()));
}

#[test]
fn the_stream_name_is_answered() {
    let mut rig = Rig::webcam(4096);
    let mut name = [0u8; 32];
    let length = rig.uvc.frame_stream(&mut name).unwrap();
    assert_eq!(&name[..length], b"frames");
    assert_eq!(
        rig.uvc.frame_stream(&mut [0u8; 3]),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_mode_is_negotiated_in_order_with_the_exact_bytes() {
    let rig = Rig::streaming(4096, MODE_VGA_15);
    let setup = |request_type, request, control: u16| ControlSetup {
        request_type,
        request,
        value: control << 8,
        index: u16::from(STREAM_INTERFACE),
    };
    let device = rig.usb.state();
    assert_eq!(device.claimed, vec![CONTROL_INTERFACE, STREAM_INTERFACE]);
    assert_eq!(device.alternates, vec![(STREAM_INTERFACE, 2)]);
    assert_eq!(device.controls.len(), 3);

    let mut expected_probe = vec![0u8; 34];
    expected_probe[0] = 1;
    expected_probe[2] = 1;
    expected_probe[3] = 1;
    expected_probe[4..8].copy_from_slice(&666_666u32.to_le_bytes());
    assert_eq!(
        device.controls[0],
        Control {
            setup: setup(OUT, SET_CUR, 1),
            data: expected_probe.clone(),
        }
    );
    assert_eq!(device.controls[1].setup, setup(IN, GET_CUR, 1));

    // The commit carries what the device answered.
    let mut expected_commit = expected_probe;
    expected_commit[12..14].copy_from_slice(&5000u16.to_le_bytes());
    expected_commit[18..22].copy_from_slice(&153_600u32.to_le_bytes());
    expected_commit[22..26].copy_from_slice(&900u32.to_le_bytes());
    assert_eq!(
        device.controls[2],
        Control {
            setup: setup(OUT, SET_CUR, 2),
            data: expected_commit,
        }
    );
    drop(device);
    assert_eq!(rig.uvc.session.state(), STREAMING);
}

#[test]
fn the_alternate_setting_follows_the_bandwidth_the_device_asks_for() {
    for (payload, alternate) in [(0, 1), (192, 1), (900, 2), (5000, 3)] {
        let mut rig = Rig::webcam(4096);
        rig.usb.state().max_payload = payload;
        rig.settle();
        rig.uvc.select_format(MODE_MJPEG).unwrap();
        rig.uvc.start().unwrap();
        rig.settle_streaming();
        assert_eq!(
            rig.usb.state().alternates,
            vec![(STREAM_INTERFACE, alternate)]
        );
    }
}

#[test]
fn stopping_turns_the_camera_off() {
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.uvc.stop().unwrap();
    rig.settle();
    assert_eq!(rig.uvc.session.state(), IDLE);
    assert_eq!(
        rig.usb.state().alternates,
        vec![(STREAM_INTERFACE, 2), (STREAM_INTERFACE, 0)]
    );
    assert_eq!(rig.tick(), None, "nothing is due until something changes");
}

#[test]
fn a_probe_reply_for_another_format_is_refused_and_retried() {
    let mut rig = Rig::webcam(4096);
    rig.settle();
    rig.usb.state().answer_format = Some(7);
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle();
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Failed(session::CHANGED_MODE)
    );
    assert_eq!(rig.usb.state().controls.len(), 2, "no commit was sent");
    rig.usb.state().answer_format = None;
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(rig.uvc.session.state(), STREAMING);
}

#[test]
fn a_probe_reply_too_short_to_read_is_refused() {
    let mut rig = Rig::webcam(4096);
    rig.settle();
    rig.usb.state().short_probe = Some(25);
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle();
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Failed(session::BAD_PROBE)
    );
    assert_eq!(rig.usb.state().controls.len(), 2);
}

#[test]
fn a_short_write_of_the_probe_is_refused() {
    let mut rig = Rig::webcam(4096);
    rig.settle();
    rig.usb.state().short_probe_write = Some(10);
    rig.uvc.select_format(MODE_MJPEG).unwrap();
    rig.uvc.start().unwrap();
    rig.settle();
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Failed(session::SHORT_TRANSFER)
    );
}

/// Waits out the backoffs in `waits`, in order, with the claim failing each time.
fn expect_backoffs(rig: &mut Rig, waits: &[u64]) {
    let mut claims = rig.usb.state().claims;
    for wait in waits {
        rig.advance(wait - 1);
        rig.tick();
        assert_eq!(rig.usb.state().claims, claims, "retried before {wait}");
        rig.advance(1);
        rig.tick();
        claims += 1;
        assert_eq!(rig.usb.state().claims, claims, "not retried at {wait}");
    }
}

#[test]
fn the_hosts_own_driver_is_detached_and_the_claim_then_succeeds() {
    let mut rig = Rig::webcam(4096);
    rig.usb.bind_kernel_driver(CONTROL_INTERFACE);
    rig.usb.bind_kernel_driver(STREAM_INTERFACE);
    rig.settle();
    let device = rig.usb.state();
    assert_eq!(device.detaches, vec![CONTROL_INTERFACE, STREAM_INTERFACE]);
    assert!(device.kernel_drivers.is_empty());
    assert_eq!(device.claimed, vec![CONTROL_INTERFACE, STREAM_INTERFACE]);
    assert_eq!(device.claims, 2, "each claim went through the first time");
    drop(device);
    assert_eq!(rig.uvc.session.state(), IDLE);
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Ok);
}

#[test]
fn a_detach_that_fails_is_reported_and_retried() {
    let mut rig = Rig::webcam(4096);
    rig.usb.bind_kernel_driver(STREAM_INTERFACE);
    rig.usb.state().detach_errors.push_back(ErrorKind::Failed);
    rig.settle();
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(DETACH_FAILED));
    assert_eq!(
        rig.usb.state().claims,
        0,
        "nothing is claimed before both are detached"
    );
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(
        rig.usb.state().claimed,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(rig.uvc.session.state(), IDLE);
}

#[test]
fn a_claim_that_half_succeeds_carries_on_from_where_it_stopped() {
    let mut rig = Rig::webcam(4096);
    // Another handle holds the streaming interface, so only the control interface is taken.
    rig.usb.state().claimed.push(STREAM_INTERFACE);
    rig.settle();
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(CLAIM_BUSY));
    assert_eq!(
        rig.usb.state().claimed,
        vec![STREAM_INTERFACE, CONTROL_INTERFACE]
    );
    // The other holder lets go. The control interface isn't claimed a second time.
    rig.usb.state().claimed.retain(|i| *i != STREAM_INTERFACE);
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(
        rig.usb.state().claimed,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(rig.uvc.session.state(), IDLE);
}

#[test]
fn an_interface_held_by_someone_else_is_reported_and_retried_for_ever() {
    let mut rig = Rig::webcam(4096);
    let mut errors = std::collections::VecDeque::new();
    errors.extend([ErrorKind::Busy; 12]);
    rig.usb.state().claim_errors = errors;
    rig.settle();
    assert_eq!(rig.usb.state().claims, 1);
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(CLAIM_BUSY));
    assert!(rig.describe().iter().any(|item| item.name == CLAIM_BUSY));
    // Formats are readable without the claim: reading descriptors doesn't need it.
    assert_eq!(rig.uvc.format_count(), Ok(6));

    let (first, cap) = (BACKOFF_FIRST_NS, BACKOFF_CAP_NS);
    expect_backoffs(
        &mut rig,
        &[
            first,
            first * 2,
            first * 4,
            cap,
            cap,
            cap,
            cap,
            cap,
            cap,
            cap,
            cap,
        ],
    );
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(CLAIM_BUSY));

    // The other holder lets go, and the next retry takes it.
    rig.advance(cap);
    rig.settle();
    assert_eq!(
        rig.usb.state().claimed,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(rig.uvc.session.state(), IDLE);
}

#[test]
fn a_camera_unplugged_mid_stream_is_retried_for_ever_and_answers_no_device() {
    let mut rig = Rig::streaming(4096, MODE_QVGA_MIN);
    rig.usb.unplug();
    rig.tick();
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(GONE));
    assert!(rig.describe().iter().any(|item| item.name == GONE));
    // Every method but the stream's name says the device is gone, which is what makes the host
    // drop this driver and open the device again by name.
    assert_eq!(rig.uvc.counters(), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.format_count(), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.format(0), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.select_format(0), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.selected(), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.start(), Err(Status::NO_DEVICE));
    assert_eq!(rig.uvc.stop(), Err(Status::NO_DEVICE));
    let mut name = [0u8; 32];
    assert_eq!(rig.uvc.frame_stream(&mut name), Ok(6));

    // It never gives up: a read of the descriptors at each backoff, never more than 8 s apart.
    let (first, cap) = (BACKOFF_FIRST_NS, BACKOFF_CAP_NS);
    for wait in [first, first * 2, first * 4, cap, cap, cap, cap, cap, cap] {
        let due = rig.tick().unwrap();
        assert_eq!(due - rig.now_ns, wait);
        rig.now_ns = due;
        rig.tick();
        assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(GONE));
        assert_eq!(rig.uvc.counters(), Err(Status::NO_DEVICE));
    }
}

#[test]
fn a_camera_that_answers_again_is_read_afresh_detached_and_claimed() {
    // A handle that outlives its device going: everything is done again from the start.
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.usb.state().gone = true;
    rig.tick();
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(GONE));
    {
        let mut device = rig.usb.state();
        device.gone = false;
        device.claimed.clear();
        device.kernel_drivers = vec![CONTROL_INTERFACE, STREAM_INTERFACE];
        device.detaches.clear();
    }
    let reads = rig.usb.state().descriptor_reads;
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(rig.usb.state().descriptor_reads, reads + 1);
    assert_eq!(
        rig.usb.state().detaches,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(rig.uvc.session.state(), STREAMING, "it was still wanted");
    assert_eq!(rig.uvc.counters().map(|c| c.frames), Ok(0));
}

#[test]
fn malformed_descriptors_are_retried_and_counted() {
    let mut rig = Rig::new(vec![0u8; 2], 4096, 2);
    rig.settle();
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Failed(descriptors::BAD_CONFIG)
    );
    assert_eq!(rig.uvc.format_count(), Err(Status::AGAIN));
    rig.usb.state().descriptors = fixture::webcam().bytes;
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(rig.uvc.format_count(), Ok(6));
    assert_eq!(rig.uvc.session.malformed_descriptors(), 0);
}

#[test]
fn a_stop_that_fails_is_retried() {
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.usb
        .state()
        .alternate_errors
        .push_back(ErrorKind::Failed);
    rig.uvc.stop().unwrap();
    rig.settle();
    assert_eq!(
        Lifecycle::health(&rig.uvc),
        Health::Failed(session::TRANSFER_FAILED)
    );
    assert_eq!(
        rig.usb.state().alternates.last(),
        Some(&(STREAM_INTERFACE, 2))
    );
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(
        rig.usb.state().alternates.last(),
        Some(&(STREAM_INTERFACE, 0))
    );
    assert_eq!(rig.uvc.session.state(), IDLE);
}

#[test]
fn a_timed_out_transfer_restarts_the_stream_after_a_backoff() {
    let mut rig = Rig::streaming(4096, MODE_MJPEG);
    rig.usb.state().iso_errors.push_back(ErrorKind::Timeout);
    rig.tick();
    assert_eq!(Lifecycle::health(&rig.uvc), Health::Failed(session::SILENT));
    rig.advance(BACKOFF_FIRST_NS);
    rig.settle();
    assert_eq!(rig.uvc.session.state(), STREAMING);
    assert_eq!(
        rig.usb.state().alternates.last(),
        Some(&(STREAM_INTERFACE, 2))
    );
}

#[test]
fn a_tick_that_comes_early_does_nothing() {
    let mut rig = Rig::webcam(4096);
    rig.usb.state().claim_errors.push_back(ErrorKind::Busy);
    rig.settle();
    let claims = rig.usb.state().claims;
    let due = rig.tick().unwrap();
    assert_eq!(due, rig.now_ns + BACKOFF_FIRST_NS);
    assert_eq!(rig.usb.state().claims, claims);
}
