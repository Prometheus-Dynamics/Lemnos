//! Look presets over the socket: applying one is remembered across a restart
//! of the service, and a client outside Atlas, Orion and the operator's
//! `lemnos-ctl` may not change looks.

use lemnos_ipc::{ClientError, ClientOptions, LooksOp};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::path::PathBuf;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"
"#;

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnosd-presets-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    root
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
    let mut atlas = ClientOptions::new(service.socket(), "atlas")
        .leds()
        .unwrap();
    let listing = atlas.looks(LooksOp::PresetList).unwrap();
    assert!(listing.contains("* scheme-b (built-in)"), "{listing}");

    // A client that is not Atlas, Orion or the operator may not change looks.
    let mut helios = ClientOptions::new(service.socket(), "helios")
        .leds()
        .unwrap();
    match helios.looks(LooksOp::PresetApply("scheme-a".into())) {
        Err(ClientError::Rejected(reason)) => {
            assert!(reason.contains("may not change"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Reads are open to every client.
    assert!(helios.looks(LooksOp::PresetList).is_ok());

    atlas
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
    let mut atlas = ClientOptions::new(service.socket(), "atlas")
        .leds()
        .unwrap();
    let listing = atlas.looks(LooksOp::PresetList).unwrap();
    assert!(listing.contains("* scheme-c"), "{listing}");
    drop(service);
}
