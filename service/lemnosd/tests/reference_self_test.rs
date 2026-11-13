//! The reference board's self-test device validates: each member it names is a device on the
//! board that serves `self-test/1`, so the device binds; a name the board does not have refuses it.

use std::sync::Arc;

use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options};
use lemnos_sim::SimBoard;
use lemnosd::builtin;

const REFERENCE: &str = include_str!("../../../testing/boards/reference.toml");

fn start(text: &str) -> Host {
    let board = parse(text).expect("the board parses");
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &board,
        &builtin::drivers(),
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .expect("the board starts")
}

#[test]
fn the_reference_board_self_test_binds_with_its_members() {
    let host = start(REFERENCE);
    let device = host
        .device("selftest")
        .expect("the board's self-test device");
    assert_eq!(device.state, DeviceState::Running, "{:?}", device.state);
}

#[test]
fn a_member_the_board_does_not_have_refuses_the_self_test() {
    let text = REFERENCE.replace(
        r#"members = ["watchdog", "mag", "power"]"#,
        r#"members = ["watchdog", "no-such-member"]"#,
    );
    assert_ne!(text, REFERENCE, "the members line is in the board file");
    let host = start(&text);
    let device = host
        .device("selftest")
        .expect("the board's self-test device");
    assert_ne!(
        device.state,
        DeviceState::Running,
        "a name the board lacks binds"
    );
}
