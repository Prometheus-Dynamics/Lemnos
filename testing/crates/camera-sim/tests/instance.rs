//! The in-process instance (`lemnos_camera_sim::Sim`): paced frames on the sensor's period, the
//! lock-step grant with its controls, the scenes, the buffers, the per-instance switches, and two
//! instances in one process sharing nothing. Each test starts its own service on its own socket.
//!
//! The numbers are the reference board's: a 1280 x 800 RAW10 frame and the sensor's mode 0. A sensor
//! at rest runs its clock at 96 MHz; once it streams, the driver's RAW10 mode runs it at 80 MHz. The
//! frame period is `(vts - 1) x hts + 0x05b0` clocks over that clock, where `vts` is the frame length
//! in lines and `hts` 728 clocks: 4395 lines at rest is 33 336 333 ns; 3664 lines streaming is
//! 33 351 500 ns (the chain proof's 30 fps); 915 lines streaming is 8 335 600 ns. A line is 9 100 ns.

use std::time::{Duration, Instant};

use lemnos_camera_sim::chain::SensorFault;
use lemnos_camera_sim::scene::{self, HEIGHT, RAW10_STRIDE, Scene, WIDTH};
use lemnos_camera_sim::sim::{MAX_EXPOSURE_US, MAX_GAIN_MILLI, Refusal};
use lemnos_camera_sim::{Control, Event, Memory, Pace, Sim, SimBuffers};
use lemnos_core::Status;

/// The frame period of a sensor at rest (96 MHz, 4395 lines), in ns.
const PERIOD_REST_NS: u64 = 33_336_333;
/// The frame period streaming at 3664 lines (30 fps, the chain proof's), in ns.
const PERIOD_NS: u64 = 33_351_500;
/// The frame period streaming at 915 lines, in ns.
const PERIOD_915_NS: u64 = 8_335_600;
/// One line of the sensor's mode 0 streaming (728 clocks at 80 MHz), in ns: the most a stamp may differ.
const LINE_NS: u64 = 9_100;
/// The frame length the streaming tests use.
const LINES: u32 = 3664;
const FRAME_BYTES: usize = RAW10_STRIDE * HEIGHT as usize;
const WAIT: Duration = Duration::from_secs(10);

/// The 10-bit codes of a packed RAW10 frame, row by row.
fn codes(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for y in 0..HEIGHT as usize {
        let row = &bytes[y * RAW10_STRIDE..(y + 1) * RAW10_STRIDE];
        for g in 0..(WIDTH / 4) as usize {
            let group = &row[g * 5..g * 5 + 5];
            for i in 0..4 {
                let low = u16::from(group[4] >> (2 * i)) & 3;
                out.push(u16::from(group[i]) << 2 | low);
            }
        }
    }
    out
}

fn mean(bytes: &[u8]) -> f64 {
    let c = codes(bytes);
    c.iter().map(|v| f64::from(*v)).sum::<f64>() / c.len() as f64
}

/// A consumer started at `lines` (the frame length is set before the sensor starts, as the chain
/// proof sets it), once the sensor streams.
fn started_at(sim: &Sim, lines: u32) -> lemnos_camera_sim::Consumer {
    let mut consumer = sim.consumer().unwrap();
    consumer.set_frame_length(lines).unwrap();
    start_when_free(&mut consumer);
    let until = Instant::now() + WAIT;
    while !sim.streaming() {
        assert!(Instant::now() < until, "the sensor did not start streaming");
        std::thread::sleep(Duration::from_millis(1));
    }
    consumer
}

fn started(sim: &Sim) -> lemnos_camera_sim::Consumer {
    started_at(sim, LINES)
}

/// Starts the receiver and the sensor. A call that meets a tick of the receiver is answered
/// `TIMED_OUT` without running (the lane is busy), so the start is asked again until it runs or the
/// wait is over: a start that answers any other way is final.
fn start_when_free(consumer: &mut lemnos_camera_sim::Consumer) {
    let until = Instant::now() + WAIT;
    loop {
        match consumer.start() {
            Ok(()) => return,
            Err(Status::TIMED_OUT) if Instant::now() < until => {}
            Err(status) => panic!("start: {status:?}"),
        }
    }
}

