//! A driver that panics, hangs or stops answering takes nothing else with it.

mod common;

use std::time::Duration;

use common::{COUNTERS, get_counter, host, host_with, method, quick};
use lemnos_core::{Health, Status};
use lemnos_host::{DeviceState, OffReason, Options};

fn index(host: &lemnos_host::Host, id: &str) -> u32 {
    host.device(id).unwrap().index
}

#[test]
fn a_panic_is_contained_and_retried() {
    let (host, clock) = host(COUNTERS);
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));

    assert_eq!(
        host.call(bad, method(3), &[], &mut [0; 8]),
        Err(Status::PANIC)
    );
    assert_eq!(get_counter(&host, good), Ok(1));
    assert!(matches!(
        host.device("bad").unwrap().state,
        DeviceState::Backoff { attempts: 1, .. }
    ));
    assert_eq!(get_counter(&host, bad), Err(Status::NO_DEVICE));

    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(get_counter(&host, bad), Ok(1));
}

#[test]
fn a_hang_returns_on_time_and_leaves_neighbours_running() {
    let (host, _) = host_with(COUNTERS, quick());
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));

    let args = 400u32.to_le_bytes();
    assert_eq!(
        host.call(bad, method(4), &args, &mut [0; 8]),
        Err(Status::TIMED_OUT)
    );
    assert_eq!(get_counter(&host, good), Ok(1));
    assert!(matches!(
        host.device("bad").unwrap().health,
        Health::Failed(_)
    ));
    assert_eq!(get_counter(&host, bad), Err(Status::NO_DEVICE));
}

#[test]
fn a_hung_device_comes_back_once_the_stuck_call_returns() {
    let (host, clock) = host_with(COUNTERS, quick());
    let bad = index(&host, "bad");
    let args = 200u32.to_le_bytes();
    assert_eq!(
        host.call(bad, method(4), &args, &mut [0; 8]),
        Err(Status::TIMED_OUT)
    );

    std::thread::sleep(Duration::from_millis(250));
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(get_counter(&host, bad), Ok(1));
}

#[test]
fn neighbours_on_one_bus_survive_a_hang_through_a_new_lane() {
    let text = r#"
    [[devices]]
    id = "bad"
    driver = "fake-counter"
    bus = "shared"
    [[devices]]
    id = "good"
    driver = "fake-counter"
    bus = "shared"
    "#;
    let (host, _) = host_with(text, quick());
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));
    let args = 400u32.to_le_bytes();
    assert_eq!(
        host.call(bad, method(4), &args, &mut [0; 8]),
        Err(Status::TIMED_OUT)
    );
    assert_eq!(get_counter(&host, good), Ok(1));
}

#[test]
fn retries_double_to_eight_seconds_and_never_stop() {
    let text =
        "[[devices]]\nid = \"a\"\ndriver = \"fake-accelerometer\"\nconfig = { refuse = true }\n";
    let (host, clock) = host(text);
    let mut waits = Vec::new();
    for _ in 0..8 {
        let next = host.next_retry_ns().unwrap();
        waits.push(Duration::from_nanos(next - clock.now_ns()).as_secs());
        clock.advance(Duration::from_nanos(next - clock.now_ns()));
        host.tick();
    }
    assert_eq!(waits, [1, 2, 4, 8, 8, 8, 8, 8]);
    assert!(matches!(
        host.device("a").unwrap().state,
        DeviceState::Backoff { .. }
    ));
}

#[test]
fn nothing_is_retried_before_it_is_due() {
    let text =
        "[[devices]]\nid = \"a\"\ndriver = \"fake-accelerometer\"\nconfig = { refuse = true }\n";
    let (host, clock) = host(text);
    clock.advance(Duration::from_millis(999));
    host.tick();
    assert!(matches!(
        host.device("a").unwrap().state,
        DeviceState::Backoff { attempts: 1, .. }
    ));
}

