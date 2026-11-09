//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Health, Lifecycle, Result, Status};
use lemnos_measure::test_alloc::allocations;

use crate::hw::{Hardware, StartParams};
use crate::{Rp1Csi2, StreamName as Name, backoff_ns};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[derive(Default)]
struct Mock {
    started: Vec<StartParams>,
    stopped: u32,
    fail_start: Option<Status>,
    fail_check: Option<Status>,
    max_lanes: u8,
    counters: Counters,
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

impl Hardware for Hw {
    fn max_lanes(&mut self) -> Result<u8> {
        Ok(self.with(|m| m.max_lanes))
    }

    fn start(&mut self, params: &StartParams) -> Result<()> {
        self.with(|m| match m.fail_start {
            Some(status) => Err(status),
            None => {
                m.started.push(*params);
                Ok(())
            }
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.with(|m| m.stopped += 1);
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        self.with(|m| match m.fail_check.take() {
            Some(status) => Err(status),
            None => Ok(()),
        })
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.with(|m| m.counters))
    }
}

fn driver() -> (Rp1Csi2, Hw) {
    let hw = Hw::default();
    hw.with(|m| m.max_lanes = 2);
    let name = Name::new("frames").unwrap();
    (Rp1Csi2::with_hardware(Box::new(hw.clone()), 8, name), hw)
}

/// The sensor's mode on the board: 1280x800 RAW10 over 2 lanes at 800 Mbit/s each.
fn raw10() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: 1280,
        height: 800,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

#[test]
fn limits_come_from_the_block() {
    let (mut d, _hw) = driver();
    let limits = d.limits().unwrap();
    assert_eq!(limits.max_lanes, 2);
    assert_eq!(limits.max_link_rate_mbps, 1500);
    assert_eq!(limits.max_width, 65535);
    assert_eq!(limits.max_height, 65535);
}

#[test]
fn configure_works_out_the_layout_the_board_was_measured_at() {
    let (mut d, _hw) = driver();
    assert_eq!(
        d.configure(raw10(), Packing::Packed).unwrap(),
        FrameLayout {
            width: 1280,
            height: 800,
            bit_depth: 10,
            packing: Packing::Packed,
            stride: 1600,
            size: 1_280_000,
        }
    );
    assert_eq!(
        d.configure(raw10(), Packing::Unpacked16).unwrap(),
        FrameLayout {
            width: 1280,
            height: 800,
            bit_depth: 10,
            packing: Packing::Unpacked16,
            stride: 2560,
            size: 2_048_000,
        }
    );
    assert_eq!(d.link().unwrap(), Some(raw10()));
}

#[test]
fn configure_aligns_the_stride_to_sixteen() {
    let (mut d, _hw) = driver();
    let link = LinkConfig {
        width: 1001,
        height: 3,
        bit_depth: 8,
        data_type: 0x2a,
        ..raw10()
    };
    let got = d.configure(link, Packing::Packed).unwrap();
    assert_eq!((got.stride, got.size), (1008, 3024));
}

#[test]
fn configure_refuses_what_the_block_cannot_take() {
    let (mut d, _hw) = driver();
    let mut bad = |f: &dyn Fn(&mut LinkConfig), packing: Packing| {
        let mut link = raw10();
        f(&mut link);
        assert_eq!(
            d.configure(link, packing).unwrap_err(),
            Status::INVALID_ARGUMENT
        );
    };
    bad(&|l| l.lanes = 0, Packing::Packed);
    bad(&|l| l.lanes = 3, Packing::Packed); // two wired
    bad(&|l| l.virtual_channel = 4, Packing::Packed);
    bad(&|l| l.data_type = 0, Packing::Packed);
    bad(&|l| l.data_type = 64, Packing::Packed);
    bad(&|l| l.link_rate_mbps = 79, Packing::Packed);
    bad(&|l| l.link_rate_mbps = 1501, Packing::Packed);
    bad(&|l| l.width = 0, Packing::Packed);
    bad(&|l| l.width = 65536, Packing::Packed);
    bad(&|l| l.height = 0, Packing::Packed);
    bad(&|l| l.height = 65536, Packing::Packed);
    bad(&|l| l.bit_depth = 9, Packing::Packed);
    // RAW8 unpacked is something the block can't do; RAW10 is.
    bad(&|l| l.bit_depth = 8, Packing::Unpacked16);
}

#[test]
fn configure_refuses_a_frame_past_the_buffer_limit() {
    let (mut d, _hw) = driver();
    let link = LinkConfig {
        bit_depth: 16,
        data_type: 0x2b,
        width: 65535,
        height: 65535,
        ..raw10()
    };
    assert_eq!(
        d.configure(link, Packing::Packed).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
}

#[test]
fn start_requires_configure_and_passes_the_link_exactly() {
    let (mut d, hw) = driver();
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(
        hw.with(|m| m.started.clone()),
        vec![StartParams {
            lanes: 2,
            mbps: 800,
            vc: 0,
            data_type: 0x2b,
            width: 1280,
            height: 800,
            stride: 1600,
            size: 1_280_000,
            unpack16: false,
            buffers: 8,
            frame_slots: 8,
        }]
    );
    assert_eq!(hw.with(|m| m.stopped), 1, "starts from nothing");
}

#[test]
fn unpacked16_asks_the_block_to_unpack() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    let params = hw.with(|m| m.started[0]);
    assert!(params.unpack16);
    assert_eq!((params.stride, params.size), (2560, 2_048_000));
}

#[test]
fn a_stream_cannot_be_configured_or_started_twice() {
    let (mut d, _hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(
        d.configure(raw10(), Packing::Packed).unwrap_err(),
        Status::BUSY
    );
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
}

#[test]
fn frame_stream_gives_the_configured_name_or_refuses_a_short_buffer() {
    let (mut d, _hw) = driver();
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.frame_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"frames");
    let mut short = [0u8; 2];
    assert_eq!(
        d.frame_stream(&mut short).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}

#[test]
fn name_is_bounded() {
    assert!(Name::new("").is_none());
    assert!(Name::new(&"n".repeat(MAX_STREAM_NAME + 1)).is_none());
    assert!(Name::new(&"n".repeat(MAX_STREAM_NAME)).is_some());
}

#[test]
fn counters_are_the_blocks() {
    let (mut d, hw) = driver();
    let counters = Counters {
        frames: 7,
        dropped: 2,
        errors: 1,
    };
    hw.with(|m| m.counters = counters);
    assert_eq!(d.counters().unwrap(), counters);
}

#[test]
fn a_block_that_stops_is_restarted_with_capped_backoff() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(d.health(), Health::Ok);

    // A tick takes the clock, then the next checks the block.
    let mut now = 0u64;
    let at = d.tick(now).unwrap();
    assert_eq!(at, 1_000_000_000);
    now = at;
    hw.with(|m| m.fail_check = Some(Status::IO));
    // It failed: recovery starts, with its first delay.
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 250_000_000);
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.start().unwrap_err(), Status::BUSY, "while recovering");