/// The receiver's counters once `done` is true of them. A call that meets a tick is answered
/// `TIMED_OUT` without running, so it is asked again until the condition holds or the wait is over.
fn counters_when(
    consumer: &mut lemnos_camera_sim::Consumer,
    done: impl Fn(&lemnos_contract_camera_receiver::Counters) -> bool,
) -> lemnos_contract_camera_receiver::Counters {
    let until = Instant::now() + WAIT;
    loop {
        match consumer.counters() {
            Ok(c) if done(&c) => return c,
            Ok(c) => assert!(Instant::now() < until, "counters did not settle: {c:?}"),
            Err(Status::TIMED_OUT) => assert!(Instant::now() < until, "counters busy"),
            Err(status) => panic!("counters: {status:?}"),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Asks the sensor for a frame length until it answers. After a fault the host binds the sensor
/// again under its failure rule, and until then the sensor answers `BUSY` (or `TIMED_OUT` while its
/// lane is busy): those are asked again until the answer is `Ok` or the wait is over.
fn frame_length_when_back(
    consumer: &mut lemnos_camera_sim::Consumer,
    lines: u32,
) -> Result<(), Status> {
    let until = Instant::now() + WAIT;
    loop {
        match consumer.set_frame_length(lines) {
            Ok(()) => return Ok(()),
            Err(Status::BUSY | Status::TIMED_OUT) if Instant::now() < until => {}
            Err(status) => return Err(status),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn frame(event: Event) -> lemnos_camera_sim::SimFrame {
    match event {
        Event::Frame(f) => f,
        Event::Idle => panic!("expected a frame, got idle (no frame in the wait)"),
        Event::Stalled { since_ns } => panic!("expected a frame, got a stall since {since_ns}"),
        Event::Resumed => panic!("expected a frame, got a resume"),
        Event::Ended => panic!("expected a frame, the stream ended"),
    }
}

/// The next frame, within [`WAIT`]. A stall and the resume after it are passed over: on a loaded
/// machine the producer can fall silent past the stream's stall bound and then go on, which is
/// not what these tests look at (the stall tests read the events themselves).
fn frame_within(
    mut ask: impl FnMut(Duration) -> Result<Event, String>,
) -> lemnos_camera_sim::SimFrame {
    let until = Instant::now() + WAIT;
    loop {
        let event = ask(until.saturating_duration_since(Instant::now())).unwrap();
        match event {
            Event::Stalled { .. } | Event::Resumed if Instant::now() < until => {}
            event => return frame(event),
        }
    }
}

fn next_frame(consumer: &mut lemnos_camera_sim::Consumer) -> lemnos_camera_sim::SimFrame {
    frame_within(|wait| consumer.next(wait))
}

fn recv_frame(consumer: &mut lemnos_camera_sim::Consumer) -> lemnos_camera_sim::SimFrame {
    frame_within(|wait| consumer.recv(wait))
}

fn paced(clock: lemnos_camera_sim::ClockSource) -> Sim {
    Sim::builder()
        .pace(Pace::Real)
        .clock(clock)
        .memory(Memory::Memfd)
        .start()
        .unwrap()
}

fn lockstep() -> Sim {
    Sim::builder()
        .pace(Pace::Lockstep)
        .clock(lemnos_camera_sim::ClockSource::Injected)
        .memory(Memory::Memfd)
        .start()
        .unwrap()
}

// ---------------------------------------------------------------- paced

#[test]
fn the_period_is_the_sensors_frame_length_times_its_line_time() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    // (vts - 1) x hts + global, at 96 MHz: (4395 - 1) x 728 + 1456 clocks.
    assert_eq!(sim.period_ns(), PERIOD_REST_NS);
}

#[test]
fn paced_frames_fall_on_the_sensor_period_from_its_start_and_are_stamped_on_the_grid() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started(&sim);
    let start = sim.now_ns();
    sim.advance_to(start + 5 * PERIOD_NS);
    let mut previous = 0u64;
    for k in 0..5u64 {
        let f = recv_frame(&mut consumer);
        assert_eq!(f.id, k, "frames come in order from 0");
        assert!(f.timestamp_ns > 0, "a stamp is never 0");
        assert_eq!(
            f.timestamp_ns,
            start + (k + 1) * PERIOD_NS,
            "frame {k} is stamped on the grid: one period after the sensor's start, then each period"
        );
        if k > 0 {
            let delta = f.timestamp_ns - previous;
            assert!(
                delta.abs_diff(PERIOD_NS) <= LINE_NS,
                "interval {delta} ns is the frame length within one line"
            );
        }
        previous = f.timestamp_ns;
        consumer.release(f).unwrap();
    }
}

#[test]
fn paced_frames_carry_the_bytes_of_their_identity() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started(&sim);
    sim.advance_to(sim.now_ns() + 3 * PERIOD_NS);
    for k in 0..3u64 {
        let f = recv_frame(&mut consumer);
        let bytes = consumer.bytes(&f);
        assert_eq!(bytes.len(), FRAME_BYTES);
        let want: Vec<u8> = (0..FRAME_BYTES).map(|i| scene::paced_byte(k, i)).collect();
        assert!(
            bytes == want.as_slice(),
            "frame {k}'s bytes are its identity's"
        );
        consumer.release(f).unwrap();
    }
}

#[test]
fn a_due_frame_with_no_free_buffer_is_dropped_and_counted_and_the_stream_continues() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started(&sim);
    let start = sim.now_ns();
    let period = sim.period_ns();
    // Eight frames fall due and the consumer reads all eight and holds them: every buffer is held.
    sim.advance_to(start + 8 * period);
    let mut held = Vec::new();
    for k in 0..8u64 {
        let f = recv_frame(&mut consumer);
        assert_eq!(f.id, k);
        held.push(f);
    }
    // Six more fall due with no buffer free. They are dropped and counted, never waited for.
    sim.advance_to(start + 14 * period);
    let c = counters_when(&mut consumer, |c| c.dropped >= 6);
    assert_eq!(
        (c.frames, c.dropped, c.errors),
        (8, 6, 0),
        "eight made, six dropped (no buffer)"
    );
    // The consumer gives its eight back; the stream goes on, and the next frame is the next start.
    for f in held {
        consumer.release(f).unwrap();
    }
    sim.advance_to(start + 15 * period);
    let f = recv_frame(&mut consumer);
    assert_eq!(f.id, 14, "the dropped starts still count in the sequence");
    assert_eq!(
        f.timestamp_ns,
        start + 15 * period,
        "the next frame is stamped with its own start"
    );
    consumer.release(f).unwrap();
    let c = counters_when(&mut consumer, |c| c.frames >= 9);
    assert_eq!((c.frames, c.dropped, c.errors), (9, 6, 0));
}

/// Four seconds of the stream's own clock, moved one period at a time: every frame comes, in order, on the grid,
/// none dropped, so no gap between frames is longer than one period (33 ms, under the consumer's 250 ms). On the
/// injected clock, so the host's load can't move it.
#[test]
fn a_paced_stream_over_four_seconds_of_its_clock_has_no_gap_over_one_period() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started(&sim);
    let start = sim.now_ns();
    let frames = 4_000_000_000u64.div_ceil(PERIOD_NS);
    const { assert!(PERIOD_NS < 250_000_000) };
    for k in 0..frames {
        sim.advance_to(start + (k + 1) * PERIOD_NS);
        let f = recv_frame(&mut consumer);
        assert_eq!(f.id, k, "frame {k} comes next, none dropped");
        assert_eq!(
            f.timestamp_ns,
            start + (k + 1) * PERIOD_NS,
            "frame {k} is stamped one period after the one before"
        );
        consumer.release(f).unwrap();
    }
    let c = counters_when(&mut consumer, |c| c.frames >= frames);
    assert_eq!((c.frames, c.dropped, c.errors), (frames, 0, 0));
}

/// On the host's clock, only what load can't break: frames come in order, each stamped on the sensor's grid and
/// never in the future, and a frame the host was too slow for is dropped and counted in the sequence, not stamped
/// late. How many frames a loaded host keeps up with is a measurement, not this test's.
#[test]
fn a_real_paced_stream_stamps_on_its_grid_and_counts_what_it_drops_in_the_sequence() {
    let sim = paced(lemnos_camera_sim::ClockSource::Monotonic);
    let mut consumer = started(&sim);
    let period = sim.period_ns();
    let mut previous: Option<(u64, u64)> = None;
    for _ in 0..5 {
        let f = recv_frame(&mut consumer);
        assert!(f.timestamp_ns > 0);
        let now = lemnos_linux_sys::time::monotonic_ns().unwrap();
        assert!(
            f.timestamp_ns <= now,
            "a stamp is never in the future: {} > {now} (CLOCK_MONOTONIC)",
            f.timestamp_ns
        );
        if let Some((id, stamp)) = previous {
            assert!(f.id > id, "frames come in order: {} after {id}", f.id);
            let starts = f.id - id;
            let delta = f.timestamp_ns - stamp;
            assert!(
                delta.abs_diff(starts * period) <= LINE_NS,
                "{starts} frame start(s) apart is {delta} ns: the period {period} ns each, within one line"
            );
        }
        previous = Some((f.id, f.timestamp_ns));
        consumer.release(f).unwrap();
    }
}

// ---------------------------------------------------------------- lock-step

#[test]
fn next_advances_exactly_one_frame_in_order() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    for k in 0..3u64 {
        let f = next_frame(&mut consumer);
        assert_eq!(f.id, k);
        consumer.release(f).unwrap();
    }
    let c = counters_when(&mut consumer, |c| c.frames >= 3);
    assert_eq!(c.frames, 3, "three grants, three frames");
}

