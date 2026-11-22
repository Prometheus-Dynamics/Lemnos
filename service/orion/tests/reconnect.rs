//! A comm core that goes away and comes back, on a short heartbeat: the bridge reconnects, and its
//! topics, its resources and `describe` answer again without waiting for any change. This binary
//! runs this one test alone, so its log (`lemnos-orion:` lines, and the `reconnect:` lines the test
//! writes) is one session's, with nothing interleaved.
#![allow(clippy::print_stderr)]

use orion::provider::ResourceState;

#[allow(dead_code)]
#[path = "support/reconnect_kit.rs"]
mod kit;
use kit::*;

#[path = "support/relay.rs"]
mod relay;
use relay::Relay;

/// A stalled link that stays open keeps its session. Nothing is torn down or published again while
/// the comm core and the bridge hear each other no more: the session is quiet, not gone, and the
/// bridge does not connect again. When the stall ends, publishing continues on the same session.
#[test]
fn a_stalled_open_link_keeps_its_session_and_publishing_resumes_on_it() {
    let dir = dir("stalled");
    let (board, _logic, _service, lemnosd) = start_relayed(&dir);
    let board_socket = board.socket().to_path_buf();
    let relay = Relay::start(
        &lemnos_socket_dir::socket(&dir, "relay.sock").unwrap(),
        board_socket.clone(),
    );
    let _bridge = start_bridge(
        &lemnosd,
        &lemnos_socket_dir::socket(&dir, "relay.sock").unwrap(),
    );
    let dash = dashboard(&board_socket);
    let mut count = wait_for("the counter's first value", || {
        dash.subscribe("lemnos.thing.values").ok()
    });
    let before = wait_for("the counter has a value", || counter(&mut count));
    wait_for("the imu's resource", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(relay.accepted(), 1, "the first session");

    // The link stalls, open. Three times the comm core's idle time: a session that was closed by its
    // silence would be closed by now. The session is kept, so the resource is still there.
    relay.stall();
    std::thread::sleep(IDLE * 3);
    assert_eq!(
        relay.accepted(),
        1,
        "a stalled link made the bridge connect again"
    );
    assert!(
        resource(&dash, "lemnos.imu").is_some(),
        "a stalled link withdrew the bridge's resources"
    );

    // The stall ends: the same session carries on, and the counter moves on it.
    relay.resume();
    wait_for("publishing resumes after the stall", || {
        counter(&mut count).filter(|value| *value > before)
    });
    assert_eq!(relay.accepted(), 1, "publishing resumed on a new session");
}

/// A closed link is lost at once: the bridge's connection closes, and the bridge takes the session as
/// lost and connects again. The session it had is published again on the new connection. The test
/// waits for events, not for a time: the bridge connects again, then the resource and the description
/// are back; the outer deadline only catches a hang.
#[test]
fn a_closed_link_is_lost_and_the_session_is_published_again() {
    let dir = dir("closed");
    let (board, _logic, _service, lemnosd) = start_relayed(&dir);
    let board_socket = board.socket().to_path_buf();
    let relay_socket = lemnos_socket_dir::socket(&dir, "relay.sock").unwrap();
    let relay = Relay::start(&relay_socket, board_socket.clone());
    let _bridge = start_bridge(&lemnosd, &relay_socket);
    let dash = dashboard(&board_socket);
    let mut topic = wait_for("the imu's description topic", || {
        dash.subscribe("lemnos.imu.description").ok()
    });
    let first = wait_for("the imu's description generation", || {
        topic_generation(&mut topic)
    });
    wait_for("the imu's resource", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(relay.accepted(), 1, "the first session");

    // The link closes: both of its connections, and the relay with them.
    drop(relay);
    // The link is back. The bridge connects again, and that is its new session.
    let relay = Relay::start(&relay_socket, board_socket.clone());
    wait_for("the bridge connects again after the link closed", || {
        (relay.accepted() >= 1).then_some(())
    });
    wait_for("the imu's resource is published again", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(
        described_generation(&dash),
        first,
        "the description after the link came back"
    );
}
