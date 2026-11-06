//! The advisory GPU lease end to end over the mock server: exclusive and
//! shared leases, the queue, a closed connection ending its leases, hangs and
//! the state, and clients that never ask (untouched).

use lemnos_ipc::{
    ClientError, DeviceClient, GpuAcquire, GpuClient, GpuLeaseState, GpuMode, Refusal,
};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::path::Path;
use std::time::Duration;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"
"#;

/// A service whose fake sysfs lists one render node of driver `v3d`.
fn start() -> MockLemnosd {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "lemnosd-gpu-lease-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let drm = root.join("sys/class/drm");
    let device = root.join("sys/devices/gpu");
    let driver = root.join("sys/bus/platform/drivers/v3d");
    for p in [drm.join("renderD128"), device.clone(), driver.clone()] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::os::unix::fs::symlink(&device, drm.join("renderD128/device")).unwrap();
    std::os::unix::fs::symlink(&driver, device.join("driver")).unwrap();
    MockLemnosd::start_in(root, BOARD, MockHardware::new()).unwrap()
}

fn ask(mode: GpuMode, priority: u8) -> GpuAcquire {
    GpuAcquire {
        mode,
        priority,
        label: "test".into(),
        ..GpuAcquire::default()
    }
}

fn client(socket: &Path, name: &str) -> GpuClient {
    let c = GpuClient::connect(socket, name).unwrap();
    assert!(c.supported());
    c
}

#[test]
fn an_exclusive_lease_queues_the_next_and_ends_with_its_connection() {
    let service = start();
    let mut a = client(service.socket(), "a");
    let lease = a.acquire(ask(GpuMode::Exclusive, 50), None).unwrap();
    let mut b = client(service.socket(), "b");
    assert!(matches!(
        b.acquire(ask(GpuMode::Shared, 50), None),
        Err(ClientError::Refused(Refusal::Claimed))
    ));
    let state = a.status().unwrap();
    assert_eq!(state.len(), 1);
    assert_eq!(
        (state[0].name.as_str(), state[0].node.as_str()),
        ("v3d", "/dev/dri/renderD128")
    );
    assert_eq!(state[0].holders[0].lease, lease);
    assert_eq!(state[0].holders[0].client, "a");

    // b waits; a's connection closing hands the GPU over.
    let socket = service.socket().to_path_buf();
    let waiter = std::thread::spawn(move || {
        let mut b = client(&socket, "b2");
        let got = b.acquire(ask(GpuMode::Exclusive, 50), Some(Duration::from_secs(10)));
        (b, got)
    });
    // Until b2 shows as a waiter.
    for _ in 0..200 {
        if !a.status().unwrap()[0].waiters.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(a.status().unwrap()[0].waiters[0].client, "b2");
    drop(a);
    let (mut b2, got) = waiter.join().unwrap();
    let held = got.unwrap();
    assert_eq!(b2.status().unwrap()[0].holders[0].lease, held);
    b2.release(held).unwrap();
    assert!(matches!(
        b2.release(held),
        Err(ClientError::Refused(Refusal::UnknownHandle))
    ));
    assert!(b2.status().unwrap()[0].holders.is_empty());
    service.stop();
}

#[test]
fn shared_leases_coexist_and_a_wait_that_times_out_gives_its_place_up() {
    let service = start();
    let mut a = client(service.socket(), "a");
    let mut b = client(service.socket(), "b");
    a.acquire(ask(GpuMode::Shared, 50), None).unwrap();
    b.acquire(ask(GpuMode::Shared, 50), None).unwrap();
    let mut c = client(service.socket(), "c");
    assert!(matches!(
        c.acquire(
            ask(GpuMode::Exclusive, 50),
            Some(Duration::from_millis(200))
        ),
        Err(ClientError::Timeout)
    ));
    let state = c.status().unwrap();
    assert_eq!(state[0].holders.len(), 2);
    assert!(state[0].waiters.is_empty());
    service.stop();
}

#[test]
fn a_holder_is_asked_to_yield_and_told_when_overdue() {
    let service = start();
    let mut low = client(service.socket(), "low");
    let mut a = ask(GpuMode::Exclusive, 10);
    a.max_hold_ms = 150;
    let lease = low.acquire(a, None).unwrap();
    let socket = service.socket().to_path_buf();
    let high = std::thread::spawn(move || {
        let mut high = client(&socket, "high");
        let got = high.acquire(ask(GpuMode::Exclusive, 90), Some(Duration::from_secs(10)));
        (high, got)
    });
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let notice = low
            .next_notice(Duration::from_secs(5))
            .unwrap()
            .expect("a notice in 5 s");
        assert_eq!(notice.lease, lease);
        seen.push(notice.state);
    }
    seen.sort_by_key(|s| matches!(s, GpuLeaseState::Overdue));
    assert_eq!(seen, [GpuLeaseState::YieldAsked, GpuLeaseState::Overdue]);
    assert!(low.status().unwrap()[0].holders[0].overdue);
    low.release(lease).unwrap();
    let (_high, got) = high.join().unwrap();
    got.unwrap();
    service.stop();
}

#[test]
fn hangs_are_reported_in_the_state() {
    let service = start();
    let mut a = client(service.socket(), "a");
    let lease = a.acquire(ask(GpuMode::Shared, 50), None).unwrap();
    a.report_hang(lease, "fence never signalled").unwrap();
    assert!(matches!(
        a.report_hang(lease + 100, "x"),
        Err(ClientError::Refused(Refusal::UnknownHandle))
    ));
    let state = a.status().unwrap();
    assert_eq!(state[0].hangs, 1);
    let last = state[0].last_hang.as_ref().unwrap();
    assert_eq!(
        (
            last.client.as_str(),
            last.label.as_str(),
            last.detail.as_str()
        ),
        ("a", "test", "fence never signalled")
    );
    let mut named = ask(GpuMode::Shared, 50);
    named.gpu = "nope".into();
    assert!(matches!(
        a.acquire(named, None),
        Err(ClientError::Refused(Refusal::UnknownDevice))
    ));
    service.stop();
}

#[test]
fn a_client_that_never_asks_is_untouched_by_a_held_lease() {
    let service = start();
    let mut a = client(service.socket(), "a");
    a.acquire(ask(GpuMode::Exclusive, 50), Some(Duration::from_secs(1)))
        .unwrap();
    let mut d = DeviceClient::connect(service.socket(), "plain").unwrap();
    assert!(d.list().unwrap().is_empty());
    assert!(
        d.next_event_timeout(Duration::from_millis(100))
            .unwrap()
            .is_some_and(|e| matches!(e, lemnos_ipc::ClientEvent::Connected { .. }))
    );
    assert!(
        d.next_event_timeout(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
    service.stop();
}