#[test]
fn a_control_posted_for_frame_k_lands_on_exactly_frame_k() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    sim.post(3, Control::Exposure(5_000)).unwrap();
    for k in 0..5u64 {
        let f = next_frame(&mut consumer);
        assert_eq!(f.id, k);
        if k < 3 {
            assert_eq!(
                f.meta.exposure_us,
                scene::REFERENCE_EXPOSURE_US,
                "frame {k} is before it"
            );
            assert_eq!(f.meta.applied, [None, None]);
        } else {
            assert_eq!(
                f.meta.exposure_us, 5_000,
                "frame {k} holds it from frame 3 on"
            );
            if k == 3 {
                assert_eq!(f.meta.applied[0], Some(Control::Exposure(5_000)));
            } else {
                assert_eq!(f.meta.applied, [None, None]);
            }
        }
        consumer.release(f).unwrap();
    }
}

#[test]
fn a_late_control_is_refused_late_and_a_bad_value_is_refused_by_value() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    for _ in 0..2 {
        let f = next_frame(&mut consumer);
        consumer.release(f).unwrap();
    }
    // Frames 0 and 1 are granted: a control for either lands late.
    assert_eq!(sim.post(1, Control::Exposure(5_000)), Err(Refusal::Late));
    assert_eq!(sim.post(0, Control::Gain(2_000)), Err(Refusal::Late));
    // A value out of range is refused by value, for a frame not yet granted.
    assert_eq!(sim.post(2, Control::Exposure(0)), Err(Refusal::Value));
    assert_eq!(
        sim.post(2, Control::Exposure(MAX_EXPOSURE_US + 1)),
        Err(Refusal::Value)
    );
    assert_eq!(sim.post(2, Control::Gain(999)), Err(Refusal::Value));
    assert_eq!(
        sim.post(2, Control::Gain(MAX_GAIN_MILLI + 1)),
        Err(Refusal::Value)
    );
    // A good value for frame 2 is accepted and lands there.
    assert_eq!(sim.post(2, Control::Exposure(5_000)), Ok(()));
    let f = next_frame(&mut consumer);
    assert_eq!(f.id, 2);
    assert_eq!(f.meta.exposure_us, 5_000);
    consumer.release(f).unwrap();
}

