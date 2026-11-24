//! A lease ends with its connection: a real service, through its socket, by the client.

use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_gpu_lease::GpuLease;
use lemnos_core::Status;
use lemnos_driver_gpu_lease::GpuLeaseDevice;
use lemnos_host::board::parse;
use lemnos_host::entry;
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                     [[devices]]\nid = \"gpu\"\ndriver = \"gpu-lease\"\n";

#[test]
fn a_lease_ends_with_its_connection_and_the_next_waiter_is_granted() {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let socket = lemnos_socket_dir::socket(&dir, "sock").unwrap();
    let _service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(BOARD).unwrap(),
        drivers: vec![entry::<GpuLeaseDevice>()],
        backend: Box::new(SimBoard::new()),
        options: lemnos_test_options::options(),
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
    loop {
        match waiter.poll(waiting.lease) {
            Ok(reply) if reply.held => break,
            // No answer in time under load: ask again until the deadline.
            Ok(_) => {}
            Err(status) if status == Status::TIMED_OUT => {}
            Err(status) => panic!("poll: {status:?}"),
        }
        assert!(Instant::now() < deadline, "the lease was never given up");
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(_service);
    std::fs::remove_dir_all(&dir).unwrap();
}
