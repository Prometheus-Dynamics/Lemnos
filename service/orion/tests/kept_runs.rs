//! A detached run of a device action is kept by node logic while the bridge's session is lost, and
//! given back when a session of the bridge's identity registers the action again. These tests lose
//! the bridge's session on a relay (stalled, or dropped and restarted) and check what the caller sees:
//! a run that carries on and succeeds, one that ends while the bridge is away, a cancel asked while
//! away, a run not given back in time, and a node below the level that keeps runs.
//!
//! The node is real node logic (`Logic`), behind a comm core that welcomes at the test's level. A
//! registration without a window is not kept by that node, so the fail-before plant (the bridge
//! registering without `action_kept`) turns the give-back tests red.
#![allow(clippy::print_stderr)]

use std::time::Duration;

use orion::Orion;
use orion::actions::{ActionCall, Outcome, Target, Update};
use orion::provider::ResourceState;
use orion_stubs::Board;

#[allow(dead_code)]
#[path = "support/reconnect_kit.rs"]
mod kit;
use kit::*;

#[path = "support/relay.rs"]
#[allow(dead_code)]
mod relay;
use relay::Relay;

#[path = "support/tracked.rs"]
mod tracked;
use tracked::TrackedDevice;

/// Lemnosd with the tracked device as well as the test board's devices.
fn start_lemnosd_tracked(socket: &std::path::Path) -> lemnosd::Service {
    let board_text = format!("{BOARD}[[devices]]\nid = \"tracked\"\ndriver = \"tracked\"\n");
    lemnosd::Service::start(lemnosd::Config {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: lemnos_host::board::parse(&board_text).expect("board"),
        drivers: vec![
            lemnos_host::entry::<lemnos_fake_devices::FakeAccelerometer>(),
            lemnos_host::entry::<lemnos_fake_devices::FakeCounter>(),
            lemnos_host::entry::<lemnos_driver_system_identity::Identity>(),
            lemnos_host::entry::<TrackedDevice>(),
        ],
        backend: Box::new(lemnos_sim::SimBoard::new()),
        options: lemnos_test_options::options(),
    })
    .expect("lemnosd")
}

/// The board at `level`, its node logic, lemnosd, a relay the bridge reaches Orion through, and the
/// bridge. The caller's own session goes straight to the board, never through the relay.
struct Rig {
    board: Board,
    _logic: Logic,
    _service: lemnosd::Service,
    relay: Option<Relay>,
    relay_socket: std::path::PathBuf,
    _bridge: Bridge,
    dashboard: Orion,
}

fn start(dir: &std::path::Path, level: u16) -> Rig {
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_with(
        dir,
        orion_stubs::Options {
            level,
            ..board_options()
        },
    );
    let logic = Logic::start(dir);
    let service = start_lemnosd_tracked(&lemnosd);
    let relay_socket = dir.join("relay.sock");
    let relay = Relay::start(&relay_socket, board.socket().to_path_buf());
    let bridge = start_bridge(&lemnosd, &relay_socket);
    let dashboard = dashboard(board.socket());
    wait_for("the tracked device's resource", || {
        resource(&dashboard, "lemnos.tracked").filter(|found| found.state == ResourceState::Ready)
    });
    Rig {
        board,
        _logic: logic,
        _service: service,
        relay: Some(relay),
        relay_socket,
        _bridge: bridge,
        dashboard,
    }
}

impl Rig {
    /// Drops the relay: the bridge loses its session and cannot connect again until `restart`.
    fn drop_link(&mut self) {
        self.relay = None;
    }

    /// The relay comes back: the bridge's next connection gets through.
    fn restart(&mut self) {
        // A new relay counts its connections from zero: one is the bridge connecting again.
        self.relay = Some(Relay::start(
            &self.relay_socket,
            self.board.socket().to_path_buf(),
        ));
    }
}

/// A detached call on the tracked device, kept by node logic if the bridge loses its session.
fn detached(orion: &Orion, args: &str) -> orion::actions::Run {
    orion
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: args.as_bytes(),
            // A detached run needs a deadline at this revision: a minute.
            deadline_ms: 60_000,
            detached: true,
        })
        .expect("run")
}

/// Waits until `run` reports progress: the job is under way on the device.
fn until_progressing(run: &mut orion::actions::Run) {
    wait_for("the run reports progress", || {
        match run.wait(Duration::from_millis(100)) {
            Ok(Some(Update::Doing(_))) => Some(()),
            _ => None,
        }
    });
}

