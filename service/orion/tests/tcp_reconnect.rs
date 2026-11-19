//! Orion's silence check over TCP, through a relay on 127.0.0.1 that stalls, resumes and closes the link.
//!
//! What these tests cover, at the pinned Orion revision (8e37a8a):
//! - The bridge publishes as a provider, and the network carrier (`orion+tcp://`) offers only the `CLIENT`
//!   and `PEER` roles (`crates/comm/tcp/src/lib.rs`, `NETWORK_ROLES`), so the bridge cannot publish over TCP.
//!   It is refused at start (`service/orion/src/config.rs`, `publishes_on`) and, if the address is not known
//!   to be wrong until connect, once at connect (`service/orion/src/publish.rs`, `Opening::Refused`). The
//!   bridge here reaches its comm core on the local socket, as a deployment does.
//! - A `CLIENT` session over the TCP relay is what a remote caller is. Its link is the relay, so the relay's
//!   stall, close and reset are real link failures seen by the node. The tests assert on the bridge's side:
//!   the holds the caller took are released at lemnosd, and its attached run's job is gone from lemnosd.
//!   The client's own silence check (`crates/client/connect/src/alive.rs`) is exercised in the short-stall
//!   case and where the client pumps.
//!
//! The threshold, read at the pinned Orion revision (8e37a8a), with the file and line of each fact:
//! - a session over the network gives up once it has heard nothing for its idle time: `alive.rs`
//!   (`silent_at` is `None` only for a local transport, so `orion+tcp://` has the check);
//! - the idle time the client uses is the comm core's `idle_ms` from the welcome, or three heartbeats when
//!   it says zero, and never under one heartbeat: `crates/client/connect/src/lib.rs` (`Welcomed` `idle`);
//! - the comm core's own idle time is `comm.idle_ms`, or three heartbeats when zero
//!   (`crates/comm/server/src/limits.rs`, `Limits::idle_ms`); the one-second floor is applied only to
//!   limits read from settings (`from_source`), and the stub board builds its `Limits` directly, so a test's
//!   heartbeat is the comm core's real configuration;
//! - the comm core drops a network session it has heard nothing from for that long
//!   (`crates/comm/server/src/driver.rs`, `quiet`: `Dropped::Quiet` for a session that is not local);
//! - the client pings every half heartbeat (`alive.rs`, `keep_alive`), and only while the program pumps the
//!   session (`Session::pump`), so a test that wants the link to carry pings must pump.
//!
//! With a heartbeat of [`HEARTBEAT_MS`] the idle time is three of them, [`IDLE`]. No clock is faked: these are
//! the real timers at their real values, and the tests wait on what the client and the relay report.
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::time::Duration;

use lemnos_driver_system_identity::Identity;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter};
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Outcome, Target};
use orion::provider::ResourceState;
use orion::{Address, ClientError, Error, Orion};
use orion_stubs::{Board, Options as BoardOptions};

#[allow(dead_code)]
#[path = "support/reconnect_kit.rs"]
mod kit;
#[allow(dead_code)]
#[path = "support/tcp_relay.rs"]
mod tcp_relay;
#[allow(dead_code)]
#[path = "support/tracked.rs"]
mod tracked;

use kit::{BOARD, Bridge, Logic, Scratch, dashboard, dir, resource, run, start_bridge, wait_for};
use tcp_relay::TcpRelay;
use tracked::TrackedDevice;

/// The comm core's heartbeat for these runs: the idle time is three of them, and the client pings every
/// half heartbeat (`crates/client/connect/src/alive.rs`; `crates/comm/server/src/limits.rs`, `idle_ms`).
/// Short enough that the tests are quick, not so short that a loaded machine misses three in a row.
const HEARTBEAT_MS: u32 = 300;
/// A stall shorter than the idle time: one heartbeat, a third of the idle time.
const SHORT_STALL: Duration = Duration::from_millis(HEARTBEAT_MS as u64);
/// How long one pump waits for a frame. A loop that pumps repeats until its event, so this only sets how
/// often it looks.
const PUMP: Duration = Duration::from_millis(20);

fn board_options() -> BoardOptions {
    BoardOptions {
        for_node: true,
        heartbeat_ms: HEARTBEAT_MS,
        ..BoardOptions::default()
    }
}