#[test]
fn exposure_and_gain_scale_the_light_and_the_black_level_is_64() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    let half = scene::REFERENCE_EXPOSURE_US / 2;
    // Frame 0 is bright: half the exposure at twice the gain is the reference light (1.0).
    sim.post(0, Control::Exposure(half)).unwrap();
    sim.post(0, Control::Gain(2 * scene::REFERENCE_GAIN_MILLI))
        .unwrap();
    let f0 = next_frame(&mut consumer);
    let bright = mean(consumer.bytes(&f0));
    assert_eq!(f0.meta.analogue_gain_milli, 2 * scene::REFERENCE_GAIN_MILLI);
    consumer.release(f0).unwrap();
    // Frame 1 is dim by gain: the gain back to 1x, the exposure as it was (half the light).
    sim.post(1, Control::Gain(scene::REFERENCE_GAIN_MILLI))
        .unwrap();
    let f1 = next_frame(&mut consumer);
    let dim_gain = mean(consumer.bytes(&f1));
    assert_eq!(f1.meta.exposure_us, half, "the exposure holds from frame 0");
    consumer.release(f1).unwrap();
    // Frame 2 is dim by exposure: a quarter of the exposure at twice the gain (half the light).
    sim.post(2, Control::Exposure(scene::REFERENCE_EXPOSURE_US / 4))
        .unwrap();
    sim.post(2, Control::Gain(2 * scene::REFERENCE_GAIN_MILLI))
        .unwrap();
    let f2 = next_frame(&mut consumer);
    let dim_exposure = mean(consumer.bytes(&f2));
    consumer.release(f2).unwrap();

    let want_bright = 64.0 + scene::SIGNAL;
    let want_dim = 64.0 + scene::SIGNAL / 2.0;
    assert!(
        (bright - want_bright).abs() <= 1.0,
        "bright mean {bright}, want {want_bright}"
    );
    assert!(
        (dim_gain - want_dim).abs() <= 1.0,
        "dim (gain) mean {dim_gain}, want {want_dim}"
    );
    assert!(
        (dim_exposure - want_dim).abs() <= 1.0,
        "dim (exposure) mean {dim_exposure}, want {want_dim}"
    );
    // Dim is half of bright on the signal (above the black level), within 2.0 of the codes.
    assert!(((bright - 64.0) - 2.0 * (dim_gain - 64.0)).abs() <= 2.0);
    assert!(((bright - 64.0) - 2.0 * (dim_exposure - 64.0)).abs() <= 2.0);

    // The least exposure is still black level 64 (within a code).
    sim.post(3, Control::Exposure(1)).unwrap();
    let f3 = next_frame(&mut consumer);
    let black = mean(consumer.bytes(&f3));
    assert!((black - 64.0).abs() <= 1.0, "black level {black}");
    consumer.release(f3).unwrap();
}

