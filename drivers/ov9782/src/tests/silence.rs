//! The watch on the receiver's frame-start count: a sensor that stops sending is restarted.
//! Time is the tick's own clock; the count is a fake receiver's, moved by the test.

use super::*;
use core::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicBool, AtomicU64};

const W: u64 = RECEIVER_WATCH_NS;
const MS: u64 = 1_000_000;
/// The mode with the 60 frames-a-second timing: 1280x720 RAW10, 1742 lines of 9.5625 us.
const MODE_720: u16 = 2;

const COUNTING: u8 = 0;
const UNKNOWN: u8 = 1;
const UNSUPPORTED: u8 = 2;
const READ_FAILS: u8 = 3;

struct Rx {
    count: AtomicU64,
    answer: AtomicU8,
    failing: AtomicBool,
}

impl lemnos_core::Router for Rx {
    fn call(
        &self,
        _device: u32,
        method: lemnos_core::MethodId,
        _args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if method.method == FRAME_STARTS_METHOD {
            let answer: Option<FrameStarts> = match self.answer.load(Ordering::SeqCst) {
                COUNTING => {
                    let count = self.count.load(Ordering::SeqCst);
                    Some(FrameStarts {
                        count,
                        latest_ns: count,
                    })
                }
                UNKNOWN => None,
                UNSUPPORTED => return Err(Status::NOT_SUPPORTED),
                _ => return Err(Status::IO),
            };
            let mut writer = lemnos_core::Writer::new(reply);
            lemnos_core::Encode::encode(&answer, &mut writer)?;
            return Ok(writer.len());
        }
        let name = b"ov9782 10-0036";
        reply[..name.len()].copy_from_slice(name);
        Ok(name.len())
    }

    fn health(&self, _device: u32) -> Health {
        if self.failing.load(Ordering::SeqCst) {
            Health::Failed("the receiver is failing")
        } else {
            Health::Ok
        }
    }
}

struct Rig {
    s: Ov9782,
    bus: Bus,
    rx: &'static Rx,
    /// The time of the last watch.
    now: u64,
}

impl Rig {
    fn new(mode: u16) -> Self {
        let rx: &'static Rx = Box::leak(Box::new(Rx {
            count: AtomicU64::new(0),
            answer: AtomicU8::new(COUNTING),
            failing: AtomicBool::new(false),
        }));
        let receiver: Dev<dyn CameraReceiver> =
            lemnos_core::Bound::new(lemnos_core::LocalCaller::new(rx, 0, 0));
        let bus = bus_with_id(0x9782);
        let mut s = <Ov9782 as Lifecycle>::start(
            Ov9782Needs {
                bus: I2c(lemnos_core::Held::boxed(Box::new(bus.clone()))),
                values: None,
                _receiver: Some(receiver),
            },
            &Config::new(&[]),
        )
        .unwrap();
        s.select_mode(mode).unwrap();
        start_up(&mut s);
        Self {
            s,
            bus,
            rx,
            now: POWER_UP_NS,
        }
    }

    /// One watch interval passes, `frames` frame starts are counted in it, and the sensor ticks.
    fn watch(&mut self, frames: u64) -> Option<u64> {
        self.now += W;
        self.rx.count.fetch_add(frames, Ordering::SeqCst);
        Lifecycle::tick(&mut self.s, self.now)
    }

    /// `n` watches with a frame each: the stream is seen working.
    fn working(&mut self, n: u32) {
        for _ in 0..n {
            self.watch(1);
            assert_eq!(self.s.state, State::Streaming);
        }
    }

    fn recovering(&self) -> bool {
        matches!(self.s.state, State::Recovering { .. })
    }

    /// Watches with no frame until the sensor is judged silent; the time it took from the last
    /// watch that saw a frame, or `None` when `limit_ns` passed without a judgement.
    fn until_silent(&mut self, limit_ns: u64) -> Option<u64> {
        let from = self.now;
        while self.now - from < limit_ns {
            self.watch(0);
            if self.recovering() {
                return Some(self.now - from);
            }
        }
        None
    }

    /// Runs the recovery's first attempt: `due` ticks later the sensor is programmed again. The
    /// writes that attempt made.
    fn recover(&mut self) -> Vec<(u16, u8)> {
        let due = Lifecycle::tick(&mut self.s, self.now).unwrap();
        clear(&self.bus);
        self.now = due;
        Lifecycle::tick(&mut self.s, self.now);
        writes(&self.bus)
    }
}

