//! The camera proof, against simulated drivers on an in-process host with simulated time.

mod sim;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use lemnos_camera_proof::{Bus, DirSink, Error, FrameSink, Frames, Options, assemble, capture};
use lemnos_contract_camera_receiver::{CameraReceiver, MAX_EMBEDDED_STREAM_NAME};
use lemnos_contract_camera_sensor::{Controls, FrameValues, frame_length_for_fps_milli};
use lemnos_wire::ring::FrameSlot;

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("camera-proof-{}-{name}", std::process::id()))
}

#[test]
fn finds_the_sensor_by_tag_and_follows_its_links() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let camera = assemble(&bus, &Options::default()).unwrap();
    assert_eq!(camera.sensor().id, "cam0-sensor");
    assert_eq!(
        camera.receiver().id,
        "csi0",
        "the sensor's link, not the first receiver"
    );
    assert_eq!(camera.isp().map(|d| d.id.as_str()), Some("isp0"));
    assert_eq!(camera.stream(), "frames", "the name the receiver reports");
    assert!(sim::calls().is_empty(), "assembling starts nothing");
}

#[test]
fn a_sensor_with_no_link_is_refused() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(None));
    let err = assemble(&bus, &Options::default()).err().unwrap();
    assert_eq!(err, Error::NotLinked("cam0-sensor".into()));
}

#[test]
fn a_link_to_a_device_that_is_not_a_receiver_is_refused() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("isp0")));
    let err = assemble(&bus, &Options::default()).err().unwrap();
    assert!(
        matches!(&err, Error::WrongContract { device, .. } if device == "isp0"),
        "{err}"
    );
}

#[test]
fn two_sensors_need_a_choice() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board_with_two_sensors());
    let err = assemble(&bus, &Options::default()).err().unwrap();
    assert_eq!(err, Error::SensorCount(2));
    let chosen = assemble(
        &bus,
        &Options {
            sensor: Some("cam1-sensor".into()),
            mode: 0,
        },
    )
    .unwrap();
    assert_eq!(chosen.sensor().id, "cam1-sensor");
}

#[test]
fn starts_in_contract_order_and_the_sensor_last() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    let layout = camera.start().unwrap();
    assert_eq!(
        sim::calls(),
        [
            "sensor.select_mode",
            "receiver.configure",
            "isp.set_input",
            "receiver.start",
            "isp.start",
            "sensor.start",
        ]
    );
    assert_eq!((layout.stride, layout.size), (400, 16_000));
    camera.stop().unwrap();
    assert_eq!(
        sim::calls()[6..],
        ["sensor.stop", "isp.stop", "receiver.stop"]
    );
}

#[test]
fn controls_set_before_start_are_not_undone_by_selecting_the_mode() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    camera
        .set_controls(Controls {
            frame_length: Some(910),
            ..Controls::default()
        })
        .unwrap();
    camera.start().unwrap();
    let calls = sim::calls();
    assert_eq!(
        calls[..3],
        [
            "sensor.select_mode",
            "sensor.set_controls",
            "receiver.configure"
        ]
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "sensor.select_mode").count(),
        1,
        "selecting the mode again would reset the controls"
    );
    camera.stop().unwrap();
}

#[test]
fn streams_600_frames_at_120_fps_with_no_drops_and_saves_each() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    camera.start().unwrap();
    let dir = scratch("120fps");
    let mut sink = DirSink::new(&dir).unwrap();
    let report = capture(&mut camera, 600, &mut sink).unwrap();
    assert_eq!(report.frames, 600);
    assert_eq!(report.saved, 600);
    assert_eq!(report.dropped, 0, "{report:?}");
    assert_eq!(report.gaps, 0, "{report:?}");
    assert_eq!(report.first_sequence, 0);
    assert_eq!(report.last_sequence, 599);
    let fps = report.fps_milli().unwrap();
    assert!(
        (119_900..=120_100).contains(&fps),
        "measured {fps} milli-fps"
    );
    let mut files = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(entry.metadata().unwrap().len(), 16_000);
        files += 1;
    }
    assert_eq!(files, 600);
    assert!(dir.join("frame-00000000.raw").exists());
    assert!(dir.join("frame-00000599.raw").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_consumer_that_holds_every_buffer_makes_the_receiver_count_drops() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    camera.start().unwrap();
    let receiver = camera.receiver().clone();
    let stream = camera.stream().to_string();
    let mut frames = bus.open_frames(&receiver, &stream).unwrap();
    let mut held = Vec::new();
    while held.len() < 8 {
        match frames.pop().unwrap() {
            Some(frame) => held.push(frame),
            None => bus.wait().unwrap(),
        }
    }
    for _ in 0..4 {
        bus.wait().unwrap();
    }
    let counters = camera.counters().unwrap();
    assert_eq!(counters.frames, 8, "{counters:?}");
    assert!(counters.dropped >= 4, "{counters:?}");
    for frame in held {
        frames.release(frame).unwrap();
    }
}

