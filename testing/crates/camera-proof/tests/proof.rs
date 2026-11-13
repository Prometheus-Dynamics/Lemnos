//! The camera proof, against simulated drivers on an in-process host with simulated time.

mod sim;

use std::path::PathBuf;

use lemnos_camera_proof::{Bus, DirSink, Error, Frames, Options, assemble, capture};
use lemnos_contract_camera_receiver::{CameraReceiver, MAX_EMBEDDED_STREAM_NAME};
use lemnos_contract_camera_sensor::{Controls, FrameValues};

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