/// The period of the 720 mode at `lines`, the one the driver's bound uses: `mode.period_ps(lines)`.
fn period_720(lines: u32) -> u128 {
    MODES[usize::from(MODE_720)]
        .describe(false)
        .period_ps(lines)
}

/// The bound the driver has at `lines`.
fn bound_720(lines: u32) -> u64 {
    silence_bound_ns(period_720(lines))
}

#[test]
fn the_bound_is_four_frame_periods_with_a_floor_of_two_watches_and_a_margin() {
    // 1280x720 RAW10: 765 / 80 MHz = 9.5625 us a line, and the frame's offset from F2h3.
    let mode = MODES[usize::from(MODE_720)].describe(false);
    assert_eq!(mode.line_time_ps, 9_562_500);
    assert_eq!(SILENCE_MIN_NS, 250 * MS);
    // The bound is made of the contract's period, not of its own product.
    assert_eq!(
        period_720(1742),
        1742 * 9_562_500 + u128::from(mode.frame_offset_ps)
    );
    // The default 1742 lines: about 16.66 ms, four of them 66.6 ms: the floor holds.
    assert_eq!(bound_720(1742), 250 * MS);
    // The frame length at which four periods reach the floor.
    let first = (1..).find(|l| bound_720(*l) > SILENCE_MIN_NS).unwrap();
    assert_eq!(bound_720(first - 1), 250 * MS);
    assert_eq!(bound_720(first), (4 * period_720(first) / 1000) as u64);
    // The longest frame the mode allows: 720 + 51540 lines, so the bound scales to 2 s.
    assert_eq!(
        bound_720(52_260),
        (4 * (52_260u128 * 9_562_500 + u128::from(mode.frame_offset_ps)) / 1000) as u64
    );
    // 1998.979 ms: one offset (8.6375 us a period, 34.55 us the four) over the old lines-only 1998.945 ms.
    assert_eq!(bound_720(52_260), 1_998_979_550);
}

#[test]
fn frames_that_keep_coming_are_never_judged() {
    let mut r = Rig::new(MODE_720);
    r.working(200);
    assert!(!matches!(Driver::health(&r.s), Health::Failed(_)));
}

#[test]
fn a_count_frozen_past_the_bound_restarts_the_sensor_with_its_reason() {
    let mut r = Rig::new(MODE_720);
    r.working(5);
    // Frozen for 250 ms is the bound itself, not past it: nothing yet.
    r.watch(0);
    r.watch(0);
    r.watch(0);
    // 3 watches are 300 ms: past the bound of 250 ms.
    assert!(r.recovering(), "{:?}", r.s.state);
    assert_eq!(
        Driver::health(&r.s),
        Health::Failed(
            "no frame start for over 250 ms at the frame length in force: restarting the sensor"
        )
    );
    // The first attempt is the bottom of the backoff; the second tick is due then.
    let due = Lifecycle::tick(&mut r.s, r.now).unwrap();
    assert_eq!(due, r.now + 250 * MS);
    // The frames start again once the sensor is programmed.
    let w = r.recover();
    assert_eq!(w.first(), Some(&(0x0100, 0x00)));
    assert_eq!(w.last(), Some(&(0x0100, 0x01)));
    assert_eq!(r.s.state, State::Streaming);
    assert!(!matches!(Driver::health(&r.s), Health::Failed(_)));
    r.working(5);
    assert_eq!(r.s.watch.streak, 0);
}