/// A job that finishes on its own, detached, is given back to the bridge's new session and is
/// answered as succeeded. Mutation: the bridge registering without `action_kept` ends the run as the
/// session is lost, and the caller does not see success.
#[test]
fn a_kept_run_is_given_back_after_the_bridge_loses_its_session_and_succeeds() {
    let dir = dir("kept-success");
    let mut rig = start(&dir, 6);
    let mut run = detached(&rig.dashboard, "finish_after_ticks");
    until_progressing(&mut run);

    // The link is down for a moment, well inside the job's six seconds: the bridge loses its session,
    // and connects again on the restored link.
    rig.drop_link();
    std::thread::sleep(Duration::from_millis(1500));
    rig.restart();
    wait_for("the bridge connects again", || {
        rig.relay
            .as_ref()
            .filter(|link| link.accepted() >= 1)
            .map(|_| ())
    });

    let finished = run.finish(WAIT).expect("the kept run ends");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
}

/// A job that ends while the bridge is away is answered when the bridge comes back, not lost. The
/// relay is dropped for longer than the job takes, so the bridge's answer is held and then given.
/// Mutation: without the hold the answer is dropped with the lost session and the run is not
/// succeeded.
#[test]
fn a_run_that_ends_while_the_bridge_is_away_is_answered_when_it_returns() {
    let dir = dir("kept-ended-away");
    let mut rig = start(&dir, 6);
    let mut run = detached(&rig.dashboard, "finish_after_ticks");
    until_progressing(&mut run);

    rig.drop_link();
    // The job runs for about six seconds; the bridge is away for eight.
    std::thread::sleep(Duration::from_secs(8));
    rig.restart();
    wait_for("the bridge connects again", || {
        rig.relay
            .as_ref()
            .filter(|link| link.accepted() >= 1)
            .map(|_| ())
    });

    let finished = run.finish(WAIT).expect("the kept run ends");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
}

/// A cancel the caller asks while the bridge is away is kept by node logic, and delivered when the
/// bridge's session is given the run back: the job is cancelled and the caller sees it cancelled.
/// Mutation: a bridge that ignores a kept run's cancel leaves the job running, and this times out.
#[test]
fn a_cancel_asked_while_the_bridge_is_away_is_delivered_when_it_returns() {
    let dir = dir("kept-cancel");
    let mut rig = start(&dir, 6);
    let mut run = detached(&rig.dashboard, "hold_until_cancelled");
    std::thread::sleep(Duration::from_millis(500));

    rig.drop_link();
    std::thread::sleep(Duration::from_millis(1500));
    run.cancel().expect("cancel");
    rig.restart();
    wait_for("the bridge connects again", || {
        rig.relay
            .as_ref()
            .filter(|link| link.accepted() >= 1)
            .map(|_| ())
    });

    let finished = run.finish(WAIT).expect("the cancelled run ends");
    assert_eq!(finished.outcome, Outcome::Cancelled, "{finished:?}");
}

/// A run the bridge is not back for, within the window node logic keeps it, ends indeterminate: how
/// it ended is not known, so it is never reported as failed or cancelled. The window is thirty
/// seconds, so this test waits about that long.
#[test]
fn a_run_not_given_back_within_its_window_ends_indeterminate() {
    let dir = dir("kept-expired");
    let mut rig = start(&dir, 6);
    let mut run = detached(&rig.dashboard, "hold_until_cancelled");
    std::thread::sleep(Duration::from_millis(500));

    rig.drop_link();
    let finished = run
        .finish(Duration::from_secs(45))
        .expect("the run ends once its window is spent");
    assert_eq!(finished.outcome, Outcome::Indeterminate, "{finished:?}");
}

/// A node below the level that keeps runs (5 here, from its welcome) takes no window: the bridge
/// registers its actions with `action`, which that node does not keep, so the detached run ends as
/// the session is lost instead of being held. The bridge still runs ordinary calls through
/// `register`.
#[test]
fn a_node_below_the_kept_level_registers_without_a_window_and_does_not_hold_the_run() {
    let dir = dir("kept-level5");
    let mut rig = start(&dir, 5);
    let plain = rig.dashboard.run(&ActionCall {
        name: "lemnos.tracked.call",
        target: Target::Resource("lemnos.tracked".parse().expect("name")),
        args: b"describe",
        deadline_ms: 0,
        detached: false,
    });
    let mut plain = plain.expect("run");
    let described = plain.finish(WAIT).expect("the plain call ends");
    assert_eq!(described.outcome, Outcome::Succeeded, "{described:?}");

    let mut run = detached(&rig.dashboard, "hold_until_cancelled");
    std::thread::sleep(Duration::from_millis(500));
    rig.drop_link();
    let finished = run
        .finish(Duration::from_secs(10))
        .expect("a run not held ends at once");
    assert_ne!(finished.outcome, Outcome::Succeeded, "{finished:?}");
}
