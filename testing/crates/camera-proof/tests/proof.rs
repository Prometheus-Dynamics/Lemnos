//! The camera proof, against simulated drivers on an in-process host with simulated time.

mod sim;

use std::path::PathBuf;

use lemnos_camera_proof::{Bus, DirSink, Error, Options, assemble, capture};

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
