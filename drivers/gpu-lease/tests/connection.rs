//! A lease ends with its connection: a real service, through its socket, by the client.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_gpu_lease::GpuLease;
use lemnos_driver_gpu_lease::GpuLeaseDevice;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                     [[devices]]\nid = \"gpu\"\ndriver = \"gpu-lease\"\n";

#[test]
fn a_lease_ends_with_its_connection_and_the_next_waiter_is_granted() {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gpu-lease-connection-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let _service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(BOARD).unwrap(),
        drivers: vec![entry::<GpuLeaseDevice>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();

    let first = Client::connect(&socket, "first").unwrap();
    let mut holder = first.open::<dyn GpuLease>("gpu").unwrap();
    let held = holder.acquire(false, 50, false, 0).unwrap();
    assert!(held.held);

    let second = Client::connect(&socket, "second").unwrap();
    let mut waiter = second.open::<dyn GpuLease>("gpu").unwrap();
    let waiting = waiter.acquire(false, 50, true, 0).unwrap();
    assert!(!waiting.held);
    assert!(
        waiter.release(held.lease).is_err(),
        "another caller's lease isn't reachable"
    );

    // Closing without releasing is what a crash looks like to the service.
    drop(holder);
    drop(first);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !waiter.poll(waiting.lease).unwrap().held {
        assert!(Instant::now() < deadline, "the lease was never given up");
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(_service);
    std::fs::remove_dir_all(&dir).unwrap();
}