/// lemnosd with the tracked device, which takes holds and runs a long job until it is cancelled.
fn start_lemnosd(socket: &Path) -> Service {
    let board = format!("{BOARD}[[devices]]\nid = \"tracked\"\ndriver = \"tracked\"\n");
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(&board).expect("board"),
        drivers: vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeCounter>(),
            entry::<Identity>(),
            entry::<TrackedDevice>(),
        ],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// The whole stack: the board's comm core on TCP and its node logic, lemnosd, the bridge on the board's
/// local socket, a relay the remote caller reaches the board through, and a dashboard on the local socket.
/// Fields are dropped in this order: the dashboard and the bridge first, then the relay.
struct Up {
    dash: Orion,
    _bridge: Bridge,
    relay: TcpRelay,
    _service: Service,
    _logic: Logic,
    _board: Board,
    _dir: Scratch,
}

fn up(test: &str) -> Up {
    let dir = dir(test);
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_with(&dir, board_options());
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&lemnosd);
    let relay = TcpRelay::start(board.network());
    let bridge = start_bridge(&lemnosd, board.socket());
    let dash = dashboard(board.socket());
    wait_for("the tracked device's resource", || {
        resource(&dash, "lemnos.tracked").filter(|found| found.state == ResourceState::Ready)
    });
    Up {
        _dir: dir,
        dash,
        _bridge: bridge,
        relay,
        _service: service,
        _logic: logic,
        _board: board,
    }
}

/// A remote caller: a `CLIENT` session over the relay to the board's network port.
fn caller(relay: &TcpRelay) -> Orion {
    wait_for("a remote caller over the relay", || {
        Orion::connect(
            &Address::Network {
                host: relay.addr().to_string(),
                key: None,
            },
            "caller",
        )
        .ok()
    })
}