#[test]
fn frozen_just_under_the_bound_is_not_judged() {
    let mut r = Rig::new(MODE_720);
    r.working(5);
    r.watch(0);
    r.watch(0);
    // 200 ms of silence: under 250 ms. A frame ends it.
    assert_eq!(r.s.state, State::Streaming);
    r.watch(1);
    r.watch(0);
    r.watch(0);
    assert_eq!(r.s.state, State::Streaming);
}

#[test]
fn the_time_to_the_judgement_is_between_the_bound_and_the_bound_plus_two_watches() {
    // The count freezes right after a watch: the last frame start was just after the watch before
    // the one that saw it. The sensor is judged at the first watch with the count past the bound,
    // and health is read in the same tick, so the Failed edge goes out then.
    for (frame_length, bound) in [
        (None, 250 * MS),
        (Some(52_260), bound_720(52_260)),
        (Some(1_000), 250 * MS),
    ] {
        let mut r = Rig::new(MODE_720);
        if let Some(lines) = frame_length {
            r.s.set_controls(Controls {
                frame_length: Some(lines),
                ..Controls::default()
            })
            .unwrap();
        }
        r.working(10);
        // The frame starts 1 ns after this watch and is seen by the next.
        let last_frame = r.now + 1;
        r.watch(1);
        let judged = r.until_silent(10_000 * MS).expect("judged");
        let after_last_frame = r.now - last_frame;
        assert!(after_last_frame > bound, "{after_last_frame} vs {bound}");
        assert!(
            after_last_frame <= bound + 2 * W,
            "{after_last_frame} vs {bound}"
        );
        // And never before the bound counted from the watch that saw the frame.
        assert!(judged > bound, "{judged} vs {bound}");
    }
}

#[test]
fn the_worked_numbers_for_the_default_and_the_longest_frame() {
    // 60 frames a second, 1742 lines: judged 300 ms after the watch that saw the last frame, so
    // 400 ms (less 1 ns) after the frame start in the worst case.
    let mut r = Rig::new(MODE_720);
    r.working(10);
    let last_frame = r.now + 1;
    r.watch(1);
    r.until_silent(10_000 * MS).unwrap();
    assert_eq!(r.now - last_frame, 400 * MS - 1);
    // 52260 lines: 2000 ms after the watch, 2100 ms (less 1 ns) after the frame start.
    let mut r = Rig::new(MODE_720);
    r.s.set_controls(Controls {
        frame_length: Some(52_260),
        ..Controls::default()
    })
    .unwrap();
    r.working(10);
    let last_frame = r.now + 1;
    r.watch(1);
    r.until_silent(10_000 * MS).unwrap();
    assert_eq!(r.now - last_frame, 2100 * MS - 1);
}

#[test]
fn an_unknown_or_unsupported_count_is_never_judged() {
    for answer in [UNKNOWN, UNSUPPORTED, READ_FAILS] {
        let mut r = Rig::new(MODE_720);
        r.working(5);
        r.rx.answer.store(answer, Ordering::SeqCst);
        assert_eq!(r.until_silent(60_000 * MS), None, "answer {answer}");
        // The count is known again, still the same: the watch begins again, with a full bound.
        r.rx.answer.store(COUNTING, Ordering::SeqCst);
        r.watch(0);
        r.watch(0);
        assert_eq!(r.s.state, State::Streaming);
    }
}

#[test]
fn a_stream_that_has_shown_no_frame_since_start_is_not_judged() {
    // No frame has been counted since the start: a receiver with nothing to write into counts none.
    let mut r = Rig::new(MODE_720);
    assert_eq!(r.until_silent(120_000 * MS), None);
    // Likewise a count that stands at a value from the first read and never moves.
    let mut r = Rig::new(MODE_720);
    r.rx.count.store(40, Ordering::SeqCst);
    assert_eq!(r.until_silent(120_000 * MS), None);
}

