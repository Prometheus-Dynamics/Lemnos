//! The ring's runtime brightness (`Request::LightSetting`) over the mock
//! server: it scales the ring at once, a persisted value is applied at the
//! next start over the board's, and the light's writers apply.

use lemnos_ipc::{ClientError, ClientEvent, ClientOptions, FrameUpdate, LedClient, Refusal};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::path::PathBuf;
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
writers = ["atlas"]
config = { count = 4, fade_ms = 0, look_brightness = 1.0 }
"#;

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnosd-light-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), [0u8; 16]).unwrap();
    root
}

/// The red of the ring's first LED, once the ring shows red.
fn red_after_colour(socket: &std::path::Path) -> u32 {
    let mut leds: LedClient = ClientOptions::new(socket, "atlas").leds().unwrap();
    leds.watch_frames("ring", 50).unwrap();
    leds.color(0xff0000).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline, "no red frame");
        if let Ok(Some(ClientEvent::Data(FrameUpdate::Frame(frame)))) =
            leds.next_frame(Some(Duration::from_millis(100)))
            && let Some(pixel) = frame.pixels.first()
            && (pixel >> 16) & 0xff > 0
        {
            return (pixel >> 16) & 0xff;
        }
    }
}

#[test]
fn the_ring_brightness_scales_the_frames_at_once_and_is_refused_for_others() {
    let root = root("scale");
    let service = MockLemnosd::start_in(root.clone(), BOARD, MockHardware::new()).unwrap();
    let mut atlas = ClientOptions::new(service.socket(), "atlas")
        .devices()
        .unwrap();
    assert_eq!(atlas.light_brightness("ring", 1.0, false).unwrap(), 1.0);
    let full = red_after_colour(service.socket());
    assert_eq!(atlas.light_brightness("ring", 0.2, false).unwrap(), 0.2);
    let dim = red_after_colour(service.socket());
    assert!(dim < full / 2, "dim {dim} vs full {full}");

    let mut helios = ClientOptions::new(service.socket(), "helios")
        .devices()
        .unwrap();
    match helios.light_brightness("ring", 0.5, false) {
        Err(ClientError::Refused(Refusal::NotAllowed)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    drop(service);
}

#[test]
fn a_persisted_brightness_is_the_start_value_after_a_restart() {
    let root = root("persist");
    let service = MockLemnosd::start_with_state(
        root.clone(),
        BOARD,
        MockHardware::new(),
        Some(root.join("state")),
    )
    .unwrap();
    let mut atlas = ClientOptions::new(service.socket(), "atlas")
        .devices()
        .unwrap();
    atlas.light_brightness("ring", 0.2, true).unwrap();
    let saved = std::fs::read_to_string(root.join("state/light/ring.brightness")).unwrap();
    assert_eq!(saved.trim(), "200");
    let dim = red_after_colour(service.socket());
    service.stop_keep_root();

    // The board says full; the saved 0.2 is what the ring starts at.
    let service = MockLemnosd::start_with_state(
        root.clone(),
        BOARD,
        MockHardware::new(),
        Some(root.join("state")),
    )
    .unwrap();
    assert_eq!(red_after_colour(service.socket()), dim);
    drop(service);
}
