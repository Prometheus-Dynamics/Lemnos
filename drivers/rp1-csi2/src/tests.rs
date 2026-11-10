//! The driver against a stand-in for the block: every expected value here is written out by hand.

use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Health, Lifecycle, Result, Status, StreamHandle};
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;

use crate::hw::{Finished, Hardware, StartParams};
use crate::relay::Set;
use crate::{Rp1Csi2, backoff_ns};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[derive(Default)]
struct Mock {
    /// Streams made, and how many of them were then started.
    made: Vec<StartParams>,
    started: u32,
    stopped: u32,
    fail_start: Option<Status>,
    fail_check: Option<Status>,
    max_lanes: u8,
    counters: Counters,
    /// Buffers given memory on the stream now, in order; cleared when a stream is made.
    set: Vec<u32>,
    /// Buffers given memory before the block was started.
    set_before_go: usize,
    /// What the module says to the next memory offered for a buffer.
    answer: Option<(u32, Set)>,
    finished: Vec<Finished>,
    given_back: Vec<(u32, u32)>,
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

    fn make(&mut self, params: &StartParams) -> Result<()> {
        self.with(|m| match m.fail_start {
            Some(status) => Err(status),
            None => {
                m.made.push(*params);
                m.set.clear();
                Ok(())
            }
        })
    }

    fn go(&mut self) -> Result<()> {
        self.with(|m| {
            m.started += 1;
            m.set_before_go = m.set.len();
        });
        Ok(())
    }

    fn set_buffer(&mut self, buffer: u32, _memory: BorrowedFd<'_>) -> Result<Set> {
        Ok(self.with(|m| match m.answer {
            Some((b, answer)) if b == buffer => answer,
            _ => {
                m.set.push(buffer);
                Set::Done
            }
        }))
    }

    fn next_frame(&mut self) -> Result<Option<Finished>> {
        Ok(self.with(|m| {
            if m.finished.is_empty() {
                None
            } else {
                Some(m.finished.remove(0))
            }
        }))
    }