#[test]
fn repeated_errors_rebind_the_device() {
    let (host, clock) = host(COUNTERS);
    let bad = index(&host, "bad");
    for _ in 0..5 {
        assert_eq!(host.call(bad, method(5), &[], &mut [0; 8]), Err(Status::IO));
    }
    assert!(matches!(
        host.device("bad").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
}

#[test]
fn a_success_resets_the_error_count() {
    let (host, _) = host(COUNTERS);
    let bad = index(&host, "bad");
    for _ in 0..12 {
        let _ = host.call(bad, method(5), &[], &mut [0; 8]);
        if host.device("bad").unwrap().state != DeviceState::Running {
            panic!("rebound too early");
        }
        assert!(get_counter(&host, bad).is_ok());
    }
}

#[test]
fn off_is_manual_visible_and_reversible() {
    let (host, clock) = host(COUNTERS);
    let bad = index(&host, "bad");
    host.off("bad").unwrap();
    assert_eq!(
        host.device("bad").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(get_counter(&host, bad), Err(Status::NO_DEVICE));

    clock.advance(Duration::from_secs(600));
    host.tick();
    assert_eq!(
        host.device("bad").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );

    host.on("bad").unwrap();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(get_counter(&host, bad), Ok(1));
}

#[test]
fn disabled_in_the_board_file_stays_off_until_switched_on() {
    let text = "[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\ndisabled = true\n";
    let (host, _) = host(text);
    assert_eq!(
        host.device("a").unwrap().state,
        DeviceState::Off(OffReason::Disabled)
    );
    host.on("a").unwrap();
    assert_eq!(host.device("a").unwrap().state, DeviceState::Running);
}

#[test]
fn auto_off_happens_only_to_a_device_that_opts_in() {
    let text = "[[devices]]\nid = \"a\"\ndriver = \"fake-accelerometer\"\nconfig = { refuse = true }\n\
        auto_off_after = 3\n\
        [[devices]]\nid = \"b\"\ndriver = \"fake-accelerometer\"\nconfig = { refuse = true }\n";
    // Host-wide settings, at their shortest, turn nobody off.
    let options = Options {
        retry_first: Duration::from_millis(1),
        retry_cap: Duration::from_millis(1),
        ..Options::default()
    };
    let (host, clock) = host_with(text, options);
    for _ in 0..3 {
        clock.advance(Duration::from_secs(8));
        host.tick();
    }
    assert_eq!(
        host.device("a").unwrap().state,
        DeviceState::Off(OffReason::Policy)
    );
    for _ in 0..20 {
        clock.advance(Duration::from_secs(8));
        host.tick();
    }
    let DeviceState::Backoff { attempts, .. } = host.device("b").unwrap().state else {
        panic!("b opted in to nothing and is retried forever");
    };
    assert!(attempts > 3, "{attempts}");
    // Turning it on again is the way back.
    host.on("a").unwrap();
    assert!(!matches!(
        host.device("a").unwrap().state,
        DeviceState::Off(_)
    ));
}

#[test]
fn the_board_files_host_settings_reach_the_host() {
    let text = "[host]\ncall_deadline_ms = 750\nretry_first_ms = 200\nretry_cap_ms = 3000\n\
        [[devices]]\nid = \"a\"\ndriver = \"fake-accelerometer\"\nconfig = { refuse = true }\n";
    let (host, clock) = host_with(text, Options::default());
    let options = host.options();
    assert_eq!(options.call_deadline, Duration::from_millis(750));
    assert_eq!(options.retry_first, Duration::from_millis(200));
    assert_eq!(options.retry_cap, Duration::from_secs(3));
    // The waits follow them: 200 ms doubling to the 3 s cap.
    let mut waits = Vec::new();
    for _ in 0..6 {
        let now = clock.now_ns();
        let due = host.next_retry_ns().unwrap();
        let wait = due.saturating_sub(now);
        waits.push(wait / 1_000_000);
        clock.advance(Duration::from_nanos(wait));
        host.tick();
    }
    assert_eq!(waits, [200, 400, 800, 1600, 3000, 3000]);
}

#[test]
fn an_unknown_driver_stops_only_its_own_device() {
    let text = "[[devices]]\nid = \"a\"\ndriver = \"nonexistent\"\n[[devices]]\nid = \"b\"\ndriver = \"fake-counter\"\n";
    let (host, _) = host(text);
    let DeviceState::Backoff { reason, .. } = host.device("a").unwrap().state else {
        panic!("should be retrying");
    };
    assert!(reason.contains("nonexistent"), "{reason}");
    assert_eq!(host.device("b").unwrap().state, DeviceState::Running);
}

/// A loop of `uses` refuses the device that closes it, with the loop named. The rest of the board
/// starts: the other device of the loop is not refused, and it binds.
#[test]
fn devices_in_a_circle_are_refused_at_the_device_that_closes_it() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"a\"\ndriver = \"x\"\nuses = { n = \"b\" }\n\
        [[devices]]\nid = \"b\"\ndriver = \"x\"\nuses = { n = \"a\" }\n\
        [[devices]]\nid = \"c\"\ndriver = \"fake-counter\"\n";
    // The strict reader refuses the board by the loop; the lenient one keeps it.
    let error = lemnos_host::board::parse(text).unwrap_err().to_string();
    assert!(
        error.contains("\"a\"") && error.contains("lead back to itself"),
        "{error}"
    );
    let board = lemnos_host::board::parse_lenient(text).unwrap();
    let sim = lemnos_sim::SimBoard::new();
    let clock = sim.clock();
    let host = lemnos_host::Host::start(
        &board,
        &common::drivers(),
        Box::new(sim),
        std::sync::Arc::new(clock),
        Options::default(),
    )
    .expect("the board starts");
    let DeviceState::Backoff { reason, .. } = host.device("a").unwrap().state else {
        panic!("a closes the loop and should be refused");
    };
    assert!(
        reason.contains("lead back to itself through device \"b\""),
        "{reason}"
    );
    assert_eq!(host.device("c").unwrap().state, DeviceState::Running);
}

#[test]
fn out_buffers_and_replies_reach_the_caller() {
    let (host, _) = host(COUNTERS);
    let good = index(&host, "good");
    let args = 0x0102_0304u32.to_le_bytes();
    host.call(good, method(2), &args, &mut []).unwrap();
    let capacity = 8u32.to_le_bytes();
    let mut into = [0; 8];
    assert_eq!(host.call(good, method(6), &capacity, &mut into), Ok(4));
    assert_eq!(into[..4], args);
}
