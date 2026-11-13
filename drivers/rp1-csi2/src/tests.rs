//! The driver against a stand-in for the block: every expected value here is written out by hand.

use lemnos_core::CallerId;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, FrameStarts, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Health, Lifecycle, Result, Status, StreamHandle};
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;

use crate::hw::{Channel, Finished, Hardware, StartParams};
use crate::relay::Set;
use crate::{Rp1Csi2, backoff_ns};
use lemnos_driver_rp1_csi2_sys as sys;

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
    /// Whether the block is receiving, and the frame starts it has counted since it went.
    receiving: bool,
    starts: FrameStarts,
    /// The embedded stream's, the same way: its buffers given memory, what the module says to
    /// one, its finished lines, and what was given back.
    emb_set: Vec<u32>,
    emb_answer: Option<(u32, Set)>,
    emb_finished: Vec<Finished>,
    emb_given_back: Vec<(u32, u32)>,
}

impl Mock {
    /// The block raises a frame start at `at_ns`: counted while receiving. (The address-less case
    /// is the integration stand-in's, in `tests/frames.rs`.)
    fn frame_start(&mut self, at_ns: u64) {
        if self.receiving {
            self.starts.count += 1;
            self.starts.latest_ns = at_ns;
        }
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>, Option<Arc<OwnedFd>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

impl Hardware for Hw {
    fn doorbell(&self) -> Option<BorrowedFd<'_>> {
        self.1.as_deref().map(AsFd::as_fd)
    }

    fn max_lanes(&mut self) -> Result<u8> {
        Ok(self.with(|m| m.max_lanes))
    }

    fn make(&mut self, params: &StartParams) -> Result<()> {
        self.with(|m| match m.fail_start {
            Some(status) => Err(status),
            None => {
                m.made.push(*params);
                m.set.clear();
                m.emb_set.clear();
                Ok(())
            }
        })
    }

    fn go(&mut self) -> Result<()> {
        self.with(|m| {
            m.started += 1;
            m.set_before_go = m.set.len();
            m.receiving = true;
            m.starts = FrameStarts::default();
        });
        Ok(())
    }

    fn set_buffer(
        &mut self,
        channel: Channel,
        buffer: u32,
        _memory: BorrowedFd<'_>,
    ) -> Result<Set> {
        Ok(self.with(|m| {
            let (answer, set) = match channel {
                Channel::Image => (m.answer, &mut m.set),
                Channel::Embedded => (m.emb_answer, &mut m.emb_set),
            };
            match answer {
                Some((b, answer)) if b == buffer => answer,
                _ => {
                    set.push(buffer);
                    Set::Done
                }
            }
        }))
    }

    fn next_frame(&mut self, channel: Channel) -> Result<Option<Finished>> {
        Ok(self.with(|m| {
            let finished = match channel {
                Channel::Image => &mut m.finished,
                Channel::Embedded => &mut m.emb_finished,
            };
            if finished.is_empty() {
                None
            } else {
                Some(finished.remove(0))
            }
        }))
    }

    fn give_back(&mut self, channel: Channel, buffer: u32, fence: u32) -> Result<()> {
        self.with(|m| match channel {
            Channel::Image => m.given_back.push((buffer, fence)),
            Channel::Embedded => m.emb_given_back.push((buffer, fence)),
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.with(|m| {
            m.stopped += 1;
            m.receiving = false;
        });
        Ok(())
    }

    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Ok(self.with(|m| m.receiving.then_some(m.starts)))
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
    let frames: StreamHandle = lemnos_core::Held::boxed(Box::new(frames.clone()));
    let driver = Rp1Csi2::with_hardware(Box::new(hw.clone()), Some(frames));
    (driver, hw)
}

fn driver() -> (Rp1Csi2, Hw) {
    driver_on(&stream(8))
}

/// The same driver, with a doorbell: an eventfd-like descriptor the block's streams ring. The
/// peer end is kept by nothing, so the descriptor is only ever the driver's to wait on.
fn driver_with_bell(frames: &MockStream) -> (Rp1Csi2, Hw) {
    let (bell, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let hw = Hw(Arc::default(), Some(Arc::new(OwnedFd::from(bell))));
    hw.with(|m| {
        m.max_lanes = 2;
        m.finished.reserve(64);
        m.given_back.reserve(4096);
        m.set.reserve(4096);
    });
    let frames: StreamHandle = lemnos_core::Held::boxed(Box::new(frames.clone()));
    let driver = Rp1Csi2::with_hardware(Box::new(hw.clone()), Some(frames));
    (driver, hw)
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
            embedded_buffers: 0,
            embedded_frame_slots: 0,
            embedded_bytes: 0,
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
    let stopped = hw.with(|m| m.stopped);
    assert_eq!(d.start().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(hw.with(|m| (m.started, m.set.len())), (0, 0));
    assert_eq!(
        hw.with(|m| m.stopped),
        stopped,
        "nothing was made or touched"
    );
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
fn with_a_doorbell_a_streaming_driver_ticks_at_the_fallback_period_not_each_millisecond() {
    let frames = stream(4);
    let (mut d, _hw) = driver_with_bell(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(
        d.tick(0).unwrap(),
        33_000_000,
        "one frame period: the bell wakes the driver for each frame"
    );
    assert_eq!(
        d.tick(5_000_000).unwrap(),
        38_000_000,
        "a wake in between is ticked at once; the next deadline is a period from it"
    );
    assert_eq!(d.lost_wakes(), 0);
}

#[test]
fn a_deadline_tick_that_finds_frames_counts_them_as_lost_wakes_and_a_wake_does_not() {
    let frames = stream(4);
    let (mut d, hw) = driver_with_bell(&frames);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(d.tick(0).unwrap(), 33_000_000);

    // The bell rang for this frame: a tick before the deadline finds it, and that is no lost wake.
    hw.with(|m| m.finished.push(frame(1, 0, 5)));
    assert_eq!(d.tick(4_000_000).unwrap(), 37_000_000);
    assert_eq!(d.lost_wakes(), 0, "a wake brought this tick");

    // Two frames arrive with no wake: the tick at the deadline finds them, and counts them.
    hw.with(|m| {
        m.finished.push(frame(2, 1, 6));
        m.finished.push(frame(3, 2, 7));
    });
    assert_eq!(d.tick(37_000_000).unwrap(), 70_000_000);
    assert_eq!(
        d.lost_wakes(),
        2,
        "the doorbell should have woken the driver for them"
    );
    assert_eq!(frames.take_frame().map(|f| f.sequence), Some(0));
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
fn frame_starts_is_unknown_until_started_then_counts_the_blocks_starts() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    assert_eq!(
        d.frame_starts().unwrap(),
        None,
        "before START nothing is counted: unknown, not zero"
    );
    d.start().unwrap();
    assert_eq!(
        d.frame_starts().unwrap(),
        Some(FrameStarts {
            count: 0,
            latest_ns: 0,
        }),
        "started: an empty count, no start yet"
    );
    hw.with(|m| {
        m.frame_start(5_000);
        m.frame_start(9_000);
    });
    assert_eq!(
        d.frame_starts().unwrap(),
        Some(FrameStarts {
            count: 2,
            latest_ns: 9_000,
        }),
        "two starts counted, the latest at its own time"
    );
    d.stop().unwrap();
    assert_eq!(
        d.frame_starts().unwrap(),
        None,
        "stopped: unknown again, whatever the block last counted"
    );
}

#[test]
fn frames_lost_before_a_start_are_not_counted_after_it() {
    let frames = stream(4);
    let (mut d, hw) = driver_on(&frames);
    hw.with(|m| m.answer = Some((1, Set::Busy)));
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    hw.with(|m| {
        m.answer = None;
        m.finished.push(frame(1, 0, 5));
    });
    d.tick(0).unwrap();
    assert_eq!(d.counters().unwrap().dropped, 1);

    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(
        d.counters().unwrap().dropped,
        0,
        "counted since the last start, as the block's own are"
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

#[test]
fn no_isp_takes_the_frames_in_hardware() {
    let frames = MockStream::with_bytes(4, 4096);
    let (mut d, _hw) = driver_on(&frames);
    let mut name = [0xAAu8; 64];
    assert_eq!(CameraReceiver::feeds(&mut d, &mut name), Ok(0));
    assert_eq!(CameraReceiver::feeds(&mut d, &mut []), Ok(0));
}

#[test]
fn the_module_does_not_know_the_sensor_so_the_link_is_unknown() {
    let frames = MockStream::with_bytes(4, 4096);
    let (mut d, _hw) = driver_on(&frames);
    let mut name = [0u8; 64];
    assert_eq!(
        CameraReceiver::source(&mut d, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
}

#[test]
fn layout_is_what_configure_set_and_nothing_before() {
    let frames = MockStream::with_bytes(4, 4096);
    let (mut d, _hw) = driver_on(&frames);
    assert_eq!(CameraReceiver::layout(&mut d), Ok(None));
    let set = d.configure(raw10(), Packing::Packed).unwrap();
    assert_eq!(CameraReceiver::layout(&mut d), Ok(Some(set)));
}

/// The embedded data stream: four buffers of its own, a line of 256 bytes each.
fn embedded_on(frames: &MockStream, embedded: &MockStream, bytes: u32) -> (Rp1Csi2, Hw) {
    let (driver, hw) = driver_on(frames);
    let embedded: StreamHandle = lemnos_core::Held::boxed(Box::new(embedded.clone()));
    (driver.with_embedded(embedded, bytes), hw)
}

#[test]
fn the_embedded_stream_is_named_only_when_the_board_declares_one() {
    let (mut plain, _) = driver();
    let mut name = [0u8; 16];
    assert_eq!(plain.embedded_stream(&mut name), Ok(0));

    let (mut d, _) = embedded_on(&stream(8), &stream(4), 256);
    let n = d.embedded_stream(&mut name).unwrap();
    assert_eq!(&name[..n], b"embedded");
    assert_eq!(
        d.embedded_stream(&mut name[..3]),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

#[test]
fn start_makes_the_embedded_stream_and_gives_its_buffers_memory() {
    let frames = stream(8);
    let embedded = stream(4);
    let (mut d, hw) = embedded_on(&frames, &embedded, 256);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(
        hw.with(|m| {
            let p = m.made[0];
            (p.embedded_buffers, p.embedded_frame_slots, p.embedded_bytes)
        }),
        (4, 4, 256),
        "the module makes the embedded stream with the board's buffers and the line's bytes"
    );
    assert_eq!(
        hw.with(|m| m.emb_set.clone()),
        [0, 1, 2, 3],
        "every embedded buffer has its memory before the block starts"
    );
}

#[test]
fn an_embedded_line_is_published_on_its_own_stream_with_its_frames_sequence() {
    let frames = stream(8);
    let embedded = stream(4);
    let (mut d, hw) = embedded_on(&frames, &embedded, 256);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    hw.with(|m| {
        m.emb_finished.push(Finished {
            buffer: 1,
            bytes: 256,
            sequence: 5,
            timestamp_ns: 2000,
            fence: 9,
        });
    });
    d.tick(0).unwrap();
    let got = embedded
        .take_frame()
        .expect("the line is on the embedded stream");
    assert_eq!((got.buffer, got.bytes, got.sequence), (1, 256, 5));
    assert!(
        frames.take_frame().is_none(),
        "the image stream got nothing"
    );
    assert!(embedded.take_frame().is_none());
}

#[test]
fn an_embedded_line_is_refused_without_a_length_the_channel_takes() {
    for bytes in [0, 24, sys::EMBEDDED_MAX + 16] {
        let (mut d, _) = embedded_on(&stream(8), &stream(4), bytes);
        d.configure(raw10(), Packing::Packed).unwrap();
        assert_eq!(
            d.start().unwrap_err(),
            Status::INVALID_ARGUMENT,
            "bytes {bytes}"
        );
    }
}

/// A client that starts the receiver and goes away leaves nothing running: the owner's going away
/// stops the block; another caller's leaves it running.
#[test]
fn a_caller_going_away_stops_the_receiver_it_started() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    Lifecycle::caller(&mut d, CallerId(7));
    d.start().unwrap();
    let before = hw.with(|m| m.stopped);

    Lifecycle::caller_gone(&mut d, CallerId(8));
    assert_eq!(
        hw.with(|m| m.stopped),
        before,
        "another caller's going away leaves it running"
    );
    Lifecycle::caller_gone(&mut d, CallerId(7));
    assert_eq!(
        hw.with(|m| m.stopped),
        before + 1,
        "the owner's going away stops the block"
    );
    Lifecycle::caller_gone(&mut d, CallerId(7));
    assert_eq!(
        hw.with(|m| m.stopped),
        before + 1,
        "and nothing more once it is stopped"
    );
}