#[test]
fn refuses_to_capture_before_it_starts() {
    let _serial = sim::serial();
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    let mut sink = DirSink::new(scratch("unstarted")).unwrap();
    assert_eq!(capture(&mut camera, 1, &mut sink), Err(Error::NotStarted));
}

/// The values the schedule test aims at each frame: exposure, gain code, frame length.
fn intended(control: usize, frame: u64) -> u32 {
    let f = frame as u32;
    match control {
        0 => 200 + (f * 37) % 800,
        1 => 16 + (f * 5) % 200,
        _ => 500 + (f * 13) % 1000,
    }
}

/// The value a frame shows for a control before any post can have landed on it, from the start.
const START: [u32; 3] = [100, 16, 1000];

/// Pops every embedded buffer waiting, in order, and checks what each frame shows. `next` is the
/// sequence the next frame must carry.
fn check_embedded(stream: &mut dyn Frames, delays: [u64; 3], next: &mut u64) -> Result<(), Error> {
    while let Some(frame) = stream.pop()? {
        let mut bytes = [0u8; sim::EMBEDDED_BYTES];
        stream.read(&frame, &mut bytes[..frame.bytes as usize])?;
        let shown = sim::decode_embedded(&bytes).expect("the sim's layout");
        assert_eq!(shown.frame, frame.sequence, "embedded data names its frame");
        assert_eq!(
            frame.sequence, *next,
            "no frame lost between the posts and the check"
        );
        let got = [shown.exposure, shown.gain, shown.frame_length];
        for (control, (&value, &delay)) in got.iter().zip(delays.iter()).enumerate() {
            let want = if shown.frame + 1 < delay {
                START[control]
            } else {
                intended(control, shown.frame)
            };
            assert_eq!(value, want, "control {control} on frame {}", shown.frame);
        }
        *next += 1;
        stream.release(frame)?;
    }
    Ok(())
}

#[test]
fn a_consumer_lands_1000_steps_of_unequal_delays_on_the_frames_it_aims_at() {
    let _serial = sim::serial();
    // Exposure 3 frames, gain 1, frame length 2: all different, none zero. The first two frames
    // are masked at the sensor, so the first published frame is number 2.
    let bus = sim::bus(&sim::board_with(
        Some("csi0"),
        "exposure_delay = 3, gain_delay = 1, frame_length_delay = 2, drop_frames = 2, embedded = true",
        true,
    ));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    camera.start().unwrap();
    let delays = {
        let mode = camera.mode();
        [
            mode.exposure_delay,
            mode.gain_delay,
            mode.frame_length_delay,
        ]
        .map(u64::from)
    };
    assert_eq!(delays, [3, 1, 2]);
    let mut receiver = bus.open::<dyn CameraReceiver>(camera.receiver()).unwrap();
    let mut name = [0u8; MAX_EMBEDDED_STREAM_NAME];
    let len = receiver.embedded_stream(&mut name).unwrap();
    let embedded_name = std::str::from_utf8(&name[..len]).unwrap();
    let mut embedded = bus.open_frames(camera.receiver(), embedded_name).unwrap();
    let mut frames = bus.open_frames(camera.receiver(), camera.stream()).unwrap();

    const TOTAL: u64 = 1002;
    let mut next = 2;
    while sim::emitted() < TOTAL {
        // The frame last emitted is `n - 1`, so a post now lands control `c` on `n - 1 + delay`.
        let n = sim::emitted();
        for (control, &delay) in delays.iter().enumerate() {
            let Some(target) = (n + delay).checked_sub(1) else {
                continue;
            };
            if target >= TOTAL {
                continue;
            }
            let mut controls = Controls::default();
            let value = intended(control, target);
            match control {
                0 => controls.exposure = Some(value),
                1 => controls.analogue_gain = Some(value),
                _ => controls.frame_length = Some(value),
            }
            camera
                .post_values(FrameValues {
                    identity: target,
                    controls,
                })
                .unwrap();
        }
        // Let the next frame go by, checking and releasing what the streams hold.
        while sim::emitted() == n {
            while let Some(frame) = frames.pop().unwrap() {
                frames.release(frame).unwrap();
            }
            check_embedded(&mut *embedded, delays, &mut next).unwrap();
            bus.wait().unwrap();
        }
    }
    check_embedded(&mut *embedded, delays, &mut next).unwrap();
    assert_eq!(next, TOTAL, "frames 2 to 1001 published and each checked");
    camera.stop().unwrap();
}