    hw.with(|m| m.fail_start = Some(Status::IO));
    let mut expected = [
        500_000_000u64,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        8_000_000_000,
        8_000_000_000,
        8_000_000_000,
    ]
    .into_iter();
    now = at;
    for delay in &mut expected {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, delay, "backoff capped at 8 s");
        now = at;
    }
    // Forever: it never gives up and never turns itself off.
    for _ in 0..100 {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, 8_000_000_000);
        now = at;
        assert!(matches!(d.health(), Health::Degraded(_)));
    }

    // And it clears when an attempt works.
    hw.with(|m| m.fail_start = None);
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 1_000_000_000);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn stop_ends_recovery() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let t = d.tick(0).unwrap();
    hw.with(|m| m.fail_check = Some(Status::IO));
    let _ = d.tick(t);
    assert!(matches!(d.health(), Health::Degraded(_)));
    d.stop().unwrap();
    assert_eq!(d.tick(u64::MAX), None, "stopped: nothing is due");
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn a_failed_start_is_reported_and_retried_by_its_caller() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    hw.with(|m| m.fail_start = Some(Status::NO_DEVICE));
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
    assert_eq!(
        d.tick(0),
        None,
        "no background retries of a start not begun"
    );
    assert_eq!(d.health(), Health::Ok);
    hw.with(|m| m.fail_start = None);
    d.start().unwrap();
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(1), 500_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let mut now = d.tick(0).unwrap();
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            hw.with(|m| m.counters.frames += 1);
            let counters = d.counters().unwrap();
            assert_eq!(counters.dropped, 0);
            now = d.tick(now).unwrap();
        }
    });
    assert_eq!(allocated, 0, "counters and the health check");
}