#[test]
fn a_restart_without_frames_is_judged_again_at_the_next_backoff_step() {
    let mut r = Rig::new(MODE_720);
    r.working(5);
    r.until_silent(10_000 * MS).unwrap();
    // The first attempt succeeds, and no frame follows. Inside the bound and the first-frame
    // allowance nothing happens.
    r.recover();
    assert_eq!(r.s.state, State::Streaming);
    let bound = 250 * MS;
    let from = r.now;
    while r.now - from < bound + FIRST_FRAME_NS - W {
        r.watch(0);
        assert_eq!(r.s.state, State::Streaming, "at {} ms", (r.now - from) / MS);
    }
    // Past it: a failure again, and the backoff steps to the second delay.
    while !r.recovering() {
        r.watch(0);
        assert!(r.now - from < 60_000 * MS);
    }
    let due = Lifecycle::tick(&mut r.s, r.now).unwrap();
    assert_eq!(due - r.now, 500 * MS);
    assert!(
        matches!(Driver::health(&r.s), Health::Failed(reason) if reason.starts_with("no frame start for over"))
    );
    // Never a rebind: the driver never asks for one however long this goes on.
    for _ in 0..40 {
        r.recover();
        let mut guard = 0;
        while !r.recovering() {
            r.watch(0);
            guard += 1;
            assert!(guard < 1000);
        }
        let due = Lifecycle::tick(&mut r.s, r.now).unwrap();
        assert!(due - r.now <= 8_000 * MS);
        assert!(!matches!(Driver::health(&r.s), Health::Gone(_)));
    }
    // And frames, when they come, clear the streak.
    r.recover();
    r.working(3);
    assert_eq!(r.s.watch.streak, 0);
}

#[test]
fn a_failing_receiver_is_not_the_sensors_fault() {
    let mut r = Rig::new(MODE_720);
    r.working(5);
    r.rx.failing.store(true, Ordering::SeqCst);
    assert_eq!(r.until_silent(60_000 * MS), None);
    // It is back and the count still stands: the watch begins again as after a restart, with the
    // bound and the first-frame allowance before a verdict.
    r.rx.failing.store(false, Ordering::SeqCst);
    let from = r.now;
    while r.now - from < 250 * MS + FIRST_FRAME_NS - W {
        r.watch(0);
        assert_eq!(r.s.state, State::Streaming);
    }
    assert!(r.until_silent(2 * W).is_some());
}

#[test]
fn a_mode_that_sends_frames_only_on_demand_is_never_judged() {
    assert!(MODES.iter().all(|mode| mode.free_running));
    test_hooks::ON_DEMAND.with(|flag| flag.set(true));
    let mut r = Rig::new(MODE_720);
    r.working(5);
    assert_eq!(r.until_silent(120_000 * MS), None);
    test_hooks::ON_DEMAND.with(|flag| flag.set(false));
}

#[test]
fn a_stopped_sensor_is_not_judged_and_a_new_start_begins_the_watch_again() {
    let mut r = Rig::new(MODE_720);
    r.working(5);
    r.s.stop().unwrap();
    assert_eq!(Lifecycle::tick(&mut r.s, r.now + 10_000 * MS), None);
    assert_eq!(r.s.state, State::Stopped);
    assert_eq!(r.s.watch, Watch::default());
}

#[test]
fn a_shorter_frame_is_given_the_longer_frame_until_it_has_settled() {
    // A long frame, then a short one: the frames that were launched at the long length still come
    // every 500 ms for two more frames. The bound of the short frame is 250 ms: it must not fire.
    let mut r = Rig::new(MODE_720);
    r.s.set_controls(Controls {
        frame_length: Some(52_260),
        ..Controls::default()
    })
    .unwrap();
    for _ in 0..4 {
        r.watch(0);
        r.watch(0);
        r.watch(0);
        r.watch(0);
        r.watch(1);
        assert_eq!(r.s.state, State::Streaming);
    }
    r.s.set_controls(Controls {
        frame_length: Some(1742),
        ..Controls::default()
    })
    .unwrap();
    for _ in 0..2 {
        for _ in 0..4 {
            r.watch(0);
            assert_eq!(r.s.state, State::Streaming);
        }
        r.watch(1);
    }
    // After three frames the long length is let go.
    r.working(5);
    assert_eq!(r.s.watch.held_frame, 0);
    assert!(r.until_silent(1_000 * MS).is_some());
}