#[test]
fn the_pattern_is_a_static_ramp_with_bands_every_eight_rows() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    let f = next_frame(&mut consumer);
    let c = codes(consumer.bytes(&f));
    let at = |x: usize, y: usize| c[y * WIDTH as usize + x];
    // The ramp: the columns get brighter across the row.
    assert!(at(1279, 0) > at(0, 0), "a ramp across the row");
    // The bands: rows 0..8 are one band, and row 8 starts the next.
    for y in 1..8 {
        assert_eq!(at(640, y), at(640, 0), "row {y} is in the first band");
    }
    assert_ne!(at(640, 8), at(640, 0), "row 8 starts the next band");
    consumer.release(f).unwrap();
}

#[test]
fn replay_plays_its_frames_in_turn_and_loops() {
    let frames: Vec<Vec<u8>> = (1u8..=3).map(|v| vec![v; FRAME_BYTES]).collect();
    let sim = Sim::builder()
        .pace(Pace::Lockstep)
        .clock(lemnos_camera_sim::ClockSource::Injected)
        .scene(Scene::Replay(frames.clone()))
        .start()
        .unwrap();
    let mut consumer = started(&sim);
    for n in 0..4usize {
        let f = next_frame(&mut consumer);
        assert_eq!(f.id, n as u64);
        assert!(
            consumer.bytes(&f) == frames[n % 3].as_slice(),
            "frame {n} is frames[{n} % 3]"
        );
        consumer.release(f).unwrap();
    }
}

#[test]
fn memfd_frames_equal_the_reference_row_for_row_and_dma_heap_is_used_or_refused() {
    let reference = {
        let mut out = vec![0u8; FRAME_BYTES];
        scene::render_pattern(
            &mut out,
            scene::REFERENCE_EXPOSURE_US,
            scene::REFERENCE_GAIN_MILLI,
        );
        out
    };
    let sim = lockstep();
    let mut consumer = started(&sim);
    let f = next_frame(&mut consumer);
    let bytes = consumer.bytes(&f);
    for y in 0..HEIGHT as usize {
        let row = y * RAW10_STRIDE..(y + 1) * RAW10_STRIDE;
        assert!(
            bytes[row.clone()] == reference[row],
            "row {y} of the memfd frame"
        );
    }
    consumer.release(f).unwrap();
    drop(consumer);
    drop(sim);

    // The dma-heap instance either runs and matches the same reference, or the host has no heap
    // and the builder says so. It is never silently another kind of buffer.
    match (
        SimBuffers::dma_heap().is_some(),
        Sim::builder()
            .pace(Pace::Lockstep)
            .clock(lemnos_camera_sim::ClockSource::Injected)
            .memory(Memory::DmaHeap)
            .start(),
    ) {
        (true, Ok(sim)) => {
            let mut consumer = started(&sim);
            let f = next_frame(&mut consumer);
            assert!(
                consumer.bytes(&f) == reference.as_slice(),
                "the dma-buf frame is the reference"
            );
            consumer.release(f).unwrap();
        }
        (false, Err(why)) => assert!(why.contains("dma-heap"), "{why}"),
        (heap, other) => panic!(
            "the host has a heap: {heap}, but the builder said {:?}",
            other.err()
        ),
    }
}