    fn give_back(&mut self, buffer: u32, fence: u32) -> Result<()> {
        self.with(|m| m.given_back.push((buffer, fence)));
        Ok(())
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

/// A `frames` stream of `buffers` buffers, each behind a handle as a dma-buf stream's are.
fn stream(buffers: u32) -> MockStream {
    let memory = (0..buffers).map(|_| sealed_memfd(c"frames", 4096).unwrap());
    MockStream::with_handles(memory.collect())
}

fn driver_on(frames: &MockStream) -> (Rp1Csi2, Hw) {
    let hw = Hw::default();
    hw.with(|m| {
        m.max_lanes = 2;
        m.finished.reserve(64);
        m.given_back.reserve(4096);
        m.set.reserve(4096);
    });
    let frames: StreamHandle = Box::new(frames.clone());
    let driver = Rp1Csi2::with_hardware(Box::new(hw.clone()), Some(frames));
    (driver, hw)
}

fn driver() -> (Rp1Csi2, Hw) {
    driver_on(&stream(8))
}

fn frame(buffer: u32, sequence: u64, fence: u32) -> Finished {
    Finished {
        buffer,
        bytes: 1_280_000,
        sequence,
        timestamp_ns: 1000 + sequence,
        fence,
    }
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
        hw.with(|m| m.made.clone()),
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
    assert_eq!(hw.with(|m| m.started), 1);
    assert_eq!(
        hw.with(|m| (m.set.clone(), m.set_before_go)),
        ((0..8).collect(), 8),
        "every buffer of the stream had its memory before the block was started"
    );
}

#[test]
fn unpacked16_asks_the_block_to_unpack() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    let params = hw.with(|m| m.made[0]);
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
fn without_a_frames_stream_there_is_nothing_to_start() {
    let hw = Hw::default();
    hw.with(|m| m.max_lanes = 2);
    let mut d = Rp1Csi2::with_hardware(Box::new(hw.clone()), None);
    d.configure(raw10(), Packing::Packed).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::NOT_FOUND);
    let mut out = [0u8; MAX_STREAM_NAME];
    assert_eq!(d.frame_stream(&mut out).unwrap_err(), Status::NOT_FOUND);
    assert_eq!(hw.with(|m| m.made.len()), 0, "the block was never touched");
    assert_eq!(d.tick(0), None);
}

#[test]
fn a_stream_whose_buffers_have_no_handles_is_refused() {
    // Plain shared memory: there is nothing to give the block.
    let frames = MockStream::with_bytes(4, 4096);
    let (mut d, hw) = driver_on(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(hw.with(|m| (m.started, m.set.len())), (0, 0));
    assert!(hw.with(|m| m.stopped) >= 2, "what was made is ended again");
    assert_eq!(d.tick(0), None, "not streaming");
    assert!(frames.take_frame().is_none());
}

#[test]
fn memory_the_module_refuses_is_a_refused_start() {
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    hw.with(|m| m.answer = Some((2, Set::Refused)));
    d.configure(raw10(), Packing::Packed).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(hw.with(|m| m.started), 0);
}

#[test]
fn a_finished_frame_is_published_and_its_buffer_goes_back_when_returned() {
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    hw.with(|m| m.finished.push(frame(2, 7, 31)));
    let now = d.tick(0).unwrap();
    assert_eq!(now, 1_000_000, "frames are looked for every millisecond");
    let got = frames.take_frame().expect("the frame is on the stream");
    assert_eq!(
        (got.buffer, got.bytes, got.sequence, got.timestamp_ns),
        (2, 1_280_000, 7, 1007)
    );
    assert!(frames.take_frame().is_none());
    assert_eq!(
        hw.with(|m| m.given_back.clone()),
        [],
        "the consumer has it: the block doesn't"
    );

    frames.return_buffer(2);
    d.tick(now).unwrap();
    assert_eq!(
        hw.with(|m| m.given_back.clone()),
        [(2, 31)],
        "back to the block, with the fence its frame carried"
    );
    assert_eq!(
        hw.with(|m| m.set.clone()),
        [0, 1, 2, 3, 2],
        "and only after the module was shown the memory behind it again"
    );
}

#[test]
fn a_buffer_the_block_is_writing_keeps_its_memory_until_its_frame_is_done() {
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    // The module won't take buffer 1's memory: the block has it.
    hw.with(|m| m.answer = Some((1, Set::Busy)));
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(hw.with(|m| m.set.clone()), [0, 2, 3]);

    // Its frame finishes, in memory that isn't the stream's: nobody may see it.
    hw.with(|m| {
        m.answer = None;
        m.finished.push(frame(1, 0, 5));
    });
    let now = d.tick(0).unwrap();
    assert!(frames.take_frame().is_none(), "not published");
    assert_eq!(
        hw.with(|m| (m.set.clone(), m.given_back.clone())),
        (vec![0, 2, 3, 1], vec![(1, 5)]),
        "the buffer got the stream's memory, and only then went back to the block"
    );
    assert_eq!(d.counters().unwrap().dropped, 1);

    // The next frame on it is the consumer's to read.
    hw.with(|m| m.finished.push(frame(1, 1, 6)));
    d.tick(now).unwrap();
    assert_eq!(
        frames.take_frame().map(|f| (f.buffer, f.sequence)),
        Some((1, 1))
    );
}

#[test]
fn a_consumer_that_leaves_gets_nothing_more_and_the_next_one_gets_its_own_memory() {
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    hw.with(|m| m.finished.push(frame(0, 0, 1)));
    let mut now = d.tick(0).unwrap();
    assert_eq!(frames.take_frame().map(|f| f.buffer), Some(0));

    // The consumer goes with buffer 0 out and the block writing buffer 1.
    frames.change_consumer();
    hw.with(|m| {
        m.set.clear();
        m.answer = Some((1, Set::Busy));
    });
    now = d.tick(now).unwrap();
    // Every hold was on the old consumer's memory: given up, taken again, and the module
    // shown the new memory. Buffer 0 came back with the consumer, so it also goes back to the
    // block.
    assert_eq!(hw.with(|m| m.set.clone()), [0, 2, 3]);
    assert_eq!(hw.with(|m| m.given_back.clone()), [(0, 1)]);
    assert!(frames.take_frame().is_none());

    // The frame the block was writing finishes in the old memory: dropped, and swapped.
    hw.with(|m| {
        m.answer = None;
        m.finished.push(frame(1, 1, 9));
    });
    now = d.tick(now).unwrap();
    assert!(
        frames.take_frame().is_none(),
        "the new consumer never sees it"
    );
    assert_eq!(hw.with(|m| m.set.clone()), [0, 2, 3, 1]);
    assert_eq!(hw.with(|m| m.given_back.clone()), [(0, 1), (1, 9)]);
    assert_eq!(d.counters().unwrap().dropped, 1);

    hw.with(|m| m.finished.push(frame(1, 2, 10)));
    d.tick(now).unwrap();
    assert_eq!(
        frames.take_frame().map(|f| (f.buffer, f.sequence)),
        Some((1, 2))
    );
    assert_eq!(
        hw.with(|m| (m.made.len(), m.stopped)),
        (1, 1),
        "the block was never stopped"
    );
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

    // A tick takes the clock; the block is checked a second later.
    assert_eq!(d.tick(0).unwrap(), 1_000_000);
    hw.with(|m| m.fail_check = Some(Status::IO));
    assert_eq!(d.tick(999_999_999).unwrap(), 1_000_000_000, "not yet");
    assert_eq!(d.health(), Health::Ok);
    let mut now = 1_000_000_000u64;
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
    assert_eq!(at, now + 1_000_000);
    assert_eq!(d.health(), Health::Ok);
}

#[test]
fn stop_ends_recovery() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    d.tick(0).unwrap();
    hw.with(|m| m.fail_check = Some(Status::IO));
    let _ = d.tick(1_000_000_000);
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
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let mut now = d.tick(0).unwrap();
    let mut sequence = 0u64;
    // A frame finished, published, read, returned and given back to the block, every tick; the
    // module's state read every second of them.
    let mut cycle = |d: &mut Rp1Csi2, now: &mut u64| {
        let buffer = (sequence % 4) as u32;
        hw.with(|m| {
            m.counters.frames += 1;
            m.finished.push(frame(buffer, sequence, sequence as u32));
            m.set.clear();
            m.given_back.clear();
        });
        *now = d.tick(*now).unwrap();
        let got = frames.take_frame().expect("published");
        assert_eq!((got.buffer, got.sequence), (buffer, sequence));
        frames.return_buffer(buffer);
        *now = d.tick(*now + 500_000_000).unwrap();
        assert_eq!(d.counters().unwrap().dropped, 0);
        sequence += 1;
    };
    for _ in 0..16 {
        cycle(&mut d, &mut now);
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            cycle(&mut d, &mut now);
        }
    });
    assert_eq!(
        allocated, 0,
        "frames, returns, counters and the health check"
    );
}