#[test]
fn what_the_instance_holds_is_in_force_again_after_the_restart() {
    let mut r = Rig::new(MODE_720);
    r.s.set_flips(true, false).unwrap();
    r.s.set_test_pattern(3).unwrap();
    r.working(3);
    // Values posted after the start: a group under hold, and the setter path too.
    r.s.post_values(FrameValues {
        identity: 9,
        controls: Controls {
            exposure: Some(700),
            analogue_gain: Some(0x28),
            digital_gain: None,
            frame_length: Some(1900),
        },
    })
    .unwrap();
    r.s.set_controls(Controls {
        exposure: Some(650),
        ..Controls::default()
    })
    .unwrap();
    let before = (
        r.s.selected().unwrap(),
        r.s.in_force().unwrap(),
        r.s.applied().unwrap(),
    );
    let seen = writes(&r.bus);
    r.working(3);
    r.until_silent(10_000 * MS).unwrap();
    let after_restart = r.recover();
    assert_eq!(r.s.state, State::Streaming);
    let now = (
        r.s.selected().unwrap(),
        r.s.in_force().unwrap(),
        r.s.applied().unwrap(),
    );
    assert_eq!(before, now);
    assert_eq!(before.0, Some(MODE_720));
    assert_eq!(before.1.frame_length, Some(1900));
    assert_eq!(before.1.analogue_gain, Some(0x28));
    // The exposure is in sixteenths of a line: 650 is 40 lines.
    assert_eq!(before.1.exposure_lines, Some(650 >> 4));
    // The registers: the last value each was given before the failure is the one the restart wrote.
    let last = |writes: &[(u16, u8)], register: u16| {
        writes
            .iter()
            .rev()
            .find(|(r, _)| *r == register)
            .map(|(_, v)| *v)
    };
    for register in [
        chip::FRAME_LENGTH,
        chip::FRAME_LENGTH + 1,
        chip::EXPOSURE,
        chip::EXPOSURE + 1,
        chip::EXPOSURE + 2,
        chip::ANALOGUE_GAIN,
        chip::FORMAT1,
        chip::FORMAT2,
        chip::TEST_PATTERN,
    ] {
        assert!(
            last(&seen, register).is_some(),
            "{register:#x} written before"
        );
        assert_eq!(
            last(&seen, register),
            last(&after_restart, register),
            "register {register:#x}"
        );
    }
    // The frame length is 1900 = 0x076c, written by hand here.
    assert_eq!(last(&after_restart, chip::FRAME_LENGTH), Some(0x07));
    assert_eq!(last(&after_restart, chip::FRAME_LENGTH + 1), Some(0x6c));
    // Standby first, streaming last.
    assert_eq!(after_restart.first(), Some(&(0x0100, 0x00)));
    assert_eq!(after_restart.last(), Some(&(0x0100, 0x01)));
}

#[test]
fn a_receiver_that_cannot_say_for_a_long_time_and_then_counts_again_does_not_restart_the_sensor() {
    // A consumer holds every buffer: the receiver has nothing armed and answers "cannot say" (None)
    // for far longer than any bound, with the count standing at its old value behind it.
    let mut r = Rig::new(MODE_720);
    r.working(5);
    r.rx.answer.store(UNKNOWN, Ordering::SeqCst);
    assert_eq!(r.until_silent(120_000 * MS), None);
    // The consumer lets go: the receiver arms again and counts from where it stood, then moves.
    r.rx.answer.store(COUNTING, Ordering::SeqCst);
    for _ in 0..3 {
        r.watch(0);
        assert_eq!(r.s.state, State::Streaming);
    }
    r.working(20);
    assert_eq!(r.s.watch.streak, 0);
    assert!(!matches!(Driver::health(&r.s), Health::Failed(_)));
}