// ---------------------------------------------------------------- instances

#[test]
fn two_instances_in_one_process_share_no_state() {
    let a = lockstep();
    let b = lockstep();
    assert_ne!(a.socket(), b.socket(), "each instance has its own socket");
    let mut ca = started(&a);
    let mut cb = started(&b);
    // A's receiver is silent and its clock moves; B's frames and clock do not notice.
    a.receiver_silent(true);
    assert!(matches!(
        ca.next(Duration::from_millis(200)).unwrap(),
        Event::Idle
    ));
    let fb = next_frame(&mut cb);
    assert_eq!(fb.id, 0, "B makes its first frame");
    assert_eq!(b.first_unmade(), None, "B is not silent");
    assert_eq!(
        b.now_ns(),
        fb.timestamp_ns,
        "B's clock is its own: it stands at B's last start"
    );
    cb.release(fb).unwrap();
    assert_eq!(
        a.first_unmade(),
        Some(0),
        "A recorded its own first unmade frame"
    );
    // A fault on A's sensor is not on B.
    a.sensor_fault(SensorFault::Failed("only A"));
    let fb = next_frame(&mut cb);
    assert_eq!(fb.id, 1);
    cb.release(fb).unwrap();
    a.clear_sensor_fault();
    a.receiver_silent(false);
}

#[test]
fn a_refused_receiver_start_is_refused_with_io_the_number_of_times_asked() {
    let sim = lockstep();
    let mut consumer = sim.consumer().unwrap();
    sim.refuse_receiver_starts(2);
    assert_eq!(consumer.start(), Err(Status::IO));
    assert_eq!(consumer.start(), Err(Status::IO));
    assert_eq!(consumer.start(), Ok(()), "the third start is made");
}

#[test]
fn a_silent_receiver_makes_no_frames_and_no_fault_and_records_the_first_unmade_frame() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    sim.receiver_silent(true);
    for _ in 0..2 {
        match consumer.next(Duration::from_millis(200)).unwrap() {
            Event::Idle => {}
            _ => panic!("a silent receiver is idle, not stalled and not framed"),
        }
    }
    assert_eq!(
        sim.first_unmade(),
        Some(0),
        "the first frame not made is frame 0"
    );
    sim.receiver_silent(false);
    let f = next_frame(&mut consumer);
    assert_eq!(f.id, 2, "the two silent grants took ids 0 and 1");
    consumer.release(f).unwrap();
}

#[test]
fn a_sensor_failure_keeps_the_stream_attached_and_is_cleared() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    let f = next_frame(&mut consumer);
    consumer.release(f).unwrap();
    sim.sensor_fault(SensorFault::Failed("test"));
    // The sensor's transactions fail while the fault stands...
    assert!(
        consumer.set_frame_length(LINES).is_err(),
        "a failed sensor refuses a transaction"
    );
    // ...the stream is reported stalled (the producer is being retried), and it is not ended.
    let until = Instant::now() + WAIT;
    loop {
        match consumer.recv(Duration::from_millis(50)).unwrap() {
            Event::Stalled { .. } => break,
            Event::Ended => panic!("a failed sensor ended the stream"),
            Event::Frame(_) => panic!("a frame came while the sensor failed"),
            _ => assert!(
                Instant::now() < until,
                "the stream was never reported stalled"
            ),
        }
    }
    // Cleared, the sensor answers again, the stream resumes, and the next grant makes frame 1.
    sim.clear_sensor_fault();
    frame_length_when_back(&mut consumer, LINES).expect("a sensor whose fault is cleared answers");
    let until = Instant::now() + WAIT;
    let mut resumed = false;
    loop {
        match consumer.next(Duration::from_millis(50)).unwrap() {
            Event::Resumed => resumed = true,
            Event::Frame(f) => {
                assert_eq!(f.id, 1);
                consumer.release(f).unwrap();
                break;
            }
            Event::Ended => panic!("the stream ended after the sensor was cleared"),
            _ => assert!(
                Instant::now() < until,
                "no frame after the sensor was cleared"
            ),
        }
    }
    assert!(
        resumed,
        "the stream is reported resumed before its next frame"
    );
}

