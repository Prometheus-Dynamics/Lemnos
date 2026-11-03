//! Look presets over the socket: applying one is remembered across a restart
//! of the service, and a client outside the board's `look_writers` (by default
//! `lemnos-ctl` and `orion:*`) may not change looks.

use lemnos_ipc::{ClientError, ClientOptions, LooksOp};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::path::PathBuf;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
look_writers = ["manager"]

[board]
id = "example"
"#;

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnosd-presets-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    // The example schemes are saved presets (the device package ships them).
    let presets = root.join("state/presets");
    std::fs::create_dir_all(&presets).unwrap();
    let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/looks/presets");
    for scheme in ["scheme-a", "scheme-b", "scheme-c"] {
        std::fs::copy(
            format!("{examples}/{scheme}.toml"),
            presets.join(format!("{scheme}.toml")),
        )
        .unwrap();
    }
    root
}

#[test]
fn the_default_writers_are_the_operator_and_orion() {
    let root = root("defaults");
    let board = BOARD.replace("look_writers = [\"manager\"]\n", "");
    let service = MockLemnosd::start_with_state(
        root.clone(),
        &board,
        MockHardware::new(),
        Some(root.join("state")),
    )
    .unwrap();
    let mut manager = ClientOptions::new(service.socket(), "manager")
        .leds()
        .unwrap();
    match manager.looks(LooksOp::PresetApply("scheme-a".into())) {
        Err(ClientError::Rejected(reason)) => {
            assert!(reason.contains("lemnos-ctl, orion:*"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let mut ctl = ClientOptions::new(service.socket(), "lemnos-ctl")
        .leds()
        .unwrap();
    ctl.looks(LooksOp::PresetApply("scheme-a".into())).unwrap();
    drop(service);
}

#[test]
fn an_applied_preset_survives_a_restart_and_only_operators_may_apply() {
    let root = root("restart");
    let service = MockLemnosd::start_with_state(
        root.clone(),
        BOARD,
        MockHardware::new(),
        Some(root.join("state")),
    )
    .unwrap();
    let mut manager = ClientOptions::new(service.socket(), "manager")
        .leds()
        .unwrap();
    let listing = manager.looks(LooksOp::PresetList).unwrap();
    assert!(listing.contains("scheme-a (saved)"), "{listing}");
    assert!(
        !listing.contains('*'),
        "no preset is active at first: {listing}"
    );

    // A client that is not in the board's look_writers may not change looks.
    let mut app = ClientOptions::new(service.socket(), "app").leds().unwrap();
    match app.looks(LooksOp::PresetApply("scheme-a".into())) {
        Err(ClientError::Rejected(reason)) => {
            assert!(reason.contains("may not change"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Reads are open to every client.
    assert!(app.looks(LooksOp::PresetList).is_ok());

    manager
        .looks(LooksOp::PresetApply("scheme-c".into()))
        .unwrap();
    service.stop_keep_root();

    let service = MockLemnosd::start_with_state(
        root.clone(),
        BOARD,
        MockHardware::new(),
        Some(root.join("state")),
    )
    .unwrap();
    let mut manager = ClientOptions::new(service.socket(), "manager")
        .leds()
        .unwrap();
    let listing = manager.looks(LooksOp::PresetList).unwrap();
    assert!(listing.contains("* scheme-c (saved)"), "{listing}");
    drop(service);
}
