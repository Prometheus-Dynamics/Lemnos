//! Frame watches end to end over the mock server: a watch sends the light's
//! description and its current frame, then each frame that changes (and only
//! those), at the granted rate; ending the watch stops them, and a device
//! that is not a light is refused.

use lemnos_ipc::{ClientError, ClientEvent, ClientOptions, FrameUpdate, Refusal};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::time::Duration;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/leds0"
config = { count = 4, fade_ms = 0, offset = 1, direction = "ccw", look_brightness = 1.0 }

[[devices]]
id = "usb-a-power"
driver = "gpio-power-switch"
config = { chip = "pinctrl-rp1", line = 20, default_on = true }
"#;

/// A service over the mock hardware, with the ring's file in place (the
/// ws2812 driver builds only where its device node exists).
fn start() -> MockLemnosd {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "lemnosd-frames-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), [0u8; 16]).unwrap();
    MockLemnosd::start_in(root, BOARD, MockHardware::new()).unwrap()
}

/// The next frame of the watch within `wait` (descriptions are skipped).
fn next_frame(leds: &mut lemnos_ipc::LedClient, wait: Duration) -> Option<lemnos_ipc::LightFrame> {
    let deadline = std::time::Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        if let Some(ClientEvent::Data(FrameUpdate::Frame(frame))) =
            leds.next_frame(Some(left)).unwrap()
        {
            return Some(frame);
        }
    }
    None
}

/// The next description of the watch within `wait`.
fn next_info(leds: &mut lemnos_ipc::LedClient, wait: Duration) -> Option<lemnos_ipc::LightInfo> {
    let deadline = std::time::Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        if let Some(ClientEvent::Data(FrameUpdate::Info(info))) =
            leds.next_frame(Some(left)).unwrap()
        {
            return Some(info);
        }
    }
    None
}

/// Waits until a frame with `colour` on every LED arrives.
fn frame_of(leds: &mut lemnos_ipc::LedClient, colour: u32) -> lemnos_ipc::LightFrame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = next_frame(leds, Duration::from_millis(200));
        if let Some(frame) = frame
            && frame.pixels.iter().all(|p| *p == colour)
        {
            return frame;
        }
        assert!(std::time::Instant::now() < deadline, "no {colour:#x} frame");
    }
}

#[test]
fn a_watch_sends_the_description_and_each_change_once() {
    let service = start();
    let mut leds = ClientOptions::new(service.socket(), "atlas")
        .leds()
        .unwrap();
    // A colour before the watch: the watch starts from what the ring shows.
    leds.color(0xff0000).unwrap();
    leds.sync().unwrap();
    assert_eq!(leds.watch_frames("ring", 50).unwrap(), 50);

    let info = next_info(&mut leds, Duration::from_secs(5)).expect("a description");
    assert_eq!(info.device, "ring");
    assert_eq!(info.count, 4);
    assert_eq!(info.offset, 1);
    assert!(!info.clockwise);
    assert_eq!(info.owner, "atlas");

    // A colour fill shows at the look's 70% (the frame is what the ring shows).
    let first = frame_of(&mut leds, 0x00b2_0000);
    assert_eq!(first.device, "ring");
    assert_eq!(first.pixels, vec![0x00b2_0000; 4]);

    // Nothing changes: no frame arrives.
    assert!(next_frame(&mut leds, Duration::from_millis(300)).is_none());

    // A change arrives as a new frame.
    leds.color(0x0000_ff00).unwrap();
    let second = frame_of(&mut leds, 0x0000_b200);
    assert!(second.seq > first.seq);
    drop(service);
}

#[test]
fn ending_the_watch_stops_the_frames() {
    let service = start();
    let mut leds = ClientOptions::new(service.socket(), "atlas")
        .leds()
        .unwrap();
    assert_eq!(leds.watch_frames("ring", 20).unwrap(), 20);
    leds.color(0x0000_00ff).unwrap();
    frame_of(&mut leds, 0x0000_00b2);
    assert_eq!(leds.watch_frames("ring", 0).unwrap(), 0);
    leds.color(0x00ff_ffff).unwrap();
    assert!(next_frame(&mut leds, Duration::from_millis(400)).is_none());
    drop(service);
}

#[test]
fn only_lights_can_be_watched() {
    let hw = MockHardware::new();
    hw.label_chip("pinctrl-rp1", "gpiochip0");
    let service = start();
    let mut leds = ClientOptions::new(service.socket(), "atlas")
        .leds()
        .unwrap();
    match leds.watch_frames("usb-a-power", 20) {
        Err(ClientError::Refused(Refusal::Unsupported)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
    match leds.watch_frames("nope", 20) {
        Err(ClientError::Refused(Refusal::UnknownDevice)) => {}
        other => panic!("expected UnknownDevice, got {other:?}"),
    }
    drop(service);
}