#[test]
fn a_consumer_fault_ends_the_consumer_on_the_next_tick() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started(&sim);
    sim.fault_consumer();
    let until = Instant::now() + WAIT;
    loop {
        match consumer.recv(Duration::from_millis(100)).unwrap() {
            Event::Ended => break,
            Event::Idle => assert!(Instant::now() < until, "the consumer was not ended"),
            _ => {}
        }
    }
}

#[test]
fn a_gone_receiver_reports_gone_for_good() {
    let sim = lockstep();
    let consumer = started(&sim);
    sim.receiver_gone("test: the receiver is gone");
    // The host asks the receiver for its health: it answers `Gone`, and the host then binds it
    // again under the failure rule, so the device is never seen healthy again.
    let until = Instant::now() + WAIT;
    while sim.gone_reports() == 0 {
        assert!(
            Instant::now() < until,
            "the host never asked the receiver for its health"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut seen_not_ok = false;
    while Instant::now() < until && !seen_not_ok {
        let list = match consumer.client().list() {
            Ok(list) => list,
            // No answer in time under load: ask again until the bound.
            Err(lemnos_client::Error::TimedOut) => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(e) => panic!("list: {e:?}"),
        };
        let receiver = list.iter().find(|d| d.name == "cam0-receiver").unwrap();
        seen_not_ok = !format!("{:?}", receiver.health).contains("Ok");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(seen_not_ok, "the receiver device never left Ok");
}

#[test]
fn an_instance_refuses_a_grant_when_it_is_not_lock_step() {
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    assert!(sim.grant().is_err());
}

#[test]
fn the_frame_length_a_test_sets_moves_the_period_and_the_stamps_follow_it() {
    // 915 lines (the counters test's frame length): the period is (915 - 1) x 728 + 1456 clocks at
    // 80 MHz, and each stamp interval is it within one line.
    let sim = paced(lemnos_camera_sim::ClockSource::Injected);
    let mut consumer = started_at(&sim, 915);
    assert_eq!(sim.period_ns(), PERIOD_915_NS);
    let start = sim.now_ns();
    sim.advance_to(start + 3 * PERIOD_915_NS);
    let mut previous = 0u64;
    for k in 0..3u64 {
        let f = recv_frame(&mut consumer);
        if k > 0 {
            let delta = f.timestamp_ns - previous;
            assert!(delta.abs_diff(PERIOD_915_NS) <= LINE_NS, "interval {delta}");
        }
        previous = f.timestamp_ns;
        consumer.release(f).unwrap();
    }
}

#[test]
fn a_gone_sensor_is_spent_once_the_host_has_seen_it() {
    let sim = lockstep();
    let mut consumer = started(&sim);
    // The fault is spent by the next transaction to the sensor: that one finds no device, and the
    // one after it works (the host has bound the sensor again).
    sim.sensor_fault(SensorFault::Gone("test"));
    assert!(
        consumer.set_frame_length(LINES).is_err(),
        "the transaction that meets the gone sensor fails"
    );
    frame_length_when_back(&mut consumer, LINES).expect("the sensor is bound again and answers");
    // Frames still come once the sensor is back.
    let f = next_frame(&mut consumer);
    consumer.release(f).unwrap();
}

#[test]
fn a_gone_receiver_with_auto_off_ends_its_stream_device_failed() {
    let sim = Sim::builder()
        .pace(Pace::Lockstep)
        .clock(lemnos_camera_sim::ClockSource::Injected)
        .auto_off_after(2)
        .start()
        .unwrap();
    let mut consumer = started(&sim);
    let f = next_frame(&mut consumer);
    consumer.release(f).unwrap();
    sim.receiver_gone("test: gone for good");
    let until = Instant::now() + WAIT;
    loop {
        match consumer.recv(Duration::from_millis(100)).unwrap() {
            Event::Ended => break,
            _ => assert!(Instant::now() < until, "the stream did not end"),
        }
    }
    let list = consumer.client().list().unwrap();
    let receiver = list.iter().find(|d| d.name == "cam0-receiver").unwrap();
    assert!(
        format!("{:?}", receiver.state)
            .to_lowercase()
            .contains("off"),
        "the receiver is turned off: {:?}",
        receiver.state
    );
}
