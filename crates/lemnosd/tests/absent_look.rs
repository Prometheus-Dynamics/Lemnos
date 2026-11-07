//! The board's absent look (`absent_look`): the service holds it at its
//! priority, so the ring shows it while no client above that priority holds
//! the light, and falls back to it when such a client goes.

use lemnos_ipc::{ClientEvent, ClientOptions, FrameUpdate};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::time::{Duration, Instant};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/leds0"
config = { count = 4, fade_ms = 0, absent_look = "status.warn", absent_priority = 20 }
"#;

fn start() -> MockLemnosd {
    let root = std::env::temp_dir().join(format!("lemnosd-absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), [0u8; 16]).unwrap();
    MockLemnosd::start_in(root, BOARD, MockHardware::new()).unwrap()
}

/// Waits for the watch's description to say `owner` holds the ring with
/// `look` (empty: any look), and returns it.
fn held_by(leds: &mut lemnos_ipc::LedClient, owner: &str, look: &str) -> lemnos_ipc::LightInfo {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Some(ClientEvent::Data(FrameUpdate::Info(info))) =
            leds.next_frame(Some(left)).unwrap()
        {
            if info.owner == owner && (look.is_empty() || info.look == look) {
                return info;
            }
            last = Some(info);
        }
    }
    panic!("the ring never showed {owner}'s {look:?}; last: {last:?}");
}

#[test]
fn the_absent_look_shows_until_a_higher_client_and_comes_back_when_it_goes() {
    let service = start();
    let mut watcher = ClientOptions::new(service.socket(), "watcher")
        .leds()
        .unwrap();
    watcher.watch_frames("ring", 50).unwrap();
    // Nobody connected: the service's absent look, in the application layer.
    let info = held_by(&mut watcher, "lemnosd", "status.warn");
    assert_eq!(info.layer, "app");

    // A client under the absent look's priority does not show.
    let mut low = ClientOptions::new(service.socket(), "low")
        .priority(10)
        .leds()
        .unwrap();
    low.color(0x00ff_0000).unwrap();
    low.sync().unwrap();
    // A client at the default priority (50) shows over it...
    let mut api = ClientOptions::new(service.socket(), "api").leds().unwrap();
    api.look("status.ok").unwrap();
    api.sync().unwrap();
    held_by(&mut watcher, "api", "status.ok");
    // ...and when it goes, the ring falls back to the absent look, not to the
    // lower client.
    drop(api);
    held_by(&mut watcher, "lemnosd", "status.warn");
    drop(low);
    drop(service);
}