/// The tracked device's action, on the device's resource.
fn tracked_action(args: &'static str) -> ActionCall<'static> {
    ActionCall {
        name: "lemnos.tracked.call",
        target: Target::Resource("lemnos.tracked".parse().expect("name")),
        args: args.as_bytes(),
        deadline_ms: 0,
        detached: false,
    }
}

/// The holds the tracked device counts, read through the bridge on the local dashboard.
fn held(dash: &Orion) -> String {
    let finished = run(dash, "lemnos.tracked.call", "lemnos.tracked", "held");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    String::from_utf8(finished.output).expect("text")
}

/// Whether lemnosd has a long job open on the device, from `describe`: a job is a `job` record there while
/// it is open (`service/orion/src/description.rs`), and a finished job is gone.
fn job_open(dash: &Orion) -> bool {
    let finished = run(dash, "lemnos.tracked.call", "lemnos.tracked", "describe");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    String::from_utf8(finished.output)
        .expect("text")
        .lines()
        .any(|line| line.starts_with("job "))
}

/// A remote caller takes a hold and starts a long job, attached. Its link goes silent while open: the
/// node ends its session, the bridge gets `CallerGone`, releases the caller's holds at lemnosd and cancels
/// its run, so the job is gone from lemnosd. Asserted on the bridge's side. The caller is dropped only
/// after the stall: the client's session is still alive until then, so the relay sees no end of link.
#[test]
fn a_silent_tcp_caller_is_ended_and_the_bridge_releases_its_holds_and_cancels_its_run() {
    let up = up("tcp-silent");
    let remote = caller(&up.relay);
    let took = run(&remote, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(took.outcome, Outcome::Succeeded, "{took:?}");
    assert_eq!(held(&up.dash), "1", "the remote caller's hold");
    let long = remote
        .run(&tracked_action("hold_until_cancelled"))
        .expect("the long job");
    wait_for("the job is open at lemnosd", || {
        job_open(&up.dash).then_some(())
    });

    // The link goes silent and the caller sends nothing: the node's silence check ends the session.
    up.relay.stall();
    wait_for("the holds are released at lemnosd", || {
        (held(&up.dash) == "0").then_some(())
    });
    wait_for("the run's job is cancelled and gone at lemnosd", || {
        (!job_open(&up.dash)).then_some(())
    });

    drop(long);
    drop(remote);
    up.relay.resume();
    wait_for("the old connection is closed once it carries on", || {
        (up.relay.ended() >= 1).then_some(())
    });
}

/// A short stall, below the idle time: the node keeps the remote caller's session, so its hold and its
/// attached job stay until the link carries on, and the caller answers when it does.
#[test]
fn a_short_tcp_stall_below_the_silence_check_keeps_the_caller_its_hold_and_its_run() {
    let up = up("tcp-short");
    let remote = caller(&up.relay);
    let took = run(&remote, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(took.outcome, Outcome::Succeeded, "{took:?}");
    let long = remote
        .run(&tracked_action("hold_until_cancelled"))
        .expect("the long job");
    wait_for("the job is open at lemnosd", || {
        job_open(&up.dash).then_some(())
    });

    up.relay.stall();
    // The length of the stall is the condition under test; its outcome is checked below.
    std::thread::sleep(SHORT_STALL);
    up.relay.resume();

    let answer = run(&remote, "lemnos.tracked.call", "lemnos.tracked", "held");
    assert_eq!(answer.outcome, Outcome::Succeeded, "{answer:?}");
    assert_eq!(
        held(&up.dash),
        "1",
        "a short stall released the caller's hold"
    );
    assert!(job_open(&up.dash), "a short stall ended the caller's job");
    assert_eq!(
        up.relay.accepted(),
        1,
        "a short stall made the caller connect again"
    );
    drop(long);
}

/// A link the relay closes, with nothing unread: the node reads the end of the link and ends the session.
/// The bridge releases the caller's hold and cancels its run.
#[test]
fn a_closed_tcp_caller_releases_its_holds_and_its_run_is_cancelled() {
    let up = up("tcp-closed");
    let remote = caller(&up.relay);
    let took = run(&remote, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(took.outcome, Outcome::Succeeded, "{took:?}");
    let long = remote
        .run(&tracked_action("hold_until_cancelled"))
        .expect("the long job");
    wait_for("the job is open at lemnosd", || {
        job_open(&up.dash).then_some(())
    });

    up.relay.close();
    wait_for("the holds are released at lemnosd", || {
        (held(&up.dash) == "0").then_some(())
    });
    wait_for("the run's job is cancelled and gone at lemnosd", || {
        (!job_open(&up.dash)).then_some(())
    });
    // The caller's end sees the link close: its session is lost.
    wait_for("the caller's session is lost", || match remote.pump(PUMP) {
        Err(Error::Client(ClientError::Lost)) => Some(()),
        _ => None,
    });
    drop(long);
}

/// A link closed while the relay holds bytes from the caller it has not read: the kernel answers the close
/// with a reset, a real failure of the socket. The bridge still releases the caller's hold. The caller's
/// own pings, sent while it pumps, are the unread bytes; the relay reports them by peeking while stalled.
#[test]
fn a_tcp_caller_reset_with_unread_bytes_releases_its_holds() {
    let up = up("tcp-reset");
    let remote = caller(&up.relay);
    let took = run(&remote, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(took.outcome, Outcome::Succeeded, "{took:?}");
    assert_eq!(held(&up.dash), "1", "the remote caller's hold");

    up.relay.stall();
    wait_for(
        "the relay holds bytes from the caller that it has not read",
        || {
            let _ = remote.pump(PUMP);
            up.relay.unread().then_some(())
        },
    );
    up.relay.close();
    wait_for("the holds are released at lemnosd", || {
        (held(&up.dash) == "0").then_some(())
    });
}

/// A bridge that is given a carrier which cannot publish is refused once, in words, and stops: it is a
/// configuration error, so it is not retried. The network carrier is refused at connect here, since the
/// bridge is handed the address directly, without the start-time check (`config.rs`, `publishes_on`).
#[test]
fn a_bridge_given_a_carrier_that_cannot_publish_is_refused_once_and_stops() {
    let dir = dir("tcp-refused");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_with(&dir, board_options());
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&lemnosd);
    let relay = TcpRelay::start(board.network());
    let bridge = kit::start_bridge_to(
        &lemnosd,
        Address::Network {
            host: relay.addr().to_string(),
            key: None,
        },
    );
    let why = wait_for("the bridge refuses its carrier", || bridge.shared.refusal());
    assert!(why.contains("provider role"), "the refusal says why: {why}");
    assert!(
        why.contains("ORION_ADDRESS"),
        "the refusal says what to set: {why}"
    );
    // One connection attempt, which the comm core refused; the bridge stopped rather than retrying.
    assert_eq!(
        relay.accepted(),
        1,
        "a refused bridge connected more than once"
    );
    drop((service, logic, board, bridge));
}