/// A free-running board: a frame every 10 ms, each post written for 3 ms, unequal delays (exposure
/// 3, gain 1, frame length 2) and two frames masked at the sensor.
const FREE_RUNNING: &str = "free_running = true, period_us = 10000, write_us = 3000, \
    exposure_delay = 3, gain_delay = 1, frame_length_delay = 2, drop_frames = 2, embedded = true";

/// A deterministic schedule, the same on every run.
struct Steps(u64);

impl Steps {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// What one free-running run found.
struct FreeRun {
    /// Per post, in order: the values posted (exposure, gain, frame length) and the frame each
    /// one landed on, as `post_values_at` answered it.
    posts: Vec<([u32; 3], [u64; 3])>,
    /// Per frame sequence, the values its embedded data shows. `None` for a masked frame.
    shown: Vec<Option<[u32; 3]>>,
    /// Posts whose group launch fell in a later frame than their write began in.
    straddles: u64,
    /// The frame count the receiver reports at the end, and the frames the sim has emitted.
    counted: (Option<u64>, u64),
}

/// Pops every embedded buffer waiting and records what each frame shows, by sequence.
fn drain_embedded(stream: &mut dyn Frames, shown: &mut Vec<Option<[u32; 3]>>) {
    while let Some(frame) = stream.pop().unwrap() {
        let mut bytes = [0u8; sim::EMBEDDED_BYTES];
        stream
            .read(&frame, &mut bytes[..frame.bytes as usize])
            .unwrap();
        let seen = sim::decode_embedded(&bytes).expect("the sim's layout");
        assert_eq!(seen.frame, frame.sequence, "embedded data names its frame");
        let at = frame.sequence as usize;
        if shown.len() <= at {
            shown.resize(at + 1, None);
        }
        shown[at] = Some([seen.exposure, seen.gain, seen.frame_length]);
        stream.release(frame).unwrap();
    }
}

/// Runs `steps` posts on a free-running board. Each post comes at a time of its own: 10 to 40 ms
/// after the last, so consecutive posts are at least one frame apart, and each is made where the
/// frame boundaries fall as they fall.
fn free_run(config: &str, steps: u64) -> FreeRun {
    let bus = sim::bus(&sim::board_with(Some("csi0"), config, true));
    let mut camera = assemble(&bus, &Options::default()).unwrap();
    camera.start().unwrap();
    let mut receiver = bus.open::<dyn CameraReceiver>(camera.receiver()).unwrap();
    let mut name = [0u8; MAX_EMBEDDED_STREAM_NAME];
    let len = receiver.embedded_stream(&mut name).unwrap();
    let embedded_name = std::str::from_utf8(&name[..len]).unwrap();
    let mut embedded = bus.open_frames(camera.receiver(), embedded_name).unwrap();
    let mut frames = bus.open_frames(camera.receiver(), camera.stream()).unwrap();

    let mut shown = Vec::new();
    let mut posts = Vec::new();
    let mut steps_taken = Steps(0x5eed_0000_0000_0001);
    bus.advance(Duration::from_millis(50));
    drain_embedded(&mut *embedded, &mut shown);
    while let Some(frame) = frames.pop().unwrap() {
        frames.release(frame).unwrap();
    }
    for step in 0..steps {
        let gap_us = 10_000 + steps_taken.below(30_000);
        bus.advance(Duration::from_micros(gap_us));
        drain_embedded(&mut *embedded, &mut shown);
        while let Some(frame) = frames.pop().unwrap() {
            frames.release(frame).unwrap();
        }
        // Every value differs from the last post's, so a frame shows the post it is named for.
        let s = step as u32;
        let values = [300 + s, 16 + s % 200, 500 + (s * 7) % 1000];
        let landing = camera
            .post_values_at(FrameValues {
                identity: step,
                controls: Controls {
                    exposure: Some(values[0]),
                    analogue_gain: Some(values[1]),
                    digital_gain: None,
                    frame_length: Some(values[2]),
                },
            })
            .unwrap();
        let landed = [
            landing.exposure,
            landing.analogue_gain,
            landing.frame_length,
        ]
        .map(|frame| frame.expect("every posted control has a landing"));
        posts.push((values, landed));
    }
    // Let every value land and its frame be published. The consumer keeps up while it waits, as it
    // does between posts: read every 10 ms, a frame or two at a time, so no frame is replaced
    // unread.
    for _ in 0..20 {
        bus.advance(Duration::from_millis(10));
        drain_embedded(&mut *embedded, &mut shown);
        while let Some(frame) = frames.pop().unwrap() {
            frames.release(frame).unwrap();
        }
    }
    let counted = (
        camera.frame_starts().unwrap().map(|starts| starts.count),
        sim::emitted(),
    );
    camera.stop().unwrap();
    FreeRun {
        posts,
        shown,
        straddles: sim::straddles(),
        counted,
    }
}

/// Counts the controls of every post whose answered frame does not show the posted value: a frame
/// the value is not in force on, or the frame before it still showing the value of the post before.
fn mismatches(run: &FreeRun) -> usize {
    let mut bad = 0;
    for (step, (values, landed)) in run.posts.iter().enumerate() {
        for control in 0..3 {
            let frame = landed[control] as usize;
            let shows = |at: usize| run.shown.get(at).copied().flatten().map(|v| v[control]);
            let now_ok = shows(frame) == Some(values[control]);
            let before_ok = match step.checked_sub(1) {
                None => true,
                Some(prev) => match shows(frame.wrapping_sub(1)) {
                    // A masked or unpublished frame before the landing says nothing either way.
                    None => true,
                    Some(seen) => seen == run.posts[prev].0[control],
                },
            };
            if !(now_ok && before_ok) {
                bad += 1;
            }
        }
    }
    bad
}

#[test]
fn a_free_running_landing_names_the_frame_its_values_show_on_over_1000_steps() {
    let _serial = sim::serial();
    let run = free_run(FREE_RUNNING, 1000);
    assert_eq!(run.posts.len(), 1000);
    assert!(
        run.straddles > 100,
        "the schedule must straddle often to prove anything: {} straddles",
        run.straddles
    );
    assert_eq!(
        mismatches(&run),
        0,
        "every posted control shows on the frame post_values_at named"
    );
    assert_eq!(
        run.counted,
        (Some(sim::emitted()), sim::emitted()),
        "frame_starts counts every frame start the receiver saw"
    );
    // Unequal delays (3, 1, 2): every post's three controls land on three different frames.
    let staggered = run
        .posts
        .iter()
        .filter(|(_, landed)| landed[0] != landed[1] && landed[1] != landed[2])
        .count();
    assert_eq!(staggered, 1000, "unequal delays land on unequal frames");
}

#[test]
fn without_the_straddle_rule_a_straddling_post_is_named_one_frame_early() {
    let _serial = sim::serial();
    let run = free_run(&format!("{FREE_RUNNING}, naive_landing = true"), 1000);
    assert!(run.straddles > 100, "{} straddles", run.straddles);
    let bad = mismatches(&run);
    assert!(
        bad > 100,
        "the same check must fail when the answer skips the straddle rule: {bad} mismatches"
    );
}

/// Records each frame's sequence and timestamp; the bytes are not kept.
#[derive(Default)]
struct Stamps(Vec<(u64, u64)>);

impl FrameSink for Stamps {
    fn save(&mut self, frame: &FrameSlot, _bytes: &[u8]) -> std::io::Result<()> {
        self.0.push((frame.sequence, frame.timestamp_ns));
        Ok(())
    }
}

/// One run's rates, in milli-fps: the rate asked for, the rate the frame length the contract
/// answers computes to, and the rate the frame timestamps measure.
struct Rates {
    asked: u32,
    lines: u32,
    computed: u32,
    measured: u64,
}

/// Asks for `asked_milli` on `mode`, sets the frame length the contract answers with before the
/// start, takes `frames` frames and measures the rate from their timestamps. Frame 0 is not
/// counted: the programmed frame length lands two frames after the start, so frame 0's interval
/// runs at the frame length in force before it.
fn rates_of(asked_milli: u32, mode: u16, frames: u64) -> Rates {
    let bus = sim::bus(&sim::board(Some("csi0")));
    let mut camera = assemble(&bus, &Options { sensor: None, mode }).unwrap();
    let m = *camera.mode();
    let (lines, computed) = frame_length_for_fps_milli(
        m.line_time_ps,
        m.frame_offset_ps,
        m.frame_length_min,
        m.frame_length_max,
        asked_milli,
    )
    .unwrap();
    camera
        .set_controls(Controls {
            frame_length: Some(lines),
            ..Controls::default()
        })
        .unwrap();
    camera.start().unwrap();
    let mut stamps = Stamps::default();
    let report = capture(&mut camera, frames, &mut stamps).unwrap();
    camera.stop().unwrap();
    assert_eq!((report.dropped, report.gaps), (0, 0), "{report:?}");
    let counted: Vec<u64> = stamps
        .0
        .iter()
        .filter(|(sequence, _)| *sequence >= 1)
        .map(|&(_, ns)| ns)
        .collect();
    let span = u128::from(counted[counted.len() - 1] - counted[0]);
    let measured = (counted.len() as u128 - 1) * 1_000_000_000_000 / span;
    Rates {
        asked: asked_milli,
        lines,
        computed,
        measured: measured as u64,
    }
}

/// The measured rate must equal the computed one to 200 parts per million (the rate proof's
/// tolerance). The run is written to stdout.
fn assert_measured_is_computed(r: &Rates) {
    let ppm =
        u128::from(r.measured.abs_diff(u64::from(r.computed))) * 1_000_000 / u128::from(r.computed);
    let _ = writeln!(
        std::io::stdout(),
        "asked {} milli-fps: frame length {} lines, computed {} milli-fps, measured {} milli-fps, difference {ppm} ppm",
        r.asked,
        r.lines,
        r.computed,
        r.measured
    );
    assert!(
        ppm <= 200,
        "asked {}: computed {} measured {} ({ppm} ppm)",
        r.asked,
        r.computed,
        r.measured
    );
}

#[test]
fn the_measured_rate_is_the_computed_rate_at_60_fps() {
    let _serial = sim::serial();
    assert_measured_is_computed(&rates_of(60_000, 0, 2000));
}

#[test]
fn the_measured_rate_is_the_computed_rate_at_120_fps() {
    let _serial = sim::serial();
    assert_measured_is_computed(&rates_of(120_000, 0, 2000));
}

#[test]
fn the_measured_rate_is_the_computed_rate_for_an_out_of_range_ask() {
    // 2000 fps is faster than the shortest frame (100 lines, 1200 fps): the contract answers with
    // that frame, and the frames must run at the rate it says.
    let _serial = sim::serial();
    let r = rates_of(2_000_000, 0, 2000);
    assert_eq!((r.lines, r.computed), (100, 1_200_000));
    assert_measured_is_computed(&r);
}

#[test]
fn the_measured_rate_is_the_computed_rate_in_the_wide_mode_at_30_fps() {
    let _serial = sim::serial();
    assert_measured_is_computed(&rates_of(30_000, 1, 2000));
}
